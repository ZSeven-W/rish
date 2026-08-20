//! CMOS RAM and real-time clock behind ports 0x70/0x71.

use crate::CpuError;
use crate::devices::PortDevice;

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const RTC_SECONDS: u8 = 0x00;
const RTC_MINUTES: u8 = 0x02;
const RTC_HOURS: u8 = 0x04;
const RTC_STATUS_A: u8 = 0x0A;
const RTC_STATUS_B: u8 = 0x0B;

/// Seconds since the Unix epoch used to derive the RTC fields.
#[derive(Clone, Copy, Debug)]
pub struct Cmos {
    index: u8,
    boot_epoch_seconds: u64,
}

impl Default for Cmos {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Cmos {
    #[must_use]
    pub fn new(boot_epoch_seconds: u64) -> Self {
        Self {
            index: 0,
            boot_epoch_seconds,
        }
    }

    fn rtc_seconds(&self) -> u64 {
        self.boot_epoch_seconds
    }

    fn bcd(value: u64) -> u8 {
        ((value / 10) as u8) << 4 | (value % 10) as u8
    }

    fn read_register(&self, register: u8) -> u8 {
        let epoch = self.rtc_seconds();
        let seconds = epoch % 60;
        let minutes = (epoch / 60) % 60;
        let hours = (epoch / 3600) % 24;
        match register {
            RTC_SECONDS => Self::bcd(seconds),
            RTC_MINUTES => Self::bcd(minutes),
            RTC_HOURS => Self::bcd(hours),
            RTC_STATUS_A => 0x20,
            RTC_STATUS_B => 0x02,
            _ => 0,
        }
    }
}

impl PortDevice for Cmos {
    fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        let _ = size;
        match port {
            CMOS_DATA => Ok(u32::from(self.read_register(self.index))),
            _ => Ok(u32::MAX),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        let _ = size;
        if port == CMOS_INDEX {
            self.index = value as u8;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_then_data_round_trip() {
        let mut cmos = Cmos::new(0);
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_HOURS)).unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0);
        // 1 hour after epoch.
        let mut cmos = Cmos::new(3600);
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_HOURS)).unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x01);
    }

    #[test]
    fn bcd_encoding_of_minutes() {
        let mut cmos = Cmos::new(25 * 60);
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_MINUTES)).unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x25);
    }
}
