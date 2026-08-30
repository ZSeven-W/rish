//! CMOS RAM and real-time clock behind ports 0x70/0x71.

use crate::CpuError;
use crate::devices::PortDevice;

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const RTC_SECONDS: u8 = 0x00;
const RTC_MINUTES: u8 = 0x02;
const RTC_HOURS: u8 = 0x04;
const RTC_DAY_OF_WEEK: u8 = 0x06;
const RTC_DAY_OF_MONTH: u8 = 0x07;
const RTC_MONTH: u8 = 0x08;
const RTC_YEAR: u8 = 0x09;
const RTC_STATUS_A: u8 = 0x0A;
const RTC_STATUS_B: u8 = 0x0B;
const RTC_CENTURY: u8 = 0x32;

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
        let days = (epoch / 86_400) as i64;
        let (year, month, day) = civil_from_days(days);
        // Sunday = 0 .. Saturday = 6 (1970-01-01 was a Thursday).
        let weekday = (days + 4).rem_euclid(7) as u64;
        match register {
            RTC_SECONDS => Self::bcd(seconds),
            RTC_MINUTES => Self::bcd(minutes),
            RTC_HOURS => Self::bcd(hours),
            // The RTC stores the weekday as 1..=7 with Sunday = 1.
            RTC_DAY_OF_WEEK => Self::bcd(weekday + 1),
            RTC_DAY_OF_MONTH => Self::bcd(u64::from(day)),
            RTC_MONTH => Self::bcd(u64::from(month)),
            // Two-digit BCD year; rtc-cmos maps 0..=69 to 2000..=2069.
            RTC_YEAR => Self::bcd(year.rem_euclid(100) as u64),
            RTC_STATUS_A => 0x20,
            RTC_STATUS_B => 0x02,
            // Century register, read when the platform declares it.
            RTC_CENTURY => Self::bcd((year / 100) as u64),
            _ => 0,
        }
    }
}

/// Days since 1970-01-01 to (year, month 1..=12, day 1..=31), using Howard
/// Hinnant's days-from-civil inverse. The RTC only has to be correct enough
/// for the kernel's rtc_valid_tm checks and for TLS certificate validity.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
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

    #[test]
    fn civil_dates_match_known_calendar_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-01-01 is 10,957 days after the epoch.
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        // 2024-02-29 (leap day) and the day after: 10,957 days to
        // 2000-01-01, 8,766 to 2024-01-01, and 59 more to the leap day.
        let leap = 10_957 + 8_766 + 59;
        assert_eq!(civil_from_days(leap), (2024, 2, 29));
        assert_eq!(civil_from_days(leap + 1), (2024, 3, 1));
    }

    #[test]
    fn rtc_registers_report_the_boot_date() {
        // 2000-01-01 00:00:00 UTC was a Saturday.
        let mut cmos = Cmos::new(946_684_800);
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_DAY_OF_WEEK))
            .unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x07); // Saturday
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_DAY_OF_MONTH))
            .unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x01);
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_MONTH)).unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x01);
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_YEAR)).unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x00); // 2000
        cmos.write(CMOS_INDEX, 1, u32::from(RTC_CENTURY)).unwrap();
        assert_eq!(cmos.read(CMOS_DATA, 1).unwrap(), 0x20);
    }
}
