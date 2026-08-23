//! 8254 programmable interval timer.
//!
//! Channel 0 drives IRQ 0. Channel 2 is gated by port 0x61 and its output is
//! readable there, which is the path `quick_pit_calibrate` uses to measure the
//! TSC frequency. Channel 1 (DRAM refresh) is modeled only well enough to be
//! programmed without side effects. All three run from the 1,193,182 Hz clock.

use crate::CpuError;
use crate::devices::PortDevice;

pub const PIT_BASE_FREQUENCY_HZ: u64 = 1_193_182;

const PIT_CHANNEL_0: u16 = 0x40;
const PIT_CHANNEL_1: u16 = 0x41;
const PIT_CHANNEL_2: u16 = 0x42;
const PIT_MODE_COMMAND: u16 = 0x43;

/// System control port B: channel 2 gate, speaker enable, and the readback
/// bits the kernel polls.
pub const PORT_SYSTEM_CONTROL_B: u16 = 0x61;

const CONTROL_B_TIMER2_GATE: u8 = 1 << 0;
const CONTROL_B_SPEAKER_DATA: u8 = 1 << 1;
const CONTROL_B_REFRESH_TOGGLE: u8 = 1 << 4;
const CONTROL_B_TIMER2_OUTPUT: u8 = 1 << 5;

/// The refresh bit toggles every 15.085 microseconds on a real PC.
const REFRESH_TOGGLE_NANOSECONDS: u64 = 15_085;

pub struct Pit8254 {
    channels: [PitChannel; 3],
    /// Writable bits of port 0x61.
    control_b: u8,
    /// Shared clock accumulator in units of 1/PIT_BASE_FREQUENCY_HZ seconds
    /// scaled by 1e9, so no ticks are lost between calls.
    fractional_ticks: u64,
    refresh_nanoseconds: u64,
    refresh_state: bool,
}

struct PitChannel {
    reload: u16,
    counter: u16,
    mode: u8,
    access: AccessMode,
    /// Half-written low byte in low/high access mode.
    partial_write: Option<u8>,
    /// Latched value returned by the next reads, if any.
    latched: Option<u16>,
    read_high_next: bool,
    gate: bool,
    /// Channel output pin state.
    output: bool,
}

impl Default for PitChannel {
    fn default() -> Self {
        Self {
            reload: u16::MAX,
            counter: u16::MAX,
            mode: 0,
            access: AccessMode::LowHigh,
            partial_write: None,
            latched: None,
            read_high_next: false,
            // Channels 0 and 1 are permanently enabled; channel 2 follows
            // port 0x61 bit 0 and starts disabled.
            gate: true,
            output: false,
        }
    }
}

/// Counter access modes. A latch command is handled where it is decoded, so
/// it never becomes a persistent access mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AccessMode {
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
        channels[2].gate = false;
        Self {
            channels,
            control_b: 0,
            fractional_ticks: 0,
            refresh_nanoseconds: 0,
            refresh_state: false,
        }
    }

    /// Advances simulated time and returns whether channel 0's output pulsed,
    /// which is what raises IRQ 0.
    pub fn advance(&mut self, host_nanoseconds: u64) -> bool {
        self.refresh_nanoseconds = self.refresh_nanoseconds.saturating_add(host_nanoseconds);
        while self.refresh_nanoseconds >= REFRESH_TOGGLE_NANOSECONDS {
            self.refresh_nanoseconds -= REFRESH_TOGGLE_NANOSECONDS;
            self.refresh_state = !self.refresh_state;
        }
        self.fractional_ticks = self
            .fractional_ticks
            .saturating_add(host_nanoseconds.saturating_mul(PIT_BASE_FREQUENCY_HZ));
        let ticks = self.fractional_ticks / 1_000_000_000;
        if ticks == 0 {
            return false;
        }
        self.fractional_ticks -= ticks * 1_000_000_000;
        let mut channel_zero_fired = false;
        for (index, channel) in self.channels.iter_mut().enumerate() {
            if channel.advance(ticks) && index == 0 {
                channel_zero_fired = true;
            }
        }
        channel_zero_fired
    }

    /// Returns the remaining channel-0 count, used by the BIOS delay loop.
    #[must_use]
    pub fn channel0_count(&self) -> u16 {
        self.channels[0].counter
    }

    /// Reads system control port B.
    #[must_use]
    pub fn read_control_b(&self) -> u8 {
        let mut value = self.control_b & (CONTROL_B_TIMER2_GATE | CONTROL_B_SPEAKER_DATA);
        if self.refresh_state {
            value |= CONTROL_B_REFRESH_TOGGLE;
        }
        if self.channels[2].output {
            value |= CONTROL_B_TIMER2_OUTPUT;
        }
        value
    }

    /// Writes system control port B, which gates channel 2.
    pub fn write_control_b(&mut self, value: u8) {
        self.control_b = value & (CONTROL_B_TIMER2_GATE | CONTROL_B_SPEAKER_DATA);
        self.channels[2].gate = value & CONTROL_B_TIMER2_GATE != 0;
    }
}

