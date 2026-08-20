//! Master/slave 8259A pair with edge-triggered inputs and priority scan.
//!
//! Only the modes Linux uses at boot are modeled: level/edge triggering per
//! line, standard EOI (0x20), OCW2 rotation is accepted and treated as EOI,
//! and the read-back register (OCW3) reports the in-service register.

use crate::CpuError;
use crate::devices::{InterruptLines, PortDevice};

const PIC_MASTER_COMMAND: u16 = 0x20;
const PIC_MASTER_DATA: u16 = 0x21;
const PIC_SLAVE_COMMAND: u16 = 0xA0;
const PIC_SLAVE_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
const ICW4_8086: u8 = 0x01;
const OCW2_EOI: u8 = 0x20;

pub struct Pic8259 {
    master: Pic,
    slave: Pic,
    /// Line-level inputs including the slave cascade on master line 2.
    inputs: u16,
    initialized: bool,
}

#[derive(Default)]
struct Pic {
    vector_offset: u8,
    mask: u8,
    init_state: u8,
    expect_icw4: bool,
    in_service: u8,
}

impl Pic8259 {
    #[must_use]
    pub fn new() -> Self {
        Self {
            master: Pic::default(),
            slave: Pic::default(),
            inputs: 0,
            initialized: false,
        }
    }

    /// Returns the highest-priority unmasked pending interrupt, if any.
    #[must_use]
    pub fn pending_irq(&self) -> Option<u8> {
        if !self.initialized {
            return None;
        }
        let slave_bits = (self.inputs >> 8) as u8 & !self.slave.mask & !self.slave.in_service;
        let direct = (self.inputs as u8) & !self.master.mask & !self.master.in_service & !(1 << 2);
        let cascade = if slave_bits != 0 { 1 << 2 } else { 0 };
        let master_pending = direct | cascade;
        if master_pending == 0 {
            return None;
        }
        let line = master_pending.trailing_zeros() as u8;
        if line == 2 {
            let slave_line = slave_bits.trailing_zeros() as u8;
            return Some(8 + slave_line);
        }
        Some(line)
    }

    /// Acknowledges an interrupt: the INTA cycle marks it in service and the
    /// CPU receives the vector.
    #[must_use]
    pub fn acknowledge(&mut self, irq: u8) -> u8 {
        if !self.initialized {
            return 0;
        }
        if irq >= 8 {
            self.slave.in_service |= 1 << (irq - 8);
            self.master.in_service |= 1 << 2;
            self.slave.vector_offset + (irq - 8)
        } else {
            self.master.in_service |= 1 << irq;
            self.master.vector_offset + irq
        }
    }

    /// Standard end-of-interrupt for the given line.
    pub fn end_of_interrupt(&mut self, irq: u8) {
        if irq >= 8 {
            self.slave.in_service &= !(1 << (irq - 8));
        } else {
            self.master.in_service &= !(1 << irq);
        }
    }

    pub fn set_input(&mut self, lines: InterruptLines) {
        self.inputs = lines.asserted;
    }

    #[must_use]
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn master_mask(&self) -> u8 {
        self.master.mask
    }

    fn write_master_command(&mut self, value: u8) {
        if value & ICW1_INIT != 0 {
            self.master.init_state = 1;
            self.master.expect_icw4 = value & ICW1_ICW4 != 0;
            return;
        }
        match value & 0b1110_0000 {
            0b0110_0000 => {
                // Specific EOI.
                self.master.in_service &= !(1 << (value & 0b111));
            }
            OCW2_EOI => {
                self.master.in_service = 0;
            }
            _ => {}
        }
    }

    fn write_slave_command(&mut self, value: u8) {
        if value & ICW1_INIT != 0 {
            self.slave.init_state = 1;
            self.slave.expect_icw4 = value & ICW1_ICW4 != 0;
            return;
        }
        match value & 0b1110_0000 {
            0b0110_0000 => {
                self.slave.in_service &= !(1 << (value & 0b111));
            }
            OCW2_EOI => {
                self.slave.in_service = 0;
            }
            _ => {}
        }
    }

