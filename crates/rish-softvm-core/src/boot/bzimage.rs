//! Linux x86 bzImage boot loader with the 64-bit boot protocol.
//!
//! Parses the setup header, loads the whole kernel image, prepares the
//! boot_params zero page (E820 map, command line, initramfs), builds a 4 GiB
//! identity map, installs the __BOOT_CS/__BOOT_DS GDT, and enters the kernel
//! at kernel_base + 0x200 in long mode per Documentation/arch/x86/boot.rst.

use crate::arch::registers::{Cr0, Cr4, Efer, index};
use crate::arch::segments::{Descriptor, SegmentRegister, SegmentSelector};
use crate::{Cpu, CpuError};

pub const ZERO_PAGE_BASE: u64 = 0x10000;
pub const CMDLINE_BASE: u64 = 0x20000;
pub const STACK_BASE: u64 = 0x90000;
pub const KERNEL_BASE: u64 = 0x100000;
pub const KERNEL_ENTRY_OFFSET: u64 = 0x200;
pub const INITRD_BASE: u64 = 0x1000_0000;
pub const GDT_BASE: u64 = 0x40000;
pub const PML4_BASE: u64 = 0x50000;

const HDRS_MAGIC: u32 = 0x5372_6448;
const BOOT_FLAG: u16 = 0xAA55;

const LOADFLAG_LOADED_HIGH: u8 = 0x01;
const LOADFLAG_KEEP_SEGMENTS: u8 = 0x40;
const LOADFLAG_CAN_USE_HEAP: u8 = 0x80;

const CR0_LONG_MODE: u64 = 0x8000_0033; // PE | MP | ET | NE | WP | PG

#[derive(Clone, Debug)]
pub struct BootParams {
    pub command_line: String,
    pub memory_mib: usize,
}

/// Loads a bzImage plus initramfs and leaves the CPU at the 64-bit entry.
pub fn load(
    cpu: &mut Cpu,
    kernel_image: &[u8],
    initramfs: Option<&[u8]>,
    params: &BootParams,
) -> Result<(), CpuError> {
    let header = Header::parse(kernel_image)?;
    let setup_bytes = usize::from(header.setup_sectors) * 512 + 512;
    if kernel_image.len() < setup_bytes {
        return Err(CpuError::InvalidConfig(format!(
            "bzImage is {} bytes, setup section needs {setup_bytes}",
            kernel_image.len()
        )));
    }

    // Compressed kernel payload at the kernel's preferred address
    // (LOAD_PHYSICAL_ADDR): direct calls inside the image are linked against
    // it. The 64-bit entry is base + 0x200.
    let kernel_base = if header.pref_address != 0 {
        header.pref_address
    } else {
        KERNEL_BASE
    };
    // Copy the remainder of the image (payload, kernel_info, trailing
    // sections) to EOF: the image contains linked code past payload_length.
    let payload = &kernel_image[setup_bytes..];
    cpu.memory.write(kernel_base, payload)?;
    // boot_params zero page: per the 64-bit boot protocol the page is
    // zeroed and only the setup header (from offset 0x1f1) is loaded. The
    // real-mode boot sector and setup code must NOT be copied.
    let header_size = (setup_bytes as u64 - 0x1F1).min(0x100);
    cpu.memory.write(
        ZERO_PAGE_BASE + 0x1F1,
        &kernel_image[0x1F1..0x1F1 + header_size as usize],
    )?;
    // Command line.
    let mut cmdline = params.command_line.as_bytes().to_vec();
    cmdline.push(0);
    cpu.memory.write(CMDLINE_BASE, &cmdline)?;

    let (initrd_image, initrd_size) = match initramfs {
        Some(bytes) if !bytes.is_empty() => (INITRD_BASE, bytes.len() as u32),
        _ => (0, 0),
    };
    if let Some(bytes) = initramfs {
        if !bytes.is_empty() {
            cpu.memory.write(INITRD_BASE, bytes)?;
        }
    }

    // Runtime boot_params fields, at their absolute offsets in the zero
    // page (boot.rst offsets are relative to the start of boot_params).
    write_u8(cpu, ZERO_PAGE_BASE + 0x210, 0xFF)?; // type_of_loader
    let loadflags = LOADFLAG_LOADED_HIGH | LOADFLAG_KEEP_SEGMENTS | LOADFLAG_CAN_USE_HEAP;
    write_u8(cpu, ZERO_PAGE_BASE + 0x211, loadflags)?;
    write_u32(cpu, ZERO_PAGE_BASE + 0x218, initrd_image as u32)?; // ramdisk_image
    write_u32(cpu, ZERO_PAGE_BASE + 0x21C, initrd_size)?; // ramdisk_size
    write_u16(cpu, ZERO_PAGE_BASE + 0x224, 0xFE00)?; // heap_end_ptr + ext_loader_ver
    write_u32(cpu, ZERO_PAGE_BASE + 0x228, CMDLINE_BASE as u32)?; // cmd_line_ptr
    let alt_mem_k = (params.memory_mib as u64)
        .saturating_mul(1024)
        .saturating_sub(1024);
    write_u32(cpu, ZERO_PAGE_BASE + 0x1E0, alt_mem_k as u32)?; // alt_mem_k

    // E820 memory map.
    let ram_end = (params.memory_mib as u64) * 1024 * 1024;
    let entries: [(u64, u64, u32); 4] = [
        (0x0000_0000, 0x0009_FC00, 1),
        (0x0009_FC00, 0x0000_0400, 2),
        (0x000F_0000, 0x0001_0000, 2),
        (0x0010_0000, ram_end - 0x0010_0000, 1),
    ];
    write_u32(cpu, ZERO_PAGE_BASE + 0x2D0, entries.len() as u32)?;
    for (index, (base, size, kind)) in entries.iter().enumerate() {
        let address = ZERO_PAGE_BASE + 0x2D4 + (index as u64) * 20;
        write_u64(cpu, address, *base)?;
        write_u64(cpu, address + 8, *size)?;
        write_u32(cpu, address + 16, *kind)?;
    }

    enter_long_mode(cpu, params.memory_mib, kernel_base)?;
    Ok(())
}

