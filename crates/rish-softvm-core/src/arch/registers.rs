//! x86_64 architectural register state.
//!
//! The model covers real mode, 32-bit protected mode, and 64-bit long mode.
//! Segment registers keep their hidden descriptor caches so protected-mode
//! and long-mode addressing match real hardware.

use crate::arch::segments::{SegmentRegister, SegmentSelector};

pub const GP_REGISTERS: usize = 16;
pub const XMM_REGISTERS: usize = 16;

/// Fixed register indices for the general-purpose file.
#[allow(clippy::missing_docs_in_private_items)]
pub mod index {
    pub const RAX: usize = 0;
    pub const RCX: usize = 1;
    pub const RDX: usize = 2;
    pub const RBX: usize = 3;
    pub const RSP: usize = 4;
    pub const RBP: usize = 5;
    pub const RSI: usize = 6;
    pub const RDI: usize = 7;
    pub const R8: usize = 8;
    pub const R9: usize = 9;
    pub const R10: usize = 10;
    pub const R11: usize = 11;
    pub const R12: usize = 12;
    pub const R13: usize = 13;
    pub const R14: usize = 14;
    pub const R15: usize = 15;
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct RFlags: u64 {
        const CF = 1 << 0;
        const PF = 1 << 2;
        const AF = 1 << 4;
        const ZF = 1 << 6;
        const SF = 1 << 7;
        const TF = 1 << 8;
        const IF = 1 << 9;
        const DF = 1 << 10;
        const OF = 1 << 11;
        const IOPL_HIGH = 1 << 12;
        const IOPL_LOW = 1 << 13;
        const NT = 1 << 14;
        const RF = 1 << 16;
        const VM = 1 << 17;
        const AC = 1 << 18;
        const VIF = 1 << 19;
        const VIP = 1 << 20;
        const ID = 1 << 21;
    }
}

// Control register bits this milestone tracks.
bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Cr0: u64 {
        const PE = 1 << 0;
        const MP = 1 << 1;
        const EM = 1 << 2;
        const TS = 1 << 3;
        const ET = 1 << 4;
        const NE = 1 << 5;
        const WP = 1 << 16;
        const AM = 1 << 18;
        const NW = 1 << 29;
        const CD = 1 << 30;
        const PG = 1 << 31;
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Cr4: u64 {
        const VME = 1 << 0;
        const PVI = 1 << 1;
        const TSD = 1 << 2;
        const DE = 1 << 3;
        const PSE = 1 << 4;
        const PAE = 1 << 5;
        const MCE = 1 << 6;
        const PGE = 1 << 7;
        const PCE = 1 << 8;
        const OSFXSR = 1 << 9;
        const OSXMMEXCPT = 1 << 10;
        const LA57 = 1 << 12;
        const FSGSBASE = 1 << 16;
        const SMEP = 1 << 20;
        const SMAP = 1 << 21;
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Efer: u64 {
        const SCE = 1 << 0;
        const LME = 1 << 8;
        const LMA = 1 << 10;
        const NXE = 1 << 11;
        const SVME = 1 << 12;
    }
}

/// Execution mode derived from control state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuMode {
    Real,
    Protected16,
    Protected32,
    Long,
}

#[derive(Clone, Debug)]
pub struct Registers {
    pub gpr: [u64; GP_REGISTERS],
    pub rip: u64,
    pub rflags: RFlags,
    pub xmm: [u128; XMM_REGISTERS],
    pub cr0: Cr0,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: Cr4,
    pub cr8: u64,
    pub efer: Efer,
    pub cs: SegmentRegister,
    pub ds: SegmentRegister,
    pub es: SegmentRegister,
    pub fs: SegmentRegister,
    pub gs: SegmentRegister,
    pub ss: SegmentRegister,
    pub gdt_base: u64,
    pub gdt_limit: u32,
    pub idt_base: u64,
    pub idt_limit: u32,
    pub ldtr: SegmentRegister,
    pub tr: SegmentRegister,
    pub tr_base: u64,
    pub instructions_retired: u64,
}

