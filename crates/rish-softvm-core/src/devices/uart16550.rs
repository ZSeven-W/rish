//! 16550A UART with FIFO semantics, wired to the host control plane.
//!
//! Two instances are used: the console (ttyS0) and the guest control channel
//! (ttyS1). Output bytes go into a bounded host-visible queue; input bytes
//! are queued by the host. The line status register reports THR empty and
//! receiver data with FIFO counts.

use std::collections::VecDeque;

use crate::CpuError;
use crate::devices::PortDevice;

const REG_THR_RBR: u16 = 0;
const REG_IER: u16 = 1;
const REG_IIR_FCR: u16 = 2;
const REG_LCR: u16 = 3;
const REG_MCR: u16 = 4;
const REG_LSR: u16 = 5;
const REG_MSR: u16 = 6;
const REG_SCR: u16 = 7;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;
const LSR_TX_EMPTY: u8 = 1 << 6;
const IIR_NO_INTERRUPT: u8 = 1;

pub struct Uart16550 {
    base: u16,
    input: VecDeque<u8>,
    output: VecDeque<u8>,
    input_capacity: usize,
    output_capacity: usize,
    dropped_output: u64,
    ier: u8,
    fifo_enabled: bool,
    lcr: u8,
    mcr: u8,
    scr: u8,
}

impl Uart16550 {
    #[must_use]
    pub fn new(base: u16, input_capacity: usize, output_capacity: usize) -> Self {
        Self {
            base,
            input: VecDeque::new(),
            output: VecDeque::new(),
            input_capacity,
            output_capacity,
            dropped_output: 0,
            ier: 0,
            fifo_enabled: false,
            lcr: 0b11,
            mcr: 0,
            scr: 0,
        }
    }

    /// Queues host-to-guest bytes. Excess bytes are dropped and counted.
    pub fn push_input(&mut self, bytes: &[u8]) -> usize {
        let mut accepted = 0;
        for byte in bytes {
            if self.input.len() >= self.input_capacity {
                break;
            }
            self.input.push_back(*byte);
            accepted += 1;
        }
        accepted
    }

    /// Drains guest-to-host bytes, up to capacity.
    #[must_use]
    pub fn drain_output(&mut self) -> Vec<u8> {
        let count = self.output.len().min(4096);
        self.output.drain(..count).collect()
    }

    #[must_use]
    pub fn dropped_output(&self) -> u64 {
        self.dropped_output
    }

    #[must_use]
    pub fn has_pending_interrupt(&self) -> bool {
        // Receiver-line interrupt when enabled and data is buffered.
        self.ier & 1 != 0 && !self.input.is_empty()
    }
    pub fn irq_line(&self) -> u8 {
        match self.base {
            0x3F8 => 4,
            0x2F8 => 3,
            _ => 0,
        }
    }
}

impl PortDevice for Uart16550 {
    fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        let _ = size;
        match port - self.base {
            REG_THR_RBR => Ok(u32::from(self.input.pop_front().unwrap_or(0))),
            REG_IER => Ok(u32::from(self.ier)),
            REG_IIR_FCR => {
                let value = if self.input.is_empty() {
                    IIR_NO_INTERRUPT
                } else {
                    0b100
                };
                Ok(u32::from(value))
            }
            REG_LCR => Ok(u32::from(self.lcr)),
            REG_MCR => Ok(u32::from(self.mcr)),
            REG_LSR => Ok(u32::from(
                LSR_THR_EMPTY | LSR_TX_EMPTY | {
                    if self.input.is_empty() {
                        0
                    } else {
                        LSR_DATA_READY
                    }
                },
            )),
            REG_MSR => Ok(0xB0),
            REG_SCR => Ok(u32::from(self.scr)),
            _ => Ok(0),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        let _ = size;
        let value = value as u8;
        match port - self.base {
            REG_THR_RBR => {
                if self.output.len() < self.output_capacity {
                    self.output.push_back(value);
                } else {
                    self.dropped_output = self.dropped_output.saturating_add(1);
                }
            }
            REG_IER => self.ier = value & 0xF,
            REG_IIR_FCR => self.fifo_enabled = value & 1 != 0,
            REG_LCR => self.lcr = value,
            REG_MCR => self.mcr = value,
            REG_SCR => self.scr = value,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_written_bytes_to_output_queue() {
        let mut uart = Uart16550::new(0x3F8, 16, 16);
        uart.write(0x3F8, 1, b'A'.into()).unwrap();
        uart.write(0x3F8, 1, b'B'.into()).unwrap();
        assert_eq!(uart.drain_output(), b"AB");
        assert!(uart.drain_output().is_empty());
    }

    #[test]
    fn lsr_reports_thr_empty_and_data_ready() {
        let mut uart = Uart16550::new(0x3F8, 16, 16);
        let lsr = uart.read(0x3F8 + 5, 1).unwrap() as u8;
        assert_ne!(lsr & LSR_THR_EMPTY, 0);
        assert_eq!(lsr & LSR_DATA_READY, 0);
        uart.push_input(b"x");
        let lsr = uart.read(0x3F8 + 5, 1).unwrap() as u8;
        assert_ne!(lsr & LSR_DATA_READY, 0);
        assert_eq!(uart.read(0x3F8, 1).unwrap() as u8, b'x');
    }

    #[test]
    fn output_capacity_drops_are_counted() {
        let mut uart = Uart16550::new(0x3F8, 16, 2);
        for byte in b"hello" {
            uart.write(0x3F8, 1, u32::from(*byte)).unwrap();
        }
        assert_eq!(uart.drain_output(), b"he");
        assert_eq!(uart.dropped_output(), 3);
    }
}
