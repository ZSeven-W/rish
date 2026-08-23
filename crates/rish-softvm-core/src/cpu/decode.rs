//! Instruction fetch and the decoded-instruction cache.
//!
//! Decoding dominates interpreter cost, so a decoded instruction is kept and
//! reused when the guest executes the same address again. A mapped hit checks
//! the TLB invalidation epoch, execution-permission context, and containing
//! physical page's write counter before skipping translation and guest-memory
//! reads. Thus `invlpg`, CR3 changes, permission-mode changes, self-modifying
//! code, and code patched through an alias still invalidate the fast path.

use iced_x86::{Decoder, DecoderOptions, Instruction};

use crate::Memory;

pub const MAX_INSTRUCTION_BYTES: usize = 15;

/// Direct-mapped entry count. Must be a power of two.
const ENTRIES: usize = 1 << 15;

const INVALID: u64 = u64::MAX;

#[derive(Clone, Copy)]
struct Entry {
    /// Linear instruction pointer, or `INVALID` for an empty slot.
    tag: u64,
    /// Linear fetch address after applying the current code-segment base.
    linear: u64,
    /// Physical address the bytes were decoded from.
    physical: u64,
    /// The containing page's write counter at decode time.
    generation: u32,
    /// Explicit TLB invalidation generation at the last validated mapping.
    translation_epoch: u64,
    /// Bitness/CPL and execute-permission control bits at validation time.
    execution_context: u32,
    instruction: Instruction,
}

/// One fully validated instruction-fetch mapping.
#[derive(Clone, Copy)]
pub struct DecodeMapping {
    pub ip: u64,
    pub linear: u64,
    pub physical: u64,
    pub page_generation: u32,
    pub translation_epoch: u64,
    pub execution_context: u32,
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
            linear: 0,
            physical: 0,
            generation: 0,
            translation_epoch: 0,
            execution_context: 0,
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

    /// Returns an instruction without repeating translation when every fact
    /// that can invalidate its already-approved execute mapping is unchanged.
    #[inline]
    pub fn lookup_mapped(
        &mut self,
        memory: &Memory,
        ip: u64,
        linear: u64,
        translation_epoch: u64,
        execution_context: u32,
    ) -> Option<Instruction> {
        let entry = &self.entries[Self::slot(ip)];
        if entry.tag != ip
            || entry.linear != linear
            || entry.translation_epoch != translation_epoch
            || entry.execution_context != execution_context
            || memory.page_generation(entry.physical) != entry.generation
        {
            return None;
        }
        self.hits = self.hits.wrapping_add(1);
        Some(entry.instruction)
    }

    /// Returns a cached decode after the caller has translated the address.
    /// A successful fallback refreshes the fast-path mapping context.
    #[inline]
    pub fn lookup(&mut self, mapping: DecodeMapping) -> Option<Instruction> {
        let entry = &mut self.entries[Self::slot(mapping.ip)];
        if entry.tag != mapping.ip
            || entry.linear != mapping.linear
            || entry.physical != mapping.physical
            || entry.generation != mapping.page_generation
            || entry.execution_context != mapping.execution_context
        {
            self.misses = self.misses.wrapping_add(1);
            return None;
        }
        entry.translation_epoch = mapping.translation_epoch;
        entry.execution_context = mapping.execution_context;
        self.hits = self.hits.wrapping_add(1);
        Some(entry.instruction)
    }

    #[inline]
    pub fn insert(&mut self, mapping: DecodeMapping, instruction: Instruction) {
        if instruction.len() > MAX_INSTRUCTION_BYTES {
            return;
        }
        self.entries[Self::slot(mapping.ip)] = Entry {
            tag: mapping.ip,
            linear: mapping.linear,
            physical: mapping.physical,
            generation: mapping.page_generation,
            translation_epoch: mapping.translation_epoch,
            execution_context: mapping.execution_context,
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

    fn mapping(ip: u64, linear: u64, physical: u64, page_generation: u32) -> DecodeMapping {
        DecodeMapping {
            ip,
            linear,
            physical,
            page_generation,
            translation_epoch: 3,
            execution_context: 64,
        }
    }

    #[test]
    fn reuses_a_decode_for_the_same_address_and_generation() {
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        let decoded = decode_window(64, &window, 0x1000);
        let mapping = mapping(0x1000, 0x1000, 0x5000, 7);
        cache.insert(mapping, decoded);
        let found = cache.lookup(mapping).expect("hit");
        assert_eq!(found.mnemonic(), iced_x86::Mnemonic::Xor);

        let mut changed_context = mapping;
        changed_context.execution_context = 32;
        assert!(cache.lookup(changed_context).is_none());
    }

    #[test]
    fn a_bumped_page_generation_misses_at_the_same_address() {
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        let decoded = decode_window(64, &window, 0x1000);
        cache.insert(mapping(0x1000, 0x1000, 0x5000, 7), decoded);
        // A write to the page bumps its counter; the stale entry must miss so
        // self-modified or patched code is re-decoded.
        assert!(cache.lookup(mapping(0x1000, 0x1000, 0x5000, 8)).is_none());
        let _ = INC;
    }

    #[test]
    fn the_same_address_at_a_new_physical_page_misses() {
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        let decoded = decode_window(64, &window, 0x1000);
        cache.insert(mapping(0x1000, 0x1000, 0x5000, 0), decoded);
        assert!(cache.lookup(mapping(0x1000, 0x1000, 0x9000, 0)).is_none());
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
        cache.insert(mapping(0x1000, 0x1000, 0x5000, 0), first);
        // A different instruction pointer never reads the 0x1000 entry.
        assert!(cache.lookup(mapping(0x2000, 0x2000, 0x5000, 0)).is_none());
    }

    #[test]
    fn mapped_lookup_requires_the_same_epoch_context_and_code_page() {
        let mut memory = Memory::new(1).unwrap();
        let mut cache = DecodeCache::new();
        let window = [XOR[0], XOR[1], 0x90, 0x90];
        memory.write(0x5000, &window).unwrap();
        let generation = memory.page_generation(0x5000);
        let decoded = decode_window(64, &window, 0x1000);
        cache.insert(mapping(0x1000, 0x1000, 0x5000, generation), decoded);

        assert!(
            cache
                .lookup_mapped(&memory, 0x1000, 0x1000, 3, 64)
                .is_some()
        );
        assert!(
            cache
                .lookup_mapped(&memory, 0x1000, 0x2000, 3, 64)
                .is_none()
        );
        assert!(
            cache
                .lookup_mapped(&memory, 0x1000, 0x1000, 4, 64)
                .is_none()
        );
        assert!(
            cache
                .lookup_mapped(&memory, 0x1000, 0x1000, 3, 32)
                .is_none()
        );

        memory.write_u8(0x5003, 0xCC).unwrap();
        assert!(
            cache
                .lookup_mapped(&memory, 0x1000, 0x1000, 3, 64)
                .is_none()
        );
    }
}
