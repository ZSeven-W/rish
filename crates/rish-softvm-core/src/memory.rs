//! Physical guest memory with bounds-checked access.

use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use crate::CpuError;
use crate::devices::lapic::{LAPIC_BASE, LAPIC_SIZE, LocalApic};

pub struct Memory {
    ram: Box<[u8]>,
    lapic: Option<RefCell<LocalApic>>,
    /// Bumped on every write so translation caches can invalidate cheaply.
    generation: u64,
}

impl Memory {
    pub fn new(megabytes: usize) -> Result<Self, CpuError> {
        if megabytes == 0 {
            return Err(CpuError::InvalidConfig(
                "guest memory must be at least 1 MiB".to_owned(),
            ));
        }
        let bytes = megabytes
            .checked_mul(1024 * 1024)
            .ok_or_else(|| CpuError::InvalidConfig("guest memory size overflow".to_owned()))?;
        Ok(Self {
            ram: vec![0; bytes].into_boxed_slice(),
            lapic: None,
            generation: 0,
        })
    }

    /// Write generation, for translation-cache invalidation.
    #[inline]
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.ram.len()
    }

    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn attach_lapic(&mut self, queue: Arc<Mutex<VecDeque<u8>>>) {
        self.lapic = Some(RefCell::new(LocalApic::new(queue)));
    }

    pub fn lapic_tick(&self) {
        if let Some(lapic) = &self.lapic {
            lapic.borrow_mut().tick();
        }
    }

    fn in_lapic(address: u64) -> bool {
        (LAPIC_BASE..LAPIC_BASE + LAPIC_SIZE).contains(&address)
    }

    #[inline]
    pub fn read(&self, address: u64, output: &mut [u8]) -> Result<(), CpuError> {
        if Self::in_lapic(address) {
            if let Some(lapic) = &self.lapic {
                let offset = address - LAPIC_BASE;
                let value = lapic.borrow_mut().read(offset, output.len() as u8);
                let bytes = value.to_le_bytes();
                let first = output.len().min(4);
                output[..first].copy_from_slice(&bytes[..first]);
                if output.len() > 4 {
                    let value2 = lapic
                        .borrow_mut()
                        .read(offset + 4, (output.len() - 4) as u8);
                    let bytes2 = value2.to_le_bytes();
                    let rest = output.len() - 4;
                    output[4..].copy_from_slice(&bytes2[..rest]);
                }
                return Ok(());
            }
        }
        let end = address
            .checked_add(output.len() as u64)
            .ok_or(CpuError::GuestFault(format!(
                "memory read address overflow at {address:#x}"
            )))?;
        if end > self.ram.len() as u64 {
            return Err(CpuError::GuestFault(format!(
                "memory read {address:#x}..{end:#x} exceeds {} bytes",
                self.ram.len()
            )));
        }
        let start = address as usize;
        output.copy_from_slice(&self.ram[start..start + output.len()]);
        Ok(())
    }

    #[inline]
    pub fn write(&mut self, address: u64, input: &[u8]) -> Result<(), CpuError> {
        if Self::in_lapic(address) {
            if let Some(lapic) = &self.lapic {
                let offset = address - LAPIC_BASE;
                if input.len() == 4 {
                    let value = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
                    lapic.borrow_mut().write(offset, 4, value);
                } else {
                    let mut buffer = [0_u8; 4];
                    buffer[..input.len().min(4)].copy_from_slice(&input[..input.len().min(4)]);
                    let value = u32::from_le_bytes(buffer);
                    lapic.borrow_mut().write(offset, input.len() as u8, value);
                }
                return Ok(());
            }
        }
        let end = address
            .checked_add(input.len() as u64)
            .ok_or(CpuError::GuestFault(format!(
                "memory write address overflow at {address:#x}"
            )))?;
        if end > self.ram.len() as u64 {
            return Err(CpuError::GuestFault(format!(
                "memory write {address:#x}..{end:#x} exceeds {} bytes",
                self.ram.len()
            )));
        }
        let start = address as usize;
        self.ram[start..start + input.len()].copy_from_slice(input);
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    #[inline]
    pub fn read_u8(&self, address: u64) -> Result<u8, CpuError> {
        self.read_u64_at(address, 1).map(|value| value as u8)
    }

    #[inline]
    pub fn read_u16(&self, address: u64) -> Result<u16, CpuError> {
        self.read_u64_at(address, 2).map(|value| value as u16)
    }

    #[inline]
    pub fn read_u32(&self, address: u64) -> Result<u32, CpuError> {
        self.read_u64_at(address, 4).map(|value| value as u32)
    }

    #[inline]
    pub fn read_u64(&self, address: u64) -> Result<u64, CpuError> {
        self.read_u64_at(address, 8)
    }

    #[inline]
    fn read_u64_at(&self, address: u64, size: usize) -> Result<u64, CpuError> {
        let mut buffer = [0_u8; 8];
        self.read(address, &mut buffer[..size])?;
        Ok(u64::from_le_bytes(buffer))
    }

    #[inline]
    pub fn write_u8(&mut self, address: u64, value: u8) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }

    #[inline]
    pub fn write_u16(&mut self, address: u64, value: u16) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }

    #[inline]
    pub fn write_u32(&mut self, address: u64, value: u32) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }

    #[inline]
    pub fn write_u64(&mut self, address: u64, value: u64) -> Result<(), CpuError> {
        self.write(address, &value.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_scalar_widths() {
        let mut memory = Memory::new(1).unwrap();
        memory.write_u32(0x1000, 0xDEAD_BEEF).unwrap();
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0xDEAD_BEEF);
        assert_eq!(memory.read_u16(0x1000).unwrap(), 0xBEEF);
        memory.write_u8(0x1002, 0x42).unwrap();
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0xDE42_BEEF);
        memory.write_u64(0x2000, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(memory.read_u64(0x2000).unwrap(), 0x0102_0304_0506_0708);
    }

    #[test]
    fn out_of_bounds_reads_fail() {
        let memory = Memory::new(1).unwrap();
        let end = memory.len() as u64;
        assert!(memory.read_u32(end - 2).is_err());
        assert!(memory.read_u8(end).is_err());
    }

    #[test]
    fn zero_memory_is_rejected() {
        assert!(Memory::new(0).is_err());
    }
}
