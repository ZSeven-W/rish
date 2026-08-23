//! Instruction fetch and the decoded-instruction cache.
//!
//! Decoding dominates interpreter cost, so a decoded instruction is kept and
//! reused when the guest executes the same address again. A hit re-checks the
//! physical address and the containing page's write counter, so self-modifying
//! code, page remapping, and code patched through an alias all invalidate
//! themselves without an explicit flush — and a hit costs a single counter
//! compare instead of re-reading and comparing the instruction bytes.

use iced_x86::{Decoder, DecoderOptions, Instruction};

pub const MAX_INSTRUCTION_BYTES: usize = 15;

/// Direct-mapped entry count. Must be a power of two.
const ENTRIES: usize = 1 << 15;

const INVALID: u64 = u64::MAX;

#[derive(Clone, Copy)]
struct Entry {
    /// Linear instruction pointer, or `INVALID` for an empty slot.
    tag: u64,
    /// Physical address the bytes were decoded from.
    physical: u64,
    /// The containing page's write counter at decode time.
    generation: u32,
    bytes: [u8; MAX_INSTRUCTION_BYTES],
    instruction: Instruction,
}

pub struct DecodeCache {
    entries: Box<[Entry]>,
    hits: u64,
    misses: u64,
}

impl DecodeCache {
    #[must_use]
    pub fn new() -> Self {
        let empty = Entry {
            tag: INVALID,
            physical: 0,
            generation: 0,
            bytes: [0; MAX_INSTRUCTION_BYTES],
            instruction: Instruction::default(),
        };
        Self {
            entries: vec![empty; ENTRIES].into_boxed_slice(),
            hits: 0,
            misses: 0,
        }
    }

    #[inline]
    fn slot(ip: u64) -> usize {
        let mixed = ip.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        ((mixed >> 33) as usize) & (ENTRIES - 1)
    }

    /// Returns the cached instruction and its bytes when the entry still maps
    /// the same physical address and its page has not been written since it was
    /// decoded. Returning the cached bytes lets the caller skip re-reading guest
    /// memory on a hit; they are only needed for tracing and error reporting.
    #[inline]
    pub fn lookup(
        &mut self,
        ip: u64,
        physical: u64,
        generation: u32,
    ) -> Option<(Instruction, [u8; MAX_INSTRUCTION_BYTES])> {
        let entry = &self.entries[Self::slot(ip)];
        if entry.tag != ip || entry.physical != physical || entry.generation != generation {
            self.misses = self.misses.wrapping_add(1);
            return None;
        }
        self.hits = self.hits.wrapping_add(1);
        Some((entry.instruction, entry.bytes))
    }

    #[inline]
    pub fn insert(
        &mut self,
        ip: u64,
        physical: u64,
        generation: u32,
        bytes: &[u8],
        instruction: Instruction,
    ) {
        let length = instruction.len();
        if length > MAX_INSTRUCTION_BYTES || length > bytes.len() {
            return;
        }
        let mut stored = [0_u8; MAX_INSTRUCTION_BYTES];
        stored[..length].copy_from_slice(&bytes[..length]);
        self.entries[Self::slot(ip)] = Entry {
            tag: ip,
            physical,
            generation,
            bytes: stored,
            instruction,
        };
    }

    /// Hit and miss counters, for boot diagnostics.
    #[must_use]
    pub fn counters(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Decodes one instruction from a byte window.
#[inline]
pub fn decode_window(bitness: u32, window: &[u8], ip: u64) -> Instruction {
    let mut decoder = Decoder::with_ip(bitness, window, ip, DecoderOptions::NONE);
    decoder.decode()
}

#[cfg(test)]
mod tests {
    use super::*;

    // xor eax, eax
    const XOR: [u8; 2] = [0x31, 0xC0];
    // inc eax
    const INC: [u8; 2] = [0xFF, 0xC0];

    #[test]
    fn reuses_a_decode_for_the_same_address_and_generation() {
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        let decoded = decode_window(64, &window, 0x1000);
        cache.insert(0x1000, 0x5000, 7, &window, decoded);
        let (found, bytes) = cache.lookup(0x1000, 0x5000, 7).expect("hit");
        assert_eq!(found.mnemonic(), iced_x86::Mnemonic::Xor);
        assert_eq!(&bytes[..2], &XOR);
    }

    #[test]
    fn a_bumped_page_generation_misses_at_the_same_address() {
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        let decoded = decode_window(64, &window, 0x1000);
        cache.insert(0x1000, 0x5000, 7, &window, decoded);
        // A write to the page bumps its counter; the stale entry must miss so
        // self-modified or patched code is re-decoded.
        assert!(cache.lookup(0x1000, 0x5000, 8).is_none());
        let _ = INC;
    }

    #[test]
    fn the_same_address_at_a_new_physical_page_misses() {
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        let decoded = decode_window(64, &window, 0x1000);
        cache.insert(0x1000, 0x5000, 0, &window, decoded);
        assert!(cache.lookup(0x1000, 0x9000, 0).is_none());
    }

    #[test]
    fn rip_relative_operands_stay_bound_to_their_address() {
        // lea rax, [rip+0x10]
        let window = [0x48, 0x8D, 0x05, 0x10, 0x00, 0x00, 0x00];
        let first = decode_window(64, &window, 0x1000);
        let second = decode_window(64, &window, 0x2000);
        assert_ne!(
            first.ip_rel_memory_address(),
            second.ip_rel_memory_address()
        );
        let mut cache = DecodeCache::new();
        cache.insert(0x1000, 0x5000, 0, &window, first);
        // A different instruction pointer never reads the 0x1000 entry.
        assert!(cache.lookup(0x2000, 0x5000, 0).is_none());
    }
}