/// Builds the identity map and GDT and enters long mode at kernel_base+0x200.
fn enter_long_mode(cpu: &mut Cpu, memory_mib: usize, kernel_base: u64) -> Result<(), CpuError> {
    // PML4[0] -> PDPT; PDPT[0..] -> PDs with 2 MiB identity pages over 4 GiB.
    cpu.memory.write_u64(PML4_BASE, 0x60000 | 0x3)?;
    let mut pdpt = 0x60000_u64;
    let mut pd = 0x70000_u64;
    let mut mapped = 0_u64;
    while mapped < 4 * 1024 * 1024 * 1024 {
        cpu.memory.write_u64(pdpt, pd | 0x3)?;
        for slot in 0..512_u64 {
            cpu.memory
                .write_u64(pd + slot * 8, (mapped + slot * 0x200000) | 0x83)?;
        }
        mapped += 512 * 0x200000;
        pdpt += 8;
        pd += 4096;
    }

    // GDT: null, __BOOT_CS (0x10, 4G flat exec/read), __BOOT_DS (0x18, 4G rw).
    cpu.memory.write_u64(GDT_BASE, 0)?;
    let cs = Descriptor {
        base: 0,
        limit: 0xFFFFF,
        granularity: true,
        default_32: false,
        long_mode: true,
        present: true,
        dpl: 0,
        system: false,
        descriptor_type: 0b1010,
        code: true,
        conforming: false,
        expand_down: false,
        writable_or_readable: true,
        accessed: false,
    };
    let ds = Descriptor {
        base: 0,
        limit: 0xFFFFF,
        granularity: true,
        default_32: false,
        long_mode: false,
        present: true,
        dpl: 0,
        system: false,
        descriptor_type: 0b0010,
        code: false,
        conforming: false,
        expand_down: false,
        writable_or_readable: true,
        accessed: false,
    };
    cpu.memory
        .write_u64(GDT_BASE + 0x10, encode_descriptor(&cs))?;
    cpu.memory
        .write_u64(GDT_BASE + 0x18, encode_descriptor(&ds))?;
    cpu.regs.gdt_base = GDT_BASE;
    cpu.regs.gdt_limit = 0x27;

    // CPU state: 64-bit, paging on, boot segments, RSI = boot_params.
    cpu.regs.cr4 = Cr4::PAE;
    cpu.regs.efer = Efer::LME | Efer::SCE | Efer::NXE;
    cpu.regs.cr3 = PML4_BASE;
    cpu.regs.cr0 = Cr0::from_bits_truncate(CR0_LONG_MODE);
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cs = cs.load(SegmentSelector(0x10));
    cpu.regs.ds = ds.load(SegmentSelector(0x18));
    cpu.regs.es = ds.load(SegmentSelector(0x18));
    cpu.regs.ss = ds.load(SegmentSelector(0x18));
    cpu.regs.fs = SegmentRegister {
        selector: SegmentSelector(0),
        base: 0,
        limit: u32::MAX,
        ..SegmentRegister::default()
    };
    cpu.regs.gs = cpu.regs.fs;
    cpu.regs.rip = kernel_base + KERNEL_ENTRY_OFFSET;
    cpu.regs.set_rsp(STACK_BASE);
    cpu.regs.set_gpr(index::RSI, ZERO_PAGE_BASE);
    // Interrupts disabled at the 64-bit entry.
    cpu.regs.rflags = crate::arch::registers::RFlags::empty();
    // Keep the memory size in RBX as a debugging convenience.
    cpu.regs
        .set_gpr(index::RBX, (memory_mib as u64) * 1024 * 1024);
    Ok(())
}

