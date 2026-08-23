//! ACPI power-management register block for a non-hardware-reduced FADT.
//!
//! A normal (non-reduced) FADT keeps the legacy 8259 PIC and PIT alive, which
//! the serial console needs for its IRQ. In return the kernel's ACPICA layer
//! touches the PM registers during subsystem init: it clears PM1 status,
//! programs PM1 enable, and samples the power-management timer. None of these
//! drive real behaviour here, so the block just stores what is written and
//! reports a quiet, monotonically advancing timer.
//!
//! The FADT reports `SMI_CMD == 0`, so ACPICA treats the machine as already in
//! ACPI mode and never runs the SMI/SCI enable handshake; SCI_EN is reported
//! set regardless for callers that read PM1 control directly.

/// Base I/O port of the PM register block. Matches the address published in
/// the FADT's `PM1a_EVT_BLK`.
pub const PM_BASE: u16 = 0x600;

const PM1_STATUS: u16 = PM_BASE; // u16
const PM1_ENABLE: u16 = PM_BASE + 2; // u16
const PM1_CONTROL: u16 = PM_BASE + 4; // u16
const PM_TIMER: u16 = PM_BASE + 8; // u32

/// SCI_EN bit in PM1 control.
const SCI_ENABLE: u16 = 1 << 0;

/// Highest port the block answers for, inclusive.
pub const PM_LAST: u16 = PM_TIMER + 3;

pub struct AcpiPm {
    status: u16,
    enable: u16,
    control: u16,
}

impl AcpiPm {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: 0,
            enable: 0,
            // SCI_EN is reported set so a reader of PM1 control sees ACPI mode.
            control: SCI_ENABLE,
        }
    }

    #[must_use]
    pub fn handles(port: u16) -> bool {
        (PM_BASE..=PM_LAST).contains(&port)
    }

    /// Reads a PM register. The timer is derived from the TSC so that a replay
    /// of the same instruction stream reads the same values.
    #[must_use]
    pub fn read(&self, port: u16, size: u8, tsc: u64) -> u32 {
        match port {
            PM1_STATUS => u32::from(self.status),
            PM1_ENABLE => u32::from(self.enable),
            PM1_CONTROL => u32::from(self.control),
            PM_TIMER => {
                // The ACPI PM timer is a 24-bit counter nominally at
                // 3.579545 MHz. Scaling the TSC keeps it monotonic; the exact
                // rate does not matter because the kernel prefers the TSC and
                // treats a rejected pmtmr rate as simply unused.
                let _ = size;
                ((tsc >> 2) & 0x00FF_FFFF) as u32
            }
            _ => 0,
        }
    }

    /// Writes a PM register. PM1 status is write-1-to-clear.
    pub fn write(&mut self, port: u16, _size: u8, value: u32) {
        match port {
            PM1_STATUS => {
                // Writing a 1 clears the corresponding status bit.
                self.status &= !(value as u16);
            }
            PM1_ENABLE => self.enable = value as u16,
            PM1_CONTROL => {
                // Keep SCI_EN set; honour the rest of the write.
                self.control = (value as u16) | SCI_ENABLE;
            }
            _ => {}
        }
    }
}

impl Default for AcpiPm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_acpi_mode_through_sci_enable() {
        let pm = AcpiPm::new();
        assert_eq!(pm.read(PM1_CONTROL, 2, 0) & u32::from(SCI_ENABLE), 1);
    }

    #[test]
    fn status_is_write_one_to_clear() {
        let mut pm = AcpiPm::new();
        pm.status = 0xFFFF;
        pm.write(PM1_STATUS, 2, 0x0001);
        assert_eq!(pm.read(PM1_STATUS, 2, 0), 0xFFFE);
    }

    #[test]
    fn timer_advances_with_the_tsc() {
        let pm = AcpiPm::new();
        let early = pm.read(PM_TIMER, 4, 0x1000);
        let later = pm.read(PM_TIMER, 4, 0x1000 + (8 << 2));
        assert!(later > early);
    }

    #[test]
    fn claims_only_its_own_ports() {
        assert!(AcpiPm::handles(PM_BASE));
        assert!(AcpiPm::handles(PM_LAST));
        assert!(!AcpiPm::handles(PM_BASE - 1));
        assert!(!AcpiPm::handles(PM_LAST + 1));
    }
}