impl PitChannel {
    /// Counts down by `ticks` and returns whether the output pulsed.
    fn advance(&mut self, ticks: u64) -> bool {
        if !self.gate {
            return false;
        }
        let period = u64::from(self.reload).max(1);
        let remaining = u64::from(self.counter);
        match self.mode & 0b111 {
            // Mode 0: interrupt on terminal count. The counter stops at zero
            // and the output stays high until the counter is reloaded.
            0 | 4 => {
                if ticks >= remaining {
                    self.counter = 0;
                    let already_high = self.output;
                    self.output = true;
                    !already_high
                } else {
                    self.counter -= ticks as u16;
                    false
                }
            }
            // Modes 2 and 3 are the periodic rate generator and square wave:
            // the counter reloads and the output pulses once per period.
            _ => {
                if ticks < remaining {
                    self.counter -= ticks as u16;
                    self.output = false;
                    return false;
                }
                let overshoot = ticks - remaining;
                self.counter = (period - (overshoot % period)) as u16;
                self.output = true;
                true
            }
        }
    }

    fn read(&mut self) -> u8 {
        let value = self.latched.unwrap_or(self.counter);
        if self.access == AccessMode::LowByte {
            self.latched = None;
            return value as u8;
        }
        if self.access == AccessMode::HighByte {
            self.latched = None;
            return (value >> 8) as u8;
        }
        if self.read_high_next {
            self.read_high_next = false;
            self.latched = None;
            (value >> 8) as u8
        } else {
            self.read_high_next = true;
            value as u8
        }
    }

    fn write(&mut self, value: u8) {
        match self.access {
            AccessMode::LowByte => self.reload_with(u16::from(value)),
            AccessMode::HighByte => self.reload_with(u16::from(value) << 8),
            AccessMode::LowHigh => match self.partial_write.take() {
                None => self.partial_write = Some(value),
                Some(low) => self.reload_with(u16::from(low) | (u16::from(value) << 8)),
            },
        }
    }

    fn reload_with(&mut self, reload: u16) {
        self.reload = reload;
        self.counter = reload;
        // Writing a count restarts a mode-0 countdown and drives its output low.
        self.output = false;
    }
}

impl PortDevice for Pit8254 {
    fn read(&mut self, port: u16, size: u8) -> Result<u32, CpuError> {
        let _ = size;
        if port == PORT_SYSTEM_CONTROL_B {
            return Ok(u32::from(self.read_control_b()));
        }
        let channel = match port {
            PIT_CHANNEL_0 => &mut self.channels[0],
            PIT_CHANNEL_1 => &mut self.channels[1],
            PIT_CHANNEL_2 => &mut self.channels[2],
            // The mode/command port is write-only.
            _ => return Ok(u32::MAX),
        };
        Ok(u32::from(channel.read()))
    }