impl Default for Registers {
    fn default() -> Self {
        let mut regs = Self {
            gpr: [0; GP_REGISTERS],
            rip: 0,
            rflags: RFlags::empty(),
            xmm: [0; XMM_REGISTERS],
            cr0: Cr0::empty(),
            cr2: 0,
            cr3: 0,
            cr4: Cr4::empty(),
            cr8: 0,
            efer: Efer::empty(),
            cs: SegmentRegister::default(),
            ds: SegmentRegister::default(),
            es: SegmentRegister::default(),
            fs: SegmentRegister::default(),
            gs: SegmentRegister::default(),
            ss: SegmentRegister::default(),
            gdt_base: 0,
            gdt_limit: 0,
            idt_base: 0,
            idt_limit: 0,
            ldtr: SegmentRegister::default(),
            tr: SegmentRegister::default(),
            tr_base: 0,
            instructions_retired: 0,
        };
        // Legacy reset state: 16-bit real mode with the usual segment bases.
        regs.cs = SegmentRegister::real_mode(SegmentSelector(0xF000));
        regs.ds = SegmentRegister::real_mode(SegmentSelector(0));
        regs.es = SegmentRegister::real_mode(SegmentSelector(0));
        regs.fs = SegmentRegister::real_mode(SegmentSelector(0));
        regs.gs = SegmentRegister::real_mode(SegmentSelector(0));
        regs.ss = SegmentRegister::real_mode(SegmentSelector(0));
        regs.cr0 = Cr0::ET | Cr0::NE;
        regs
    }
}

impl Registers {
    #[inline]
    #[must_use]
    pub fn gpr(&self, index: usize) -> u64 {
        self.gpr[index]
    }

    #[inline]
    pub fn set_gpr(&mut self, index: usize, value: u64) {
        self.gpr[index] = value;
    }

    #[inline]
    #[must_use]
    pub fn rsp(&self) -> u64 {
        self.gpr[index::RSP]
    }

    #[inline]
    pub fn set_rsp(&mut self, value: u64) {
        self.gpr[index::RSP] = value;
    }

    #[inline]
    #[must_use]
    pub fn rbp(&self) -> u64 {
        self.gpr[index::RBP]
    }

    #[inline]
    #[must_use]
    pub fn mode(&self) -> CpuMode {
        if self.efer.contains(Efer::LMA) {
            CpuMode::Long
        } else if self.cr0.contains(Cr0::PE) {
            if self.cs.is_32_bit_code() {
                CpuMode::Protected32
            } else {
                CpuMode::Protected16
            }
        } else {
            CpuMode::Real
        }
    }

    /// Effective code segment base for the current mode.
    #[inline]
    #[must_use]
    pub fn code_base(&self) -> u64 {
        match self.mode() {
            CpuMode::Real | CpuMode::Protected16 | CpuMode::Protected32 => self.cs.base,
            CpuMode::Long => 0,
        }
    }

    /// Effective stack segment base for the current mode.
    #[inline]
    #[must_use]
    pub fn stack_base(&self) -> u64 {
        match self.mode() {
            CpuMode::Long => 0,
            _ => self.ss.base,
        }
    }

    /// Effective data segment base for a segment override, if any.
    #[inline]
    #[must_use]
    pub fn data_base(&self, segment: iced_x86::Register) -> u64 {
        let segment = match segment {
            iced_x86::Register::ES => &self.es,
            iced_x86::Register::FS => &self.fs,
            iced_x86::Register::GS => &self.gs,
            _ => &self.ds,
        };
        match self.mode() {
            CpuMode::Long => 0,
            _ => segment.base,
        }
    }

    /// Address size in bytes for the current mode and a possible override.
    #[inline]
    #[must_use]
    pub fn address_size(&self, address_size: iced_x86::CodeSize) -> u8 {
        match (self.mode(), address_size) {
            (CpuMode::Long, iced_x86::CodeSize::Code32) => 4,
            (CpuMode::Long, _) => 8,
            (_, iced_x86::CodeSize::Code16) => 2,
            (_, _) => 4,
        }
    }
}

pub use bitflags;
