//! I/O APIC MMIO at 0xFEC00000.
//!
//! The kernel programs one redirection entry per interrupt line; an asserted
//! line whose entry is unmasked delivers its vector to the local APIC, which
//! then prioritizes it against everything else in service. ISA interrupts are
//! edge triggered, so delivery happens on the rising edge of a line; level
//! triggered entries keep the remote-IRR bit until the local APIC signals EOI.

pub const IOAPIC_BASE: u64 = 0xFEC0_0000;
pub const IOAPIC_SIZE: u64 = 0x1000;

/// Number of input pins, matching the classic 82093AA part.
pub const IOAPIC_PINS: usize = 24;

const REG_ID: u32 = 0x00;
const REG_VERSION: u32 = 0x01;
const REG_ARBITRATION: u32 = 0x02;
const REG_REDIRECTION_BASE: u32 = 0x10;

/// Register select and window offsets inside the MMIO page.
const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;
/// Directed EOI register (IOAPIC version 0x20+); accepting writes here is
/// harmless for edge entries.
const IOEOI: u64 = 0x40;

const ENTRY_MASKED: u64 = 1 << 16;
const ENTRY_LEVEL_TRIGGERED: u64 = 1 << 15;
const ENTRY_REMOTE_IRR: u64 = 1 << 14;

pub struct IoApic {
    id: u32,
    register_select: u32,
    /// Full 64-bit redirection entries, one per pin.
    redirection: [u64; IOAPIC_PINS],
    /// Current level of each input line.
    lines: u32,
}

impl IoApic {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: 0,
            register_select: 0,
            // Every entry comes out of reset masked.
            redirection: [ENTRY_MASKED; IOAPIC_PINS],
            lines: 0,
        }
    }

    /// Applies the new line levels and returns the vectors that fire on this
    /// transition (rising edges of unmasked edge-triggered entries, plus
    /// newly-raised level entries whose remote IRR is clear).
    pub fn set_lines(&mut self, lines: u32) -> Vec<u8> {
        let rising = lines & !self.lines;
        self.lines = lines;
        let mut fired = Vec::new();
        for pin in 0..IOAPIC_PINS {
            let bit = 1_u32 << pin;
            if rising & bit == 0 {
                continue;
            }
            let entry = self.redirection[pin];
            if entry & ENTRY_MASKED != 0 {
                continue;
            }
            if entry & ENTRY_LEVEL_TRIGGERED != 0 {
                if entry & ENTRY_REMOTE_IRR != 0 {
                    continue;
                }
                self.redirection[pin] |= ENTRY_REMOTE_IRR;
            }
            let vector = (entry & 0xFF) as u8;
            if vector >= 16 {
                fired.push(vector);
            }
        }
        fired
    }

    /// Clears the remote-IRR bits for a vector when the local APIC signals
    /// EOI. Returns the vectors that immediately refire because their level
    /// line is still asserted.
    pub fn end_of_interrupt(&mut self, vector: u8) -> Vec<u8> {
        let mut refired = Vec::new();
        for pin in 0..IOAPIC_PINS {
            let entry = self.redirection[pin];
            if entry & ENTRY_LEVEL_TRIGGERED == 0
                || entry & ENTRY_REMOTE_IRR == 0
                || (entry & 0xFF) as u8 != vector
            {
                continue;
            }
            self.redirection[pin] &= !ENTRY_REMOTE_IRR;
            if self.lines & (1 << pin) != 0 && entry & ENTRY_MASKED == 0 {
                self.redirection[pin] |= ENTRY_REMOTE_IRR;
                refired.push(vector);
            }
        }
        refired
    }

    pub fn read(&mut self, offset: u64, _size: u8) -> u32 {
        match offset {
            IOREGSEL => self.register_select,
            IOWIN => self.read_register(self.register_select),
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u64, _size: u8, value: u32) {
        match offset {
            IOREGSEL => self.register_select = value & 0xFF,
            IOWIN => self.write_register(self.register_select, value),
            IOEOI => {
                let _ = self.end_of_interrupt((value & 0xFF) as u8);
            }
            _ => {}
        }
    }

    fn read_register(&self, register: u32) -> u32 {
        match register {
            REG_ID => self.id << 24,
            // Version 0x11 with the highest redirection entry in bits 16..24.
            REG_VERSION => 0x11 | (((IOAPIC_PINS as u32) - 1) << 16),
            REG_ARBITRATION => self.id << 24,
            _ => {
                let index = (register - REG_REDIRECTION_BASE) as usize;
                let pin = index / 2;
                if pin >= IOAPIC_PINS {
                    return 0;
                }
                let entry = self.redirection[pin];
                if index % 2 == 0 {
                    entry as u32
                } else {
                    (entry >> 32) as u32
                }
            }
        }
    }

    fn write_register(&mut self, register: u32, value: u32) {
        match register {
            REG_ID => self.id = (value >> 24) & 0xF,
            REG_VERSION | REG_ARBITRATION => {}
            _ => {
                let index = (register - REG_REDIRECTION_BASE) as usize;
                let pin = index / 2;
                if pin >= IOAPIC_PINS {
                    return;
                }
                let entry = &mut self.redirection[pin];
                if index % 2 == 0 {
                    // Remote IRR and delivery status are read-only; the mask
                    // bit (16) and trigger mode are software controlled.
                    let writable = 0xFFFF_FFFF_u64 & !(ENTRY_REMOTE_IRR | (1 << 12));
                    *entry = (*entry & !writable) | (u64::from(value) & writable);
                } else {
                    *entry = (*entry & 0xFFFF_FFFF) | (u64::from(value) << 32);
                }
            }
        }
    }
}

