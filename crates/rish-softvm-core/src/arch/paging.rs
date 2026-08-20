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
    let reserved = reserved_mask(efer);
    if efer.contains(Efer::LMA) {
        return if cr4.contains(Cr4::LA57) {
            walk_generic(memory, cr3, cr4, efer, reserved, linear, kind, true)
        } else {
            walk_generic(memory, cr3, cr4, efer, reserved, linear, kind, false)
        };
    }
    if cr4.contains(Cr4::PAE) {
        return walk_pae32(memory, cr3, cr4, efer, reserved, linear, kind);
    }
    walk_2_level(memory, cr3, cr4, efer, reserved, linear, kind)
}

fn reserved_mask(efer: Efer) -> u64 {
    if efer.contains(Efer::NXE) {
        !TABLE_MASK & !0xFFF & !PAGE_NX
    } else {
        !TABLE_MASK & !0xFFF
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_generic(
    memory: &Memory,
    cr3: u64,
    cr4: Cr4,
    efer: Efer,
    reserved: u64,
    linear: u64,
    kind: AccessKind,
    five_level: bool,
) -> Result<u64, PageFault> {
    let offset = linear & 0xFFF;
    let mut entry = if five_level {
        let index = (linear >> 48) & 0x1FF;
        read_entry(memory, (cr3 & TABLE_MASK) + index * 8, linear, reserved)?
    } else {
        let index = (linear >> 39) & 0x1FF;
        read_entry(memory, (cr3 & TABLE_MASK) + index * 8, linear, reserved)?
    };
    if entry & PAGE_LARGE != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    if five_level {
        let pml4_index = (linear >> 39) & 0x1FF;
        entry = read_entry(memory, entry + pml4_index * 8, linear, reserved)?;
        if entry & PAGE_LARGE != 0 {
            return Err(PageFault {
                linear,
                error_code: 0b1000,
            });
        }
    }
    let pdpt_index = (linear >> 30) & 0x1FF;
    entry = read_entry(memory, entry + pdpt_index * 8, linear, reserved)?;
    if entry & PAGE_LARGE != 0 {
        return large_page(entry, linear, 30, cr4);
    }
    let pd_index = (linear >> 21) & 0x1FF;
    entry = read_entry(memory, entry + pd_index * 8, linear, reserved)?;
    if entry & PAGE_LARGE != 0 {
        return large_page(entry, linear, 21, cr4);
    }
    let pt_index = (linear >> 12) & 0x1FF;
    entry = read_entry(memory, entry + pt_index * 8, linear, reserved)?;
    if entry & PAGE_LARGE != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    check_access(entry, linear, kind)?;
    let _ = efer;
    Ok((entry & TABLE_MASK) + offset)
}

#[allow(clippy::too_many_arguments)]
fn walk_pae32(
    memory: &Memory,
    cr3: u64,
    cr4: Cr4,
    efer: Efer,
    reserved: u64,
    linear: u64,
    kind: AccessKind,
) -> Result<u64, PageFault> {
    let offset = linear & 0xFFF;
    let pdpt_index = (linear >> 30) & 0b11;
    let mut entry = read_entry(
        memory,
        (cr3 & 0xFFFF_FFF0) + pdpt_index * 8,
        linear,
        reserved,
    )?;
    if entry & PAGE_LARGE != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    let pd_index = (linear >> 21) & 0x1FF;
    entry = read_entry(memory, entry + pd_index * 8, linear, reserved)?;
    if entry & PAGE_LARGE != 0 {
        return large_page(entry, linear, 21, cr4);
    }
    let pt_index = (linear >> 12) & 0x1FF;
    entry = read_entry(memory, entry + pt_index * 8, linear, reserved)?;
    if entry & PAGE_LARGE != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    check_access(entry, linear, kind)?;
    let _ = efer;
    Ok((entry & TABLE_MASK) + offset)
}

#[allow(clippy::too_many_arguments)]
fn walk_2_level(
    memory: &Memory,
    cr3: u64,
    cr4: Cr4,
    efer: Efer,
    reserved: u64,
    linear: u64,
    kind: AccessKind,
) -> Result<u64, PageFault> {
    let offset = linear & 0xFFF;
    let pd_index = (linear >> 22) & 0x3FF;
    let entry = read_entry(memory, (cr3 & 0xFFFF_F000) + pd_index * 4, linear, reserved)?;
    if cr4.contains(Cr4::PSE) && entry & PAGE_LARGE != 0 {
        return Ok((entry & 0xFFC0_0000) + (linear & 0x003F_FFFF));
    }
    if entry & PAGE_LARGE != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    let pt_index = (linear >> 12) & 0x3FF;
    let entry = read_entry(memory, entry + pt_index * 4, linear, reserved)?;
    check_access(entry, linear, kind)?;
    let _ = efer;
    Ok((entry & 0xFFFF_F000) + offset)
}

fn read_entry(memory: &Memory, address: u64, linear: u64, reserved: u64) -> Result<u64, PageFault> {
    let entry = memory.read_u64(address).map_err(|_| PageFault {
        linear,
        error_code: 0,
    })?;
    if entry & PAGE_PRESENT == 0 {
        return Err(PageFault {
            linear,
            error_code: 0,
        });
    }
    if entry & reserved != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    Ok(entry & TABLE_MASK)
}

fn check_access(entry: u64, linear: u64, kind: AccessKind) -> Result<(), PageFault> {
    // Supervisor-only first milestone: the guest boots as ring 0. The U/S
    // distinction is reserved for the user-mode milestone.
    if matches!(kind, AccessKind::Write) && entry & PAGE_WRITABLE == 0 {
        return Err(PageFault {
            linear,
            error_code: 0b10,
        });
    }
    if matches!(kind, AccessKind::Execute) && entry & PAGE_NX != 0 {
        return Err(PageFault {
            linear,
            error_code: 0b1_0000,
        });
    }
    Ok(())
}

fn large_page(entry: u64, linear: u64, shift: u32, cr4: Cr4) -> Result<u64, PageFault> {
    if shift == 30 && !cr4.contains(Cr4::PSE) {
        return Err(PageFault {
            linear,
            error_code: 0b1000,
        });
    }
    let mask = (1_u64 << shift) - 1;
    Ok((entry & !mask & TABLE_MASK) + (linear & mask))
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
            Cr0::PG,
            Cr4::PAE,
            Efer::LMA,
            0x0,
            AccessKind::Write,
        )
        .unwrap_err();
        assert_eq!(fault.error_code & 0b10, 0b10);
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
}
