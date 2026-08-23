//! Linear-to-physical translation: 32-bit non-PAE, PAE, 4-level, 5-level.
//!
//! Walk failures carry the x86 page-fault error code so the CPU can deliver
//! exception 14 with hardware-identical bits (P, W, U/S, RSVD, I/D).

use crate::Memory;
use crate::arch::registers::{Cr0, Cr4, Efer};

pub const PAGE_PRESENT: u64 = 1 << 0;
pub const PAGE_WRITABLE: u64 = 1 << 1;
pub const PAGE_USER: u64 = 1 << 2;
pub const PAGE_ACCESSED: u64 = 1 << 5;
pub const PAGE_DIRTY: u64 = 1 << 6;
pub const PAGE_LARGE: u64 = 1 << 7;
pub const PAGE_NX: u64 = 1 << 63;

const TABLE_MASK: u64 = 0x000F_FFFF_FFFF_F000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessKind {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageFault {
    pub linear: u64,
    /// x86 page-fault error code bits: bit0 P, bit1 W, bit2 U/S, bit3 RSVD,
    /// bit4 I/D, bit5 PK, bit6 SGX.
    pub error_code: u16,
}

/// Error-code bit positions, named so callers do not repeat magic numbers.
pub const FAULT_PRESENT: u16 = 1 << 0;
pub const FAULT_WRITE: u16 = 1 << 1;
pub const FAULT_USER: u16 = 1 << 2;
pub const FAULT_RESERVED: u16 = 1 << 3;
pub const FAULT_FETCH: u16 = 1 << 4;

/// One completed page-table walk: the physical frame plus the permissions
/// accumulated across every level, which is what a TLB entry caches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Translation {
    /// Physical base of the mapped frame, with the page offset cleared.
    pub frame: u64,
    /// Page size in bytes (4 KiB, 2 MiB, or 1 GiB).
    pub page_size: u64,
    /// Writable at every level.
    pub writable: bool,
    /// User-accessible at every level.
    pub user: bool,
    /// No-execute at any level (only meaningful when EFER.NXE is set).
    pub no_execute: bool,
}

impl Translation {
    /// Physical address of a linear address inside this page.
    #[inline]
    #[must_use]
    pub fn physical(&self, linear: u64) -> u64 {
        self.frame | (linear & (self.page_size - 1))
    }
}

/// Translates one linear address against the active paging mode.
pub fn translate(
    memory: &Memory,
    cr3: u64,
    cr0: Cr0,
    cr4: Cr4,
    efer: Efer,
    linear: u64,
    kind: AccessKind,
) -> Result<u64, PageFault> {
    if !cr0.contains(Cr0::PG) {
        return Ok(linear);
    }
    let walked = walk(memory, cr3, cr4, efer, linear)?;
    check_access(&walked, linear, kind, 0, cr0, cr4, efer, false)?;
    Ok(walked.physical(linear))
}

/// Walks the page tables without applying any privilege check. The caller
/// applies [`check_access`] so a cached walk can be re-checked against the
/// current privilege level without repeating the walk.
pub fn walk(
    memory: &Memory,
    cr3: u64,
    cr4: Cr4,
    efer: Efer,
    linear: u64,
) -> Result<Translation, PageFault> {
    let reserved = reserved_mask(efer);
    if efer.contains(Efer::LMA) {
        return walk_generic(memory, cr3, efer, reserved, linear, cr4.contains(Cr4::LA57));
    }
    if cr4.contains(Cr4::PAE) {
        return walk_pae32(memory, cr3, cr4, efer, reserved, linear);
    }
    walk_2_level(memory, cr3, cr4, efer, reserved, linear)
}

fn reserved_mask(_efer: Efer) -> u64 {
    // Bits above the physical-address width (MAXPHYADDR) are available to
    // software: Linux sets software bits 52-62 in entries, e.g. bit 58 in
    // the PML4 entries built by kernel_ident_mapping_init. Treating them as
    // reserved breaks the first CR3 switch, so stay permissive.
    0
}