fn encode_descriptor(descriptor: &Descriptor) -> u64 {
    let mut entry = 0_u64;
    entry |= (descriptor.base & 0xFF00_0000) << 32;
    entry |= (descriptor.base & 0x00FF_0000) << 16;
    entry |= (descriptor.base & 0xFFFF) << 16;
    let mut limit = descriptor.limit;
    if descriptor.granularity {
        limit >>= 12;
    }
    entry |= u64::from(limit & 0xF) << 48;
    entry |= u64::from(limit & 0xFFFF);
    if descriptor.granularity {
        entry |= 1 << 55;
    }
    if descriptor.default_32 {
        entry |= 1 << 54;
    }
    if descriptor.long_mode {
        entry |= 1 << 53;
    }
    if descriptor.present {
        entry |= 1 << 47;
    }
    entry |= u64::from(descriptor.dpl & 0b11) << 45;
    entry |= u64::from(descriptor.descriptor_type & 0xF) << 40;
    if descriptor.code {
        entry |= 1 << 43;
    }
    if descriptor.conforming {
        entry |= 1 << 42;
    }
    if descriptor.expand_down {
        entry |= 1 << 42;
    }
    if descriptor.writable_or_readable {
        entry |= 1 << 41;
    }
    if descriptor.accessed {
        entry |= 1 << 40;
    }
    entry
}

fn write_u8(cpu: &mut Cpu, offset: u64, value: u8) -> Result<(), CpuError> {
    cpu.memory.write_u8(offset, value)
}

fn write_u16(cpu: &mut Cpu, offset: u64, value: u16) -> Result<(), CpuError> {
    cpu.memory.write_u16(offset, value)
}

fn write_u32(cpu: &mut Cpu, offset: u64, value: u32) -> Result<(), CpuError> {
    cpu.memory.write_u32(offset, value)
}