impl Default for IoApic {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(ioapic: &mut IoApic, pin: u32, low: u32) {
        ioapic.write(IOREGSEL, 4, REG_REDIRECTION_BASE + pin * 2);
        ioapic.write(IOWIN, 4, low);
        ioapic.write(IOREGSEL, 4, REG_REDIRECTION_BASE + pin * 2 + 1);
        ioapic.write(IOWIN, 4, 0);
    }

    #[test]
    fn reports_version_and_pin_count() {
        let mut ioapic = IoApic::new();
        ioapic.write(IOREGSEL, 4, REG_VERSION);
        let version = ioapic.read(IOWIN, 4);
        assert_eq!(version & 0xFF, 0x11);
        assert_eq!((version >> 16) & 0xFF, 23);
    }

    #[test]
    fn redirection_entries_come_out_of_reset_masked() {
        let mut ioapic = IoApic::new();
        assert!(ioapic.set_lines(0xFFFF_FFFF).is_empty());
    }

    #[test]
    fn an_unmasked_edge_entry_fires_on_the_rising_edge_only() {
        let mut ioapic = IoApic::new();
        program(&mut ioapic, 2, 0x30); // edge, unmasked, vector 0x30
        assert_eq!(ioapic.set_lines(1 << 2), vec![0x30]);
        // Still asserted: no second delivery without a falling edge first.
        assert!(ioapic.set_lines(1 << 2).is_empty());
        assert!(ioapic.set_lines(0).is_empty());
        assert_eq!(ioapic.set_lines(1 << 2), vec![0x30]);
    }

    #[test]
    fn masking_an_entry_suppresses_delivery() {
        let mut ioapic = IoApic::new();
        program(&mut ioapic, 4, 0x34 | (ENTRY_MASKED as u32));
        assert!(ioapic.set_lines(1 << 4).is_empty());
    }

    #[test]
    fn a_level_entry_holds_remote_irr_until_eoi() {
        let mut ioapic = IoApic::new();
        program(&mut ioapic, 5, 0x35 | (ENTRY_LEVEL_TRIGGERED as u32));
        assert_eq!(ioapic.set_lines(1 << 5), vec![0x35]);
        // The line stays high; remote IRR blocks a second delivery.
        assert!(ioapic.set_lines(0).is_empty());
        assert!(ioapic.set_lines(1 << 5).is_empty());
        // EOI with the line still high refires immediately.
        assert_eq!(ioapic.end_of_interrupt(0x35), vec![0x35]);
        // EOI after the line drops does not.
        ioapic.set_lines(0);
        assert!(ioapic.end_of_interrupt(0x35).is_empty());
    }

    #[test]
    fn registers_survive_a_read_write_round_trip() {
        let mut ioapic = IoApic::new();
        program(&mut ioapic, 3, 0x71);
        ioapic.write(IOREGSEL, 4, REG_REDIRECTION_BASE + 6);
        assert_eq!(ioapic.read(IOWIN, 4) & 0xFF, 0x71);
    }
}