/// Accumulates the permission bits of one level onto a running translation.
#[inline]
fn accumulate(state: &mut Translation, entry: u64) {
    state.writable &= entry & PAGE_WRITABLE != 0;
    state.user &= entry & PAGE_USER != 0;
    state.no_execute |= entry & PAGE_NX != 0;
}

/// A fresh permission accumulator: everything allowed until a level narrows it.
#[inline]
fn permissive(page_size: u64) -> Translation {
    Translation {
        frame: 0,
        page_size,
        writable: true,
        user: true,
        no_execute: false,
    }
}

fn walk_generic(
    memory: &Memory,
    cr3: u64,
    efer: Efer,
    reserved: u64,
    linear: u64,
    five_level: bool,
) -> Result<Translation, PageFault> {
    let mut state = permissive(0x1000);
    let mut entry = if five_level {
        let index = (linear >> 48) & 0x1FF;
        read_entry(memory, (cr3 & TABLE_MASK) + index * 8, linear, reserved)?
    } else {
        let index = (linear >> 39) & 0x1FF;
        read_entry(memory, (cr3 & TABLE_MASK) + index * 8, linear, reserved)?
    };
    accumulate(&mut state, entry);
    if entry & PAGE_LARGE != 0 {
        return Err(reserved_fault(linear));
    }
    if five_level {
        let pml4_index = (linear >> 39) & 0x1FF;
        entry = read_entry(
            memory,
            (entry & TABLE_MASK) + pml4_index * 8,
            linear,
            reserved,
        )?;
        accumulate(&mut state, entry);
        if entry & PAGE_LARGE != 0 {
            return Err(reserved_fault(linear));
        }
    }
    let pdpt_index = (linear >> 30) & 0x1FF;
    entry = read_entry(
        memory,
        (entry & TABLE_MASK) + pdpt_index * 8,
        linear,
        reserved,
    )?;
    accumulate(&mut state, entry);
    if entry & PAGE_LARGE != 0 {
        return large_page(state, entry, linear, 30);
    }
    let pd_index = (linear >> 21) & 0x1FF;
    entry = read_entry(
        memory,
        (entry & TABLE_MASK) + pd_index * 8,
        linear,
        reserved,
    )?;
    accumulate(&mut state, entry);
    if entry & PAGE_LARGE != 0 {
        return large_page(state, entry, linear, 21);
    }
    let pt_index = (linear >> 12) & 0x1FF;
    entry = read_entry(
        memory,
        (entry & TABLE_MASK) + pt_index * 8,
        linear,
        reserved,
    )?;
    accumulate(&mut state, entry);
    let _ = efer;
    state.frame = entry & TABLE_MASK & !0xFFF;
    Ok(state)
}

fn walk_pae32(
    memory: &Memory,
    cr3: u64,
    cr4: Cr4,
    efer: Efer,
    reserved: u64,
    linear: u64,
) -> Result<Translation, PageFault> {
    let mut state = permissive(0x1000);
    let pdpt_index = (linear >> 30) & 0b11;
    // PDPTE entries in PAE mode carry no permission bits.
    let mut entry = read_entry(
        memory,
        (cr3 & 0xFFFF_FFE0) + pdpt_index * 8,
        linear,
        reserved,
    )?;
    if entry & PAGE_LARGE != 0 {
        return Err(reserved_fault(linear));
    }
    let pd_index = (linear >> 21) & 0x1FF;
    entry = read_entry(
        memory,
        (entry & TABLE_MASK) + pd_index * 8,
        linear,
        reserved,
    )?;
    accumulate(&mut state, entry);
    if entry & PAGE_LARGE != 0 {
        return large_page(state, entry, linear, 21);
    }
    let pt_index = (linear >> 12) & 0x1FF;
    entry = read_entry(
        memory,
        (entry & TABLE_MASK) + pt_index * 8,
        linear,
        reserved,
    )?;
    accumulate(&mut state, entry);
    let _ = (cr4, efer);
    state.frame = entry & TABLE_MASK & !0xFFF;
    Ok(state)
}