fn write_u64(cpu: &mut Cpu, offset: u64, value: u64) -> Result<(), CpuError> {
    cpu.memory.write_u64(offset, value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub setup_sectors: u8,
    pub version: u16,
    pub code32_start: u32,
    pub relocatable_kernel: u8,
    pub kernel_alignment: u32,
    pub init_size: u32,
    pub payload_offset: u32,
    pub payload_length: u32,
    pub pref_address: u64,
}

impl Header {
    fn parse(image: &[u8]) -> Result<Self, CpuError> {
        if image.len() < 512 + 0x80 {
            return Err(CpuError::InvalidConfig(
                "kernel image is too small for a bzImage".to_owned(),
            ));
        }
        let read_u16 = |offset: u64| -> u16 {
            u16::from_le_bytes([image[offset as usize], image[offset as usize + 1]])
        };
        let read_u32 = |offset: u64| -> u32 {
            u32::from_le_bytes([
                image[offset as usize],
                image[offset as usize + 1],
                image[offset as usize + 2],
                image[offset as usize + 3],
            ])
        };
        let boot_flag = read_u16(0x1F1 + 0x0D);
        if boot_flag != BOOT_FLAG {
            return Err(CpuError::InvalidConfig(format!(
                "kernel boot flag is {boot_flag:#x}, expected {BOOT_FLAG:#x}"
            )));
        }
        let magic = read_u32(0x1F1 + 0x11);
        if magic != HDRS_MAGIC {
            return Err(CpuError::InvalidConfig(format!(
                "kernel image has no HdrS magic (found {magic:#x})"
            )));
        }
        let version = read_u16(0x1F1 + 0x15);
        if version < 0x0200 {
            return Err(CpuError::InvalidConfig(format!(
                "boot protocol {version:#x} is too old; 2.00+ is required"
            )));
        }
        Ok(Self {
            setup_sectors: image[0x1F1],
            version,
            code32_start: read_u32(0x1F1 + 0x23),
            relocatable_kernel: image[0x1F1 + 0x43],
            kernel_alignment: read_u32(0x1F1 + 0x3F),
            init_size: read_u32(0x1F1 + 0x6F),
            payload_offset: read_u32(0x1F1 + 0x57),
            payload_length: read_u32(0x1F1 + 0x5B),
            pref_address: u64::from_le_bytes([
                image[0x1F1 + 0x67],
                image[0x1F1 + 0x68],
                image[0x1F1 + 0x69],
                image[0x1F1 + 0x6A],
                image[0x1F1 + 0x6B],
                image[0x1F1 + 0x6C],
                image[0x1F1 + 0x6D],
                image[0x1F1 + 0x6E],
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_bzimage() -> Vec<u8> {
        let mut image = vec![0_u8; 4096 + 64];
        image[0x1F1] = 2; // setup_sectors
        image[0x1F1 + 0x0D..0x1F1 + 0x0F].copy_from_slice(&BOOT_FLAG.to_le_bytes());
        image[0x1F1 + 0x11..0x1F1 + 0x15].copy_from_slice(&HDRS_MAGIC.to_le_bytes());
        image[0x1F1 + 0x15..0x1F1 + 0x17].copy_from_slice(&0x020F_u16.to_le_bytes());
        image[0x1F1 + 0x23..0x1F1 + 0x27].copy_from_slice(&0x0010_0000_u32.to_le_bytes());
        image[0x1F1 + 0x43] = 1; // relocatable_kernel
        image[0x1F1 + 0x3F..0x1F1 + 0x43].copy_from_slice(&0x0020_0000_u32.to_le_bytes());
        image[0x1F1 + 0x6F..0x1F1 + 0x73].copy_from_slice(&0x0100_0000_u32.to_le_bytes());
        // payload_offset and payload_length point past the setup section.
        image[0x1F1 + 0x57..0x1F1 + 0x5B].copy_from_slice(&0x600_u32.to_le_bytes());
        image[0x1F1 + 0x5B..0x1F1 + 0x5F].copy_from_slice(&0x300_u32.to_le_bytes());
        // A recognizable marker at the 64-bit entry point (payload + 0x200).
        image[0x600 + KERNEL_ENTRY_OFFSET as usize] = 0xE9;
        image
    }

    #[test]
    fn parses_header_fields() {
        let image = synthetic_bzimage();
        let header = Header::parse(&image).unwrap();
        assert_eq!(header.setup_sectors, 2);
        assert_eq!(header.version, 0x020F);
        assert_eq!(header.code32_start, 0x100000);
        assert_eq!(header.relocatable_kernel, 1);
        assert_eq!(header.kernel_alignment, 0x200000);
        assert_eq!(header.init_size, 0x1000000);
    }

    #[test]
    fn rejects_missing_magic() {
        let mut image = synthetic_bzimage();
        image[0x1F1 + 0x11] = 0;
        assert!(Header::parse(&image).is_err());
    }

    #[test]
    fn rejects_wrong_boot_flag() {
        let mut image = synthetic_bzimage();
        image[0x1F1 + 0x0D] = 0;
        assert!(Header::parse(&image).is_err());
    }

    #[test]
    fn loads_kernel_initrd_and_enters_long_mode() {
        let image = synthetic_bzimage();
        let mut cpu = Cpu::new(512, 0).unwrap();
        load(
            &mut cpu,
            &image,
            Some(&[0xAB; 16]),
            &BootParams {
                command_line: "console=ttyS0,115200n8 rdinit=/init".to_owned(),
                memory_mib: 512,
            },
        )
        .unwrap();
        // Setup header copied into the zero page.
        assert_eq!(cpu.memory.read_u8(ZERO_PAGE_BASE + 0x1F1).unwrap(), 2);
        // The rest of the zero page stays zeroed.
        assert_eq!(cpu.memory.read_u8(ZERO_PAGE_BASE + 0x10).unwrap(), 0);
        // Kernel payload copied to 1 MiB; marker at the 64-bit entry.
        assert_eq!(
            cpu.memory
                .read_u8(KERNEL_BASE + KERNEL_ENTRY_OFFSET)
                .unwrap(),
            0xE9
        );
        assert_eq!(cpu.memory.read_u8(KERNEL_BASE).unwrap(), image[0x600]);
        // Initramfs copied to 256 MiB.
        assert_eq!(cpu.memory.read_u8(INITRD_BASE).unwrap(), 0xAB);
        // Runtime boot_params fields at their absolute zero-page offsets.
        assert_eq!(cpu.memory.read_u8(ZERO_PAGE_BASE + 0x210).unwrap(), 0xFF);
        assert_eq!(
            cpu.memory.read_u32(ZERO_PAGE_BASE + 0x228).unwrap(),
            CMDLINE_BASE as u32
        );
        assert_eq!(
            cpu.memory.read_u32(ZERO_PAGE_BASE + 0x218).unwrap(),
            INITRD_BASE as u32
        );
        assert_eq!(cpu.memory.read_u32(ZERO_PAGE_BASE + 0x21C).unwrap(), 16);
        // E820 count and first entry.
        assert_eq!(cpu.memory.read_u32(ZERO_PAGE_BASE + 0x2D0).unwrap(), 4);
        assert_eq!(cpu.memory.read_u64(ZERO_PAGE_BASE + 0x2D4).unwrap(), 0);
        assert_eq!(
            cpu.memory.read_u64(ZERO_PAGE_BASE + 0x2DC).unwrap(),
            0x9FC00
        );
        // CPU state: long mode, paging, boot segments, entry point.
        assert_eq!(cpu.regs.mode(), crate::arch::registers::CpuMode::Long);
        assert!(cpu.regs.cr0.contains(Cr0::PG));
        assert_eq!(cpu.regs.cs.selector.0, 0x10);
        assert_eq!(cpu.regs.ds.selector.0, 0x18);
        assert_eq!(cpu.regs.rip, KERNEL_BASE + KERNEL_ENTRY_OFFSET);
        assert_eq!(cpu.regs.gpr(index::RSI), ZERO_PAGE_BASE);
        assert!(!cpu.regs.rflags.contains(crate::arch::registers::RFlags::IF));
    }

    #[test]
    fn identity_map_covers_kernel_and_initrd() {
        let image = synthetic_bzimage();
        let mut cpu = Cpu::new(64, 0).unwrap();
        load(
            &mut cpu,
            &image,
            None,
            &BootParams {
                command_line: String::new(),
                memory_mib: 64,
            },
        )
        .unwrap();
        // A 2 MiB identity page must translate kernel and initrd regions.
        assert_eq!(
            cpu.translate(KERNEL_BASE, crate::arch::paging::AccessKind::Read)
                .unwrap(),
            KERNEL_BASE
        );
        assert_eq!(
            cpu.translate(INITRD_BASE, crate::arch::paging::AccessKind::Read)
                .unwrap(),
            INITRD_BASE
        );
    }
}