    fn write(&mut self, port: u16, size: u8, value: u32) -> Result<(), CpuError> {
        let _ = size;
        if port == PORT_SYSTEM_CONTROL_B {
            self.write_control_b(value as u8);
            return Ok(());
        }
        if port == PIT_MODE_COMMAND {
            let channel_index = ((value >> 6) & 0b11) as usize;
            if channel_index >= 3 {
                // The read-back command is not used by the kernel paths this
                // machine supports.
                return Ok(());
            }
            let access = (value >> 4) & 0b11;
            let channel = &mut self.channels[channel_index];
            if access == 0 {
                // Counter latch: freeze the current count for the next reads.
                channel.latched = Some(channel.counter);
                channel.read_high_next = false;
                return Ok(());
            }
            channel.access = match access {
                1 => AccessMode::LowByte,
                2 => AccessMode::HighByte,
                _ => AccessMode::LowHigh,
            };
            channel.mode = ((value >> 1) & 0b111) as u8;
            channel.partial_write = None;
            channel.read_high_next = false;
            channel.latched = None;
            channel.output = false;
            return Ok(());
        }
        let channel = match port {
            PIT_CHANNEL_0 => &mut self.channels[0],
            PIT_CHANNEL_1 => &mut self.channels[1],
            PIT_CHANNEL_2 => &mut self.channels[2],
            _ => return Ok(()),
        };
        channel.write(value as u8);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nanoseconds that produce exactly the requested number of PIT ticks.
    fn nanoseconds_for(ticks: u64) -> u64 {
        ticks * 1_000_000_000 / PIT_BASE_FREQUENCY_HZ + 1
    }

    fn program(pit: &mut Pit8254, port: u16, command: u8, reload: u16) {
        pit.write(PIT_MODE_COMMAND, 1, u32::from(command)).unwrap();
        pit.write(port, 1, u32::from(reload & 0xFF)).unwrap();
        pit.write(port, 1, u32::from(reload >> 8)).unwrap();
    }

    #[test]
    fn programs_and_reads_back_channel_zero() {
        let mut pit = Pit8254::new();
        // Channel 0, low/high access, mode 2 (rate generator), binary.
        program(&mut pit, PIT_CHANNEL_0, 0b0011_0100, 1000);
        assert_eq!(pit.channel0_count(), 1000);
        // Latch and read back the same value.
        pit.write(PIT_MODE_COMMAND, 1, 0b0000_0000).unwrap();
        assert_eq!(pit.read(PIT_CHANNEL_0, 1).unwrap() as u8, 0xE8);
        assert_eq!(pit.read(PIT_CHANNEL_0, 1).unwrap() as u8, 0x03);
    }

    #[test]
    fn channel_zero_fires_at_the_programmed_rate() {
        let mut pit = Pit8254::new();
        program(&mut pit, PIT_CHANNEL_0, 0b0011_0100, 1);
        assert!(pit.advance(nanoseconds_for(1)));
        assert!(!pit.advance(0));
    }

    #[test]
    fn slow_divisor_does_not_fire_immediately() {
        let mut pit = Pit8254::new();
        program(&mut pit, PIT_CHANNEL_0, 0b0011_0100, 0xFFFF);
        // 65535 ticks at 1.193182 MHz is about 55 ms; 1 ms must not fire.
        assert!(!pit.advance(1_000_000));
    }

    #[test]
    fn channel_zero_keeps_its_rate_across_batched_advances() {
        // One advance of N ticks must fire as often as N advances of one tick.
        let mut batched = Pit8254::new();
        let mut single = Pit8254::new();
        program(&mut batched, PIT_CHANNEL_0, 0b0011_0100, 100);
        program(&mut single, PIT_CHANNEL_0, 0b0011_0100, 100);
        let step = nanoseconds_for(1);
        let mut single_fires = 0;
        for _ in 0..1000 {
            if single.advance(step) {
                single_fires += 1;
            }
        }
        assert!((9..=11).contains(&single_fires), "{single_fires}");
        // The batched timer sees the same elapsed time in ten chunks.
        let mut batched_fires = 0;
        for _ in 0..10 {
            if batched.advance(step * 100) {
                batched_fires += 1;
            }
        }
        assert_eq!(batched_fires, 10);
        assert!(batched.channel0_count() <= 100);
    }

    #[test]
    fn channel_two_only_counts_while_gated_on() {
        let mut pit = Pit8254::new();
        // Channel 2, low/high, mode 0, binary: the calibration programming.
        program(&mut pit, PIT_CHANNEL_2, 0b1011_0000, 0xFFFF);
        pit.advance(nanoseconds_for(1000));
        // Gate is low, so the count has not moved.
        pit.write(PIT_MODE_COMMAND, 1, 0b1000_0000).unwrap();
        assert_eq!(pit.read(PIT_CHANNEL_2, 1).unwrap() as u8, 0xFF);
        assert_eq!(pit.read(PIT_CHANNEL_2, 1).unwrap() as u8, 0xFF);
        pit.write(PORT_SYSTEM_CONTROL_B, 1, 0x01).unwrap();
        pit.advance(nanoseconds_for(1000));
        pit.write(PIT_MODE_COMMAND, 1, 0b1000_0000).unwrap();
        let low = pit.read(PIT_CHANNEL_2, 1).unwrap() as u16;
        let high = pit.read(PIT_CHANNEL_2, 1).unwrap() as u16;
        let count = low | (high << 8);
        assert!(count < 0xFFFF, "channel 2 did not count: {count:#x}");
    }

    #[test]
    fn channel_two_output_appears_on_port_0x61() {
        let mut pit = Pit8254::new();
        pit.write(PORT_SYSTEM_CONTROL_B, 1, 0x01).unwrap();
        program(&mut pit, PIT_CHANNEL_2, 0b1011_0000, 10);
        assert_eq!(pit.read_control_b() & CONTROL_B_TIMER2_OUTPUT, 0);
        pit.advance(nanoseconds_for(10));
        assert_ne!(pit.read_control_b() & CONTROL_B_TIMER2_OUTPUT, 0);
    }

    #[test]
    fn channel_two_never_raises_the_timer_interrupt() {
        let mut pit = Pit8254::new();
        pit.write(PORT_SYSTEM_CONTROL_B, 1, 0x01).unwrap();
        program(&mut pit, PIT_CHANNEL_2, 0b1011_0000, 1);
        assert!(!pit.advance(nanoseconds_for(1000)));
    }

    #[test]
    fn the_refresh_bit_toggles_over_time() {
        let mut pit = Pit8254::new();
        let first = pit.read_control_b() & CONTROL_B_REFRESH_TOGGLE;
        pit.advance(REFRESH_TOGGLE_NANOSECONDS + 1);
        assert_ne!(pit.read_control_b() & CONTROL_B_REFRESH_TOGGLE, first);
    }
}