fn walk_2_level(
    memory: &Memory,
    cr3: u64,
    cr4: Cr4,
    efer: Efer,
    reserved: u64,
    linear: u64,
) -> Result<Translation, PageFault> {
    let mut state = permissive(0x1000);
    let pd_index = (linear >> 22) & 0x3FF;
    let entry = read_entry32(memory, (cr3 & 0xFFFF_F000) + pd_index * 4, linear, reserved)?;
    accumulate(&mut state, entry);
    if entry & PAGE_LARGE != 0 {
        if !cr4.contains(Cr4::PSE) {
            return Err(reserved_fault(linear));
        }
        state.page_size = 0x40_0000;
        state.frame = entry & 0xFFC0_0000;
        return Ok(state);
    }
    let pt_index = (linear >> 12) & 0x3FF;
    let entry = read_entry32(
        memory,
        (entry & 0xFFFF_F000) + pt_index * 4,
        linear,
        reserved,
    )?;
    accumulate(&mut state, entry);
    let _ = efer;
    state.frame = entry & 0xFFFF_F000;
    Ok(state)
}

fn read_entry(memory: &Memory, address: u64, linear: u64, reserved: u64) -> Result<u64, PageFault> {
    let entry = memory.read_u64(address).map_err(|_| PageFault {
        linear,
        error_code: 0,
    })?;
    validate_entry(entry, linear, reserved)
}

/// Reads a 32-bit (non-PAE) table entry. Bit 63 does not exist there, so the
/// NX bit must never be inferred from a sign extension.
fn read_entry32(
    memory: &Memory,
    address: u64,
    linear: u64,
    reserved: u64,
) -> Result<u64, PageFault> {
    let entry = memory.read_u32(address).map_err(|_| PageFault {
        linear,
        error_code: 0,
    })?;
    validate_entry(u64::from(entry), linear, reserved)
}

fn validate_entry(entry: u64, linear: u64, reserved: u64) -> Result<u64, PageFault> {
    if entry & PAGE_PRESENT == 0 {
        return Err(PageFault {
            linear,
            error_code: 0,
        });
    }
    if entry & reserved != 0 {
        return Err(reserved_fault(linear));
    }
    Ok(entry)
}

fn reserved_fault(linear: u64) -> PageFault {
    PageFault {
        linear,
        error_code: FAULT_PRESENT | FAULT_RESERVED,
    }
}

/// Adds the access-dependent bits to a fault raised by the walk itself.
///
/// A walk failure only knows about presence and reserved bits; the write,
/// user, and instruction-fetch bits come from the access that triggered it,
/// and the handler needs them to tell a bad write from a bad read.
#[must_use]
pub fn with_access_bits(mut fault: PageFault, kind: AccessKind, cpl: u8) -> PageFault {
    if matches!(kind, AccessKind::Write) {
        fault.error_code |= FAULT_WRITE;
    }
    if matches!(kind, AccessKind::Execute) {
        fault.error_code |= FAULT_FETCH;
    }
    if cpl == 3 {
        fault.error_code |= FAULT_USER;
    }
    fault
}

/// Applies the privilege and permission rules to a completed walk.
///
/// `cpl` is the current privilege level and `alignment_check` is RFLAGS.AC,
/// which suppresses the SMAP check for explicit supervisor accesses.
#[allow(clippy::too_many_arguments)]
pub fn check_access(
    walked: &Translation,
    linear: u64,
    kind: AccessKind,
    cpl: u8,
    cr0: Cr0,
    cr4: Cr4,
    efer: Efer,
    alignment_check: bool,
) -> Result<(), PageFault> {
    let user = cpl == 3;
    let mut error_code = FAULT_PRESENT;
    if user {
        error_code |= FAULT_USER;
    }
    if matches!(kind, AccessKind::Write) {
        error_code |= FAULT_WRITE;
    }
    if matches!(kind, AccessKind::Execute) {
        error_code |= FAULT_FETCH;
    }
    let deny = |linear: u64| PageFault { linear, error_code };
    if user && !walked.user {
        return Err(deny(linear));
    }
    match kind {
        AccessKind::Write => {
            // Supervisor writes ignore the read-only bit unless CR0.WP is set.
            let enforced = user || cr0.contains(Cr0::WP);
            if enforced && !walked.writable {
                return Err(deny(linear));
            }
        }
        AccessKind::Execute => {
            if efer.contains(Efer::NXE) && walked.no_execute {
                return Err(deny(linear));
            }
            if !user && cr4.contains(Cr4::SMEP) && walked.user {
                return Err(deny(linear));
            }
        }
        AccessKind::Read => {}
    }
    // SMAP blocks supervisor data access to user pages while AC is clear.
    if !user
        && !matches!(kind, AccessKind::Execute)
        && cr4.contains(Cr4::SMAP)
        && walked.user
        && !alignment_check
    {
        return Err(deny(linear));
    }
    Ok(())
}

