//! 8254 programmable interval timer.
//!
//! Channel 0 drives IRQ 0 at the configured rate; channel 2 feeds the PC
//! speaker (not modeled). The PIT runs on a 1,193,182 Hz clock.

use crate::CpuError;
use crate::devices::PortDevice;

pub const PIT_BASE_FREQUENCY_HZ: u64 = 1_193_182;

const PIT_CHANNEL_0: u16 = 0x40;
const PIT_CHANNEL_1: u16 = 0x41;
const PIT_CHANNEL_2: u16 = 0x42;
const PIT_MODE_COMMAND: u16 = 0x43;

pub struct Pit8254 {
    channels: [PitChannel; 3],
}

#[derive(Default)]
struct PitChannel {
    latch: u16,
    reload: u16,
    counter: u16,
    mode: u8,
    access: AccessMode,
    write_low: bool,
    read_latch: u16,
    /// Accumulated host-nanosecond ticks at the last advance.
    fractional_ticks: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AccessMode {
    LatchCount,
    LowByte,
    HighByte,
    #[default]
    LowHigh,
}

impl Default for Pit8254 {
    fn default() -> Self {
        Self::new()
    }
}

impl Pit8254 {
    #[must_use]
    pub fn new() -> Self {
        let mut channels = [
            PitChannel::default(),
            PitChannel::default(),
            PitChannel::default(),
        ];
        for channel in &mut channels {
            channel.reload = u16::MAX;
            channel.counter = u16::MAX;
        }
        Self { channels }
    }

    /// Advances simulated time and returns whether channel 0 fired this call.
    pub fn advance(&mut self, host_nanoseconds: u64) -> bool {
        let mut fired = false;
        for channel in &mut self.channels {
            channel.fractional_ticks = channel
                .fractional_ticks
                .saturating_add(host_nanoseconds * PIT_BASE_FREQUENCY_HZ);
            let ticks = channel.fractional_ticks / 1_000_000_000;
            channel.fractional_ticks %= 1_000_000_000;
            if ticks == 0 {
                continue;
            }
            let divisor = u32::from(channel.reload.max(1));
            let consumed = (ticks / u64::from(divisor)).min(1);
            if consumed == 0 {
                // Not a full period yet; keep a partial count.
                channel.counter = channel.counter.saturating_sub(ticks as u16);
                if channel.counter == 0 {
                    fired = true;
                    channel.counter = channel.reload;
                }
                continue;
            }
            fired = true;
            channel.counter = channel.reload;
        }
        fired
    }

    /// Returns the remaining channel-0 count, used by the BIOS delay loop.
    #[must_use]
    pub fn channel0_count(&self) -> u16 {
        self.channels[0].counter
    }
}

impl PortDevice for Pit8254 {
    fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        let _ = size;
        let channel = match port {
            PIT_CHANNEL_0 => &mut self.channels[0],
            PIT_CHANNEL_1 => &mut self.channels[1],
            PIT_CHANNEL_2 => &mut self.channels[2],
            _ => return Ok(u32::MAX),
        };
        Ok(u32::from(read_channel(channel)))
    }

    fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        let _ = size;
        if port == PIT_MODE_COMMAND {
            let channel_index = ((value >> 6) & 0b11) as usize;
            if channel_index >= 3 {
                return Ok(());
            }
            let access = (value >> 4) & 0b11;
            let mode = (value >> 1) & 0b111;
            let channel = &mut self.channels[channel_index];
            channel.access = match access {
                0 => AccessMode::LatchCount,
                1 => AccessMode::LowByte,
                2 => AccessMode::HighByte,
                _ => AccessMode::LowHigh,
            };
            channel.mode = mode as u8;
            if access == 0 {
                channel.read_latch = channel.counter;
            }
            return Ok(());
        }
        let channel = match port {
            PIT_CHANNEL_0 => &mut self.channels[0],
            PIT_CHANNEL_1 => &mut self.channels[1],
            PIT_CHANNEL_2 => &mut self.channels[2],
            _ => return Ok(()),
        };
        write_channel(channel, value as u8);
        Ok(())
    }
}

fn read_channel(channel: &mut PitChannel) -> u16 {
    match channel.access {
        AccessMode::LatchCount => {
            channel.access = AccessMode::LowHigh;
            channel.read_latch
        }
        AccessMode::LowByte => u16::from(channel.counter as u8),
        AccessMode::HighByte => channel.counter >> 8,
        AccessMode::LowHigh => {
            if channel.write_low {
                u16::from(channel.counter as u8)
            } else {
                channel.write_low = true;
                channel.counter >> 8
            }
        }
    }
}

fn write_channel(channel: &mut PitChannel, value: u8) {
    match channel.access {
        AccessMode::LowByte => {
            channel.reload = u16::from(value);
            channel.counter = channel.reload;
        }
        AccessMode::HighByte => {
            channel.reload = (u16::from(value)) << 8;
            channel.counter = channel.reload;
        }
        AccessMode::LowHigh => {
            if !channel.write_low {
                channel.latch = u16::from(value);
                channel.write_low = true;
            } else {
                channel.reload = channel.latch | (u16::from(value) << 8);
                channel.counter = channel.reload;
                channel.write_low = false;
            }
        }
        AccessMode::LatchCount => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_and_reads_back_channel_zero() {
        let mut pit = Pit8254::new();
        // Channel 0, low/high access, mode 2 (rate generator), binary.
        pit.write(PIT_MODE_COMMAND, 1, 0b0011_0100).unwrap();
        pit.write(PIT_CHANNEL_0, 1, 0xE8).unwrap();
        pit.write(PIT_CHANNEL_0, 1, 0x03).unwrap();
        assert_eq!(pit.channel0_count(), 1000);
        // Latch and read back the same value.
        pit.write(PIT_MODE_COMMAND, 1, 0b0000_0000).unwrap();
        assert_eq!(pit.read(PIT_CHANNEL_0, 1).unwrap() as u8, 0xE8);
        assert_eq!(pit.read(PIT_CHANNEL_0, 1).unwrap() as u8, 0x03);
    }

    #[test]
    fn channel_zero_fires_at_the_programmed_rate() {
        let mut pit = Pit8254::new();
        pit.write(PIT_MODE_COMMAND, 1, 0b0011_0100).unwrap();
        pit.write(PIT_CHANNEL_0, 1, 0x01).unwrap();
        pit.write(PIT_CHANNEL_0, 1, 0x00).unwrap();
        // divisor 1: fires on the first tick.
        assert!(pit.advance(1_000));
        assert!(!pit.advance(0));
    }

    #[test]
    fn slow_divisor_does_not_fire_immediately() {
        let mut pit = Pit8254::new();
        pit.write(PIT_MODE_COMMAND, 1, 0b0011_0100).unwrap();
        pit.write(PIT_CHANNEL_0, 1, 0xFF).unwrap();
        pit.write(PIT_CHANNEL_0, 1, 0xFF).unwrap();
        // 65535 ticks at 1.193182 MHz is about 55 ms; 1 ms must not fire.
        assert!(!pit.advance(1_000_000));
    }
}
