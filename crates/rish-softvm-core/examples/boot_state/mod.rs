//! Host-side checkpoint format for the boot diagnostics example.
//!
//! Saves and restores guest RAM plus architectural register state. Device
//! state (PIC/PIT/CMOS/UART/LAPIC) is deliberately not serialized: these
//! checkpoints are only taken while the decompressor runs, before the
//! kernel arms any device. Restoring such a checkpoint into a fresh Cpu
//! therefore reproduces the exact pre-kernel machine state.

use std::io::{Read, Write};

use rish_softvm_core::arch::registers::{Cr0, Cr4, Efer, RFlags};
use rish_softvm_core::arch::segments::{SegmentAttributes, SegmentRegister, SegmentSelector};
use rish_softvm_core::{Cpu, CpuError};

const MAGIC: &[u8; 16] = b"RISH_BOOT_STATE1";

fn write_u16(out: &mut impl Write, value: u16) -> std::io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

fn write_u32(out: &mut impl Write, value: u32) -> std::io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

fn write_u64(out: &mut impl Write, value: u64) -> std::io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

fn read_u16(input: &mut impl Read) -> std::io::Result<u16> {
    let mut bytes = [0_u8; 2];
    input.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(input: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0_u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(input: &mut impl Read) -> std::io::Result<u64> {
    let mut bytes = [0_u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_segment(out: &mut impl Write, segment: &SegmentRegister) -> std::io::Result<()> {
    write_u16(out, segment.selector.0)?;
    write_u64(out, segment.base)?;
    write_u32(out, segment.limit)?;
    out.write_all(&[
        u8::from(segment.attributes.present),
        segment.attributes.dpl,
        u8::from(segment.attributes.system),
        segment.attributes.descriptor_type,
        u8::from(segment.attributes.accessed),
        u8::from(segment.granularity),
        u8::from(segment.default_32),
        u8::from(segment.long_mode),
        u8::from(segment.expand_down),
        u8::from(segment.writable_or_readable),
        u8::from(segment.code),
        u8::from(segment.conforming),
    ])
}

fn read_segment(input: &mut impl Read) -> std::io::Result<SegmentRegister> {
    let selector = SegmentSelector(read_u16(input)?);
    let base = read_u64(input)?;
    let limit = read_u32(input)?;
    let mut bytes = [0_u8; 12];
    input.read_exact(&mut bytes)?;
    Ok(SegmentRegister {
        selector,
        base,
        limit,
        attributes: SegmentAttributes {
            present: bytes[0] != 0,
            dpl: bytes[1],
            system: bytes[2] != 0,
            descriptor_type: bytes[3],
            accessed: bytes[4] != 0,
        },
        granularity: bytes[5] != 0,
        default_32: bytes[6] != 0,
        long_mode: bytes[7] != 0,
        expand_down: bytes[8] != 0,
        writable_or_readable: bytes[9] != 0,
        code: bytes[10] != 0,
        conforming: bytes[11] != 0,
    })
}

/// Writes a pre-kernel checkpoint: guest RAM plus CPU state.
pub fn save(cpu: &Cpu, out: &mut impl Write) -> Result<(), CpuError> {
    out.write_all(MAGIC).map_err(write_error)?;
    let memory_len = cpu.memory.len();
    write_u64(out, memory_len as u64).map_err(write_error)?;
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut written = 0_usize;
    while written < memory_len {
        let count = (memory_len - written).min(buffer.len());
        cpu.memory.read(written as u64, &mut buffer[..count])?;
        out.write_all(&buffer[..count]).map_err(write_error)?;
        written += count;
    }
    let regs = &cpu.regs;
    write_u64(out, regs.instructions_retired).map_err(write_error)?;
    write_u64(out, regs.rip).map_err(write_error)?;
    write_u64(out, regs.rflags.bits()).map_err(write_error)?;
    for value in regs.gpr {
        write_u64(out, value).map_err(write_error)?;
    }
    for value in regs.xmm {
        out.write_all(&value.to_le_bytes()).map_err(write_error)?;
    }
    write_u64(out, regs.cr0.bits()).map_err(write_error)?;
    write_u64(out, regs.cr2).map_err(write_error)?;
    write_u64(out, regs.cr3).map_err(write_error)?;
    write_u64(out, regs.cr4.bits()).map_err(write_error)?;
    write_u64(out, regs.cr8).map_err(write_error)?;
    write_u64(out, regs.efer.bits()).map_err(write_error)?;
    for segment in [
        &regs.cs, &regs.ds, &regs.es, &regs.fs, &regs.gs, &regs.ss, &regs.ldtr, &regs.tr,
    ] {
        write_segment(out, segment).map_err(write_error)?;
    }
    write_u64(out, regs.gdt_base).map_err(write_error)?;
    write_u32(out, regs.gdt_limit).map_err(write_error)?;
    write_u64(out, regs.idt_base).map_err(write_error)?;
    write_u32(out, regs.idt_limit).map_err(write_error)?;
    write_u64(out, regs.tr_base).map_err(write_error)?;
    write_u64(out, cpu.tsc).map_err(write_error)?;
    out.write_all(&[u8::from(cpu.halted), u8::from(cpu.boot_ok_seen)])
        .map_err(write_error)?;
    write_u64(out, cpu.kernel_gs_base).map_err(write_error)?;
    write_u64(out, cpu.xcr0).map_err(write_error)?;
    write_u16(out, cpu.fpu_control_word).map_err(write_error)?;
    write_u16(out, cpu.fpu_status_word).map_err(write_error)?;
    write_u32(out, cpu.mxcsr).map_err(write_error)?;
    Ok(())
}

/// Restores a checkpoint into a fresh machine, returning the guest memory
/// size in MiB.
pub fn restore(cpu: &mut Cpu, input: &mut impl Read) -> Result<u64, CpuError> {
    let mut magic = [0_u8; 16];
    input.read_exact(&mut magic).map_err(read_error)?;
    if &magic != MAGIC {
        return Err(CpuError::InvalidConfig(
            "checkpoint magic mismatch".to_owned(),
        ));
    }
    let memory_len = read_u64(input).map_err(read_error)?;
    if memory_len as usize != cpu.memory.len() {
        return Err(CpuError::InvalidConfig(format!(
            "checkpoint memory is {memory_len} bytes, machine is {}",
            cpu.memory.len()
        )));
    }
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut read = 0_usize;
    while read < memory_len as usize {
        let count = (memory_len as usize - read).min(buffer.len());
        input.read_exact(&mut buffer[..count]).map_err(read_error)?;
        cpu.memory.write(read as u64, &buffer[..count])?;
        read += count;
    }
    let regs = &mut cpu.regs;
    regs.instructions_retired = read_u64(input).map_err(read_error)?;
    regs.rip = read_u64(input).map_err(read_error)?;
    regs.rflags = RFlags::from_bits_retain(read_u64(input).map_err(read_error)?);
    for slot in &mut regs.gpr {
        *slot = read_u64(input).map_err(read_error)?;
    }
    for slot in &mut regs.xmm {
        let mut bytes = [0_u8; 16];
        input.read_exact(&mut bytes).map_err(read_error)?;
        *slot = u128::from_le_bytes(bytes);
    }
    regs.cr0 = Cr0::from_bits_retain(read_u64(input).map_err(read_error)?);
    regs.cr2 = read_u64(input).map_err(read_error)?;
    regs.cr3 = read_u64(input).map_err(read_error)?;
    regs.cr4 = Cr4::from_bits_retain(read_u64(input).map_err(read_error)?);
    regs.cr8 = read_u64(input).map_err(read_error)?;
    regs.efer = Efer::from_bits_retain(read_u64(input).map_err(read_error)?);
    regs.cs = read_segment(input).map_err(read_error)?;
    regs.ds = read_segment(input).map_err(read_error)?;
    regs.es = read_segment(input).map_err(read_error)?;
    regs.fs = read_segment(input).map_err(read_error)?;
    regs.gs = read_segment(input).map_err(read_error)?;
    regs.ss = read_segment(input).map_err(read_error)?;
    regs.ldtr = read_segment(input).map_err(read_error)?;
    regs.tr = read_segment(input).map_err(read_error)?;
    regs.gdt_base = read_u64(input).map_err(read_error)?;
    regs.gdt_limit = read_u32(input).map_err(read_error)?;
    regs.idt_base = read_u64(input).map_err(read_error)?;
    regs.idt_limit = read_u32(input).map_err(read_error)?;
    regs.tr_base = read_u64(input).map_err(read_error)?;
    cpu.tsc = read_u64(input).map_err(read_error)?;
    let mut flags = [0_u8; 2];
    input.read_exact(&mut flags).map_err(read_error)?;
    cpu.halted = flags[0] != 0;
    cpu.boot_ok_seen = flags[1] != 0;
    cpu.kernel_gs_base = read_u64(input).map_err(read_error)?;
    cpu.xcr0 = read_u64(input).map_err(read_error)?;
    cpu.fpu_control_word = read_u16(input).map_err(read_error)?;
    cpu.fpu_status_word = read_u16(input).map_err(read_error)?;
    cpu.mxcsr = read_u32(input).map_err(read_error)?;
    Ok(memory_len / (1024 * 1024))
}

fn write_error(error: std::io::Error) -> CpuError {
    CpuError::InvalidConfig(format!("checkpoint write: {error}"))
}

fn read_error(error: std::io::Error) -> CpuError {
    CpuError::InvalidConfig(format!("checkpoint read: {error}"))
}