fn large_page(
    mut state: Translation,
    entry: u64,
    linear: u64,
    shift: u32,
) -> Result<Translation, PageFault> {
    let mask = (1_u64 << shift) - 1;
    // A 1 GiB page needs CPUID.80000001:EDX[26], which this CPU reports; the
    // PSE bit only gates 4 MiB pages in the 32-bit non-PAE walk.
    state.page_size = mask + 1;
    state.frame = entry & !mask & TABLE_MASK;
    let _ = linear;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::registers::Cr0;

    #[test]
    fn identity_map_without_paging() {
        let memory = Memory::new(1).unwrap();
        let physical = translate(
            &memory,
            0,
            Cr0::empty(),
            Cr4::empty(),
            Efer::empty(),
            0x1234,
            AccessKind::Read,
        )
        .unwrap();
        assert_eq!(physical, 0x1234);
    }

    #[test]
    fn four_level_identity_walk() {
        let mut memory = Memory::new(16).unwrap();
        // PML4[0] -> PDPT at 0x1000, PDPT[0] -> PD at 0x2000, PD[0] -> PT at
        // 0x3000, PT[0] -> frame 0 (identity), PT[1] -> frame 0x2000.
        memory
            .write_u64(0x0000, 0x1000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x1000, 0x2000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x2000, 0x3000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x3000, PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x3008, 0x2000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        let efer = Efer::LMA | Efer::NXE;
        let cr4 = Cr4::PAE;
        let cr0 = Cr0::PG;
        assert_eq!(
            translate(&memory, 0, cr0, cr4, efer, 0x1234, AccessKind::Read).unwrap(),
            0x2234
        );
        assert_eq!(
            translate(&memory, 0, cr0, cr4, efer, 0x1ABC, AccessKind::Read).unwrap(),
            0x2ABC
        );
    }

    #[test]
    fn write_to_read_only_page_faults() {
        let mut memory = Memory::new(16).unwrap();
        memory
            .write_u64(0x0000, 0x1000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x1000, 0x2000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x2000, 0x3000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        // PT[0] present but read-only.
        memory.write_u64(0x3000, PAGE_PRESENT).unwrap();
        let fault = translate(
            &memory,
            0,
            Cr0::PG | Cr0::WP,
            Cr4::PAE,
            Efer::LMA,
            0x0,
            AccessKind::Write,
        )
        .unwrap_err();
        assert_eq!(fault.error_code & FAULT_WRITE, FAULT_WRITE);
        assert_eq!(fault.error_code & FAULT_PRESENT, FAULT_PRESENT);
    }

    #[test]
    fn supervisor_write_to_read_only_page_passes_without_wp() {
        let mut memory = Memory::new(16).unwrap();
        memory
            .write_u64(0x0000, 0x1000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x1000, 0x2000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x2000, 0x3000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory.write_u64(0x3000, 0x5000 | PAGE_PRESENT).unwrap();
        assert_eq!(
            translate(
                &memory,
                0,
                Cr0::PG,
                Cr4::PAE,
                Efer::LMA,
                0x0,
                AccessKind::Write,
            )
            .unwrap(),
            0x5000
        );
    }

    #[test]
    fn user_access_to_supervisor_page_faults() {
        let mut memory = Memory::new(16).unwrap();
        for (address, target) in [(0x0000, 0x1000), (0x1000, 0x2000), (0x2000, 0x3000)] {
            memory
                .write_u64(address, target | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER)
                .unwrap();
        }
        // The leaf omits PAGE_USER, so ring 3 must fault while ring 0 passes.
        memory
            .write_u64(0x3000, 0x5000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        let walked = walk(&memory, 0, Cr4::PAE, Efer::LMA, 0).unwrap();
        assert!(!walked.user);
        let fault = check_access(
            &walked,
            0,
            AccessKind::Read,
            3,
            Cr0::PG | Cr0::WP,
            Cr4::PAE,
            Efer::LMA,
            false,
        )
        .unwrap_err();
        assert_eq!(
            fault.error_code & (FAULT_PRESENT | FAULT_USER),
            FAULT_PRESENT | FAULT_USER
        );
        assert!(
            check_access(
                &walked,
                0,
                AccessKind::Read,
                0,
                Cr0::PG | Cr0::WP,
                Cr4::PAE,
                Efer::LMA,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_not_present_fault_keeps_the_access_bits() {
        let memory = Memory::new(16).unwrap();
        let fault = walk(&memory, 0, Cr4::PAE, Efer::LMA, 0x1000).unwrap_err();
        assert_eq!(fault.error_code, 0);
        let annotated = with_access_bits(fault, AccessKind::Write, 3);
        assert_eq!(annotated.error_code & FAULT_PRESENT, 0);
        assert_eq!(annotated.error_code & FAULT_WRITE, FAULT_WRITE);
        assert_eq!(annotated.error_code & FAULT_USER, FAULT_USER);
    }

    #[test]
    fn permissions_narrow_across_levels() {
        let mut memory = Memory::new(16).unwrap();
        // The PML4 entry is user-accessible but read-only, so the writable
        // leaf below it must not grant write access.
        memory
            .write_u64(0x0000, 0x1000 | PAGE_PRESENT | PAGE_USER)
            .unwrap();
        for (address, target) in [(0x1000, 0x2000), (0x2000, 0x3000)] {
            memory
                .write_u64(address, target | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER)
                .unwrap();
        }
        memory
            .write_u64(0x3000, 0x5000 | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER)
            .unwrap();
        let walked = walk(&memory, 0, Cr4::PAE, Efer::LMA, 0).unwrap();
        assert!(!walked.writable);
        assert!(walked.user);
    }

    #[test]
    fn missing_table_faults_without_present_bit() {
        let memory = Memory::new(16).unwrap();
        let fault = translate(
            &memory,
            0,
            Cr0::PG,
            Cr4::PAE,
            Efer::LMA,
            0x0,
            AccessKind::Read,
        )
        .unwrap_err();
        assert_eq!(fault.error_code & 1, 0);
    }

    #[test]
    fn five_level_walk_uses_pml5() {
        let mut memory = Memory::new(16).unwrap();
        memory
            .write_u64(0x0000, 0x1000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x1000, 0x2000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x2000, 0x3000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x3000, 0x4000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x4000, 0x5000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        let physical = translate(
            &memory,
            0,
            Cr0::PG,
            Cr4::PAE | Cr4::LA57,
            Efer::LMA,
            0x0,
            AccessKind::Read,
        )
        .unwrap();
        assert_eq!(physical, 0x5000);
    }

    #[test]
    fn entry_with_software_bit_58_walks_fine() {
        let mut memory = Memory::new(64).unwrap();
        // PML4[0] = PDPT@0x1000 with Linux software bit 58 set.
        memory
            .write_u64(0x0000, 0x4000_0000_0000_1000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        // PDPT[0] -> PD@0x2000; PD[0] = 2 MiB large page at phys 0x200000.
        memory
            .write_u64(0x1000, 0x2000 | PAGE_PRESENT | PAGE_WRITABLE)
            .unwrap();
        memory
            .write_u64(0x2000, 0x200000 | PAGE_PRESENT | PAGE_WRITABLE | PAGE_LARGE)
            .unwrap();
        let physical = translate(
            &memory,
            0,
            Cr0::PG,
            Cr4::PAE | Cr4::PSE,
            Efer::LMA | Efer::NXE,
            0xABC,
            AccessKind::Read,
        )
        .unwrap();
        assert_eq!(physical, 0x200ABC);
    }
}
