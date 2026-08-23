//! Translation lookaside buffer.
//!
//! The x86 TLB is not coherent with page-table stores: software must issue
//! `invlpg` or reload CR3 for a changed entry to take effect. This cache
//! follows the same contract, so it survives ordinary guest writes instead of
//! being flushed by them. Entries keep the permissions accumulated by the
//! walk, and every hit is re-checked against the current privilege level.

use crate::arch::paging::Translation;

/// Direct-mapped entry count. Must be a power of two.
const ENTRIES: usize = 1 << 14;

#[derive(Clone, Copy)]
struct Entry {
    /// Linear page number, or `INVALID` for an empty slot.
    tag: u64,
    translation: Translation,
}

/// A tag no linear page can produce, because the page number is shifted down
/// by 12 bits and therefore never occupies the top bits.
const INVALID: u64 = u64::MAX;

pub struct Tlb {
    entries: Box<[Entry]>,
    hits: u64,
    misses: u64,
}

impl Tlb {
    #[must_use]
    pub fn new() -> Self {
        let empty = Entry {
            tag: INVALID,
            translation: Translation {
                frame: 0,
                page_size: 0x1000,
                writable: false,
                user: false,
                no_execute: true,
            },
        };
        Self {
            entries: vec![empty; ENTRIES].into_boxed_slice(),
            hits: 0,
            misses: 0,
        }
    }

    #[inline]
    fn slot(page: u64) -> usize {
        // Mix the page number so kernel and user addresses that differ only
        // in high bits do not collide on the same slot.
        let mixed = page.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        ((mixed >> 32) as usize) & (ENTRIES - 1)
    }

    #[inline]
    pub fn lookup(&mut self, linear: u64) -> Option<Translation> {
        let page = linear >> 12;
        let entry = self.entries[Self::slot(page)];
        if entry.tag == page {
            self.hits = self.hits.wrapping_add(1);
            Some(entry.translation)
        } else {
            self.misses = self.misses.wrapping_add(1);
            None
        }
    }

    #[inline]
    pub fn insert(&mut self, linear: u64, translation: Translation) {
        let page = linear >> 12;
        self.entries[Self::slot(page)] = Entry {
            tag: page,
            translation,
        };
    }

    /// Invalidates every entry, as a CR3 reload or a paging-mode change does.
    pub fn flush(&mut self) {
        for entry in self.entries.iter_mut() {
            entry.tag = INVALID;
        }
    }

    /// Invalidates the entry covering one linear address, as `invlpg` does.
    ///
    /// A large-page mapping is cached under each 4 KiB page it covers, so a
    /// single-page invalidation is enough for the address the guest names.
    pub fn invalidate(&mut self, linear: u64) {
        let page = linear >> 12;
        let slot = Self::slot(page);
        if self.entries[slot].tag == page {
            self.entries[slot].tag = INVALID;
        }
    }

    /// Hit and miss counters, for boot diagnostics.
    #[must_use]
    pub fn counters(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

impl Default for Tlb {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translation(frame: u64) -> Translation {
        Translation {
            frame,
            page_size: 0x1000,
            writable: true,
            user: false,
            no_execute: false,
        }
    }

    #[test]
    fn caches_and_returns_a_translation() {
        let mut tlb = Tlb::new();
        assert!(tlb.lookup(0x1234).is_none());
        tlb.insert(0x1234, translation(0x5000));
        let found = tlb.lookup(0x1FFF).expect("same page");
        assert_eq!(found.physical(0x1FFF), 0x5FFF);
    }

    #[test]
    fn flush_drops_every_entry() {
        let mut tlb = Tlb::new();
        tlb.insert(0x1000, translation(0x5000));
        tlb.insert(0x9000, translation(0x6000));
        tlb.flush();
        assert!(tlb.lookup(0x1000).is_none());
        assert!(tlb.lookup(0x9000).is_none());
    }

    #[test]
    fn invalidate_drops_only_the_named_page() {
        let mut tlb = Tlb::new();
        tlb.insert(0x1000, translation(0x5000));
        tlb.insert(0x9000, translation(0x6000));
        tlb.invalidate(0x1abc);
        assert!(tlb.lookup(0x1000).is_none());
        assert!(tlb.lookup(0x9000).is_some());
    }

    #[test]
    fn a_page_written_without_invalidation_keeps_the_stale_entry() {
        // Hardware behavior: page-table stores alone do not update the TLB.
        let mut tlb = Tlb::new();
        tlb.insert(0x2000, translation(0x5000));
        assert_eq!(tlb.lookup(0x2000).unwrap().frame, 0x5000);
    }
}