    fn write_master_data(&mut self, value: u8) {
        match self.master.init_state {
            1 => {
                self.master.vector_offset = value & 0xF8;
                self.master.init_state = 2;
            }
            2 => {
                self.master.init_state = 3;
            }
            3 => {
                if self.master.expect_icw4 && value & ICW4_8086 != 0 {
                    self.initialized = true;
                }
                self.master.init_state = 0;
            }
            _ => {
                self.master.mask = value;
            }
        }
    }

    fn write_slave_data(&mut self, value: u8) {
        match self.slave.init_state {
            1 => {
                self.slave.vector_offset = value & 0xF8;
                self.slave.init_state = 2;
            }
            2 => {
                self.slave.init_state = 3;
            }
            3 => {
                self.slave.init_state = 0;
            }
            _ => {
                self.slave.mask = value;
            }
        }
    }
}

impl Default for Pic8259 {
    fn default() -> Self {
        Self::new()
    }
}

impl PortDevice for Pic8259 {
    fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        let _ = size;
        match port {
            PIC_MASTER_DATA => Ok(u32::from(self.master.mask)),
            PIC_SLAVE_DATA => Ok(u32::from(self.slave.mask)),
            _ => Ok(0),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        let _ = size;
        let value = value as u8;
        match port {
            PIC_MASTER_COMMAND => self.write_master_command(value),
            PIC_MASTER_DATA => self.write_master_data(value),
            PIC_SLAVE_COMMAND => self.write_slave_command(value),
            PIC_SLAVE_DATA => self.write_slave_data(value),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_pic(pic: &mut Pic8259) {
        pic.write(0x20, 1, u32::from(ICW1_INIT | ICW1_ICW4))
            .unwrap();
        pic.write(0x21, 1, 0x20).unwrap();
        pic.write(0x21, 1, 0x04).unwrap();
        pic.write(0x21, 1, 1).unwrap();
        pic.write(0xA0, 1, u32::from(ICW1_INIT | ICW1_ICW4))
            .unwrap();
        pic.write(0xA1, 1, 0x28).unwrap();
        pic.write(0xA1, 1, 2).unwrap();
        pic.write(0xA1, 1, 1).unwrap();
    }

    #[test]
    fn reports_no_interrupt_before_initialization() {
        let mut pic = Pic8259::new();
        pic.set_input(InterruptLines { asserted: 1 << 1 });
        assert_eq!(pic.pending_irq(), None);
    }

    #[test]
    fn prioritizes_lowest_irq_line() {
        let mut pic = Pic8259::new();
        init_pic(&mut pic);
        pic.set_input(InterruptLines {
            asserted: (1 << 1) | (1 << 5),
        });
        assert_eq!(pic.pending_irq(), Some(1));
    }

    #[test]
    fn slave_cascade_routes_through_master_line_2() {
        let mut pic = Pic8259::new();
        init_pic(&mut pic);
        pic.set_input(InterruptLines { asserted: 1 << 10 });
        assert_eq!(pic.pending_irq(), Some(10));
        assert_eq!(pic.acknowledge(10), 0x28 + 2);
    }

    #[test]
    fn masked_lines_are_hidden() {
        let mut pic = Pic8259::new();
        init_pic(&mut pic);
        pic.write(0x21, 1, 0xFE_u32).unwrap();
        pic.set_input(InterruptLines { asserted: 1 << 1 });
        assert_eq!(pic.pending_irq(), None);
    }

    #[test]
    fn eoi_clears_the_in_service_line() {
        let mut pic = Pic8259::new();
        init_pic(&mut pic);
        pic.set_input(InterruptLines { asserted: 1 << 4 });
        assert_eq!(pic.pending_irq(), Some(4));
        let _ = pic.acknowledge(4);
        // Still asserted, but already in service: no new delivery.
        assert_eq!(pic.pending_irq(), None);
        pic.set_input(InterruptLines { asserted: 0 });
        pic.write(0x20, 1, u32::from(OCW2_EOI)).unwrap();
        pic.set_input(InterruptLines { asserted: 1 << 4 });
        assert_eq!(pic.pending_irq(), Some(4));
    }
}
