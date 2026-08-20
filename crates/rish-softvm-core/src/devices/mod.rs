//! Chipset devices: serial ports, interrupt controllers, timers.

pub mod cmos;
pub mod lapic;
pub mod pic8259;
pub mod pit8254;
pub mod uart16550;

pub use cmos::Cmos;
pub use pic8259::Pic8259;
pub use pit8254::Pit8254;
pub use uart16550::Uart16550;

use std::collections::BTreeMap;

use crate::CpuError;

/// A port-I/O device attached to the 16-bit I/O address space.
pub trait PortDevice {
    fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError>;
    fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError>;
}

/// The I/O port bus. Devices register port ranges; reads and writes dispatch
/// by the first matching range. Unclaimed ports return all-ones reads and
/// discard writes, matching bare-metal hardware behavior.
#[derive(Default)]
pub struct PortBus {
    devices: Vec<(u16, u16, Box<dyn PortDevice>)>,
    /// Bounded to keep lookup honest for the milestone.
    claimed: BTreeMap<u16, bool>,
}

impl PortBus {
    pub fn attach(&mut self, base: u16, size: u16, device: Box<dyn PortDevice>) {
        self.devices.push((base, size, device));
        for port in base..base.saturating_add(size) {
            self.claimed.insert(port, true);
        }
    }

    fn find(&mut self, port: u16) -> Option<&mut Box<dyn PortDevice>> {
        self.devices
            .iter_mut()
            .find(|(base, size, _)| port >= *base && port < base.saturating_add(*size))
            .map(|(_, _, device)| device)
    }

    pub fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        match self.find(port) {
            Some(device) => device.read(port, size),
            None => Ok(u32::MAX),
        }
    }

    pub fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        match self.find(port) {
            Some(device) => device.write(port, size, value),
            None => Ok(()),
        }
    }

    #[must_use]
    pub fn is_claimed(&self, port: u16) -> bool {
        self.claimed.contains_key(&port)
    }
}

/// Line-based interrupt wiring between devices and the CPU.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterruptLines {
    /// Interrupt request lines 0..=15, each asserted or not.
    pub asserted: u16,
}

impl InterruptLines {
    pub fn assert(&mut self, line: u8) {
        if line < 16 {
            self.asserted |= 1 << line;
        }
    }

    pub fn deassert(&mut self, line: u8) {
        if line < 16 {
            self.asserted &= !(1 << line);
        }
    }

    #[must_use]
    pub fn is_asserted(self, line: u8) -> bool {
        line < 16 && self.asserted & (1 << line) != 0
    }
}