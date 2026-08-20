//! Local APIC MMIO at 0xFEE00000.
//!
//! Provides the register surface the x86_64 kernel probes at boot: ID,
//! version, spurious vector, LVT entries, EOI, and a countdown timer that
//! queues its vector through the shared interrupt queue.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub const LAPIC_BASE: u64 = 0xFEE0_0000;
pub const LAPIC_SIZE: u64 = 0x1000;

const LAPIC_ID: u64 = 0x20;
const LAPIC_VERSION: u64 = 0x30;
const LAPIC_TPR: u64 = 0x80;
const LAPIC_APR: u64 = 0x90;
const LAPIC_PPR: u64 = 0xA0;
const LAPIC_EOI: u64 = 0xB0;
const LAPIC_LDR: u64 = 0xD0;
const LAPIC_DFR: u64 = 0xE0;
const LAPIC_SVR: u64 = 0xF0;
const LAPIC_ISR_BASE: u64 = 0x100;
const LAPIC_TMR_BASE: u64 = 0x180;
const LAPIC_IRR_BASE: u64 = 0x200;
const LAPIC_ESR: u64 = 0x280;
const LAPIC_ICR_LO: u64 = 0x300;
const LAPIC_ICR_HI: u64 = 0x310;
const LAPIC_LVT_TIMER: u64 = 0x320;
const LAPIC_LVT_THERMAL: u64 = 0x330;
const LAPIC_LVT_PERF: u64 = 0x340;
const LAPIC_LVT_LINT0: u64 = 0x350;
const LAPIC_LVT_LINT1: u64 = 0x360;
const LAPIC_LVT_ERROR: u64 = 0x370;
const LAPIC_TIMER_INITIAL: u64 = 0x380;
const LAPIC_TIMER_CURRENT: u64 = 0x390;
const LAPIC_TIMER_DIVIDE: u64 = 0x3E0;

pub struct LocalApic {
    svr: u32,
    lvt_timer: u32,
    timer_initial: u32,
    timer_current: u32,
    timer_divide: u32,
    timer_fractional: u64,
    interrupt_queue: Arc<Mutex<VecDeque<u8>>>,
}

impl LocalApic {
    #[must_use]
    pub fn new(interrupt_queue: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self {
            svr: 0xFF,
            lvt_timer: 0x0001_0000,
            timer_initial: 0,
            timer_current: 0,
            timer_divide: 0,
            timer_fractional: 0,
            interrupt_queue,
        }
    }

    /// Advances the countdown by one guest instruction and queues the timer
    /// vector when it expires.
    pub fn tick(&mut self) {
        if self.timer_initial == 0 || self.timer_current == 0 {
            return;
        }
        self.timer_fractional = self.timer_fractional.saturating_add(1);
        let divide = match self.timer_divide & 0b11 {
            0b11 => 1,
            0b00 => 2,
            0b01 => 4,
            0b10 => 8,
            _ => 2,
        };
        if self.timer_fractional >= divide {
            self.timer_fractional = 0;
            self.timer_current = self.timer_current.saturating_sub(1);
            if self.timer_current == 0 {
                self.timer_current = self.timer_initial;
                let masked = self.lvt_timer & (1 << 16) != 0;
                let vector = (self.lvt_timer & 0xFF) as u8;
                if !masked && vector != 0 && self.svr & (1 << 8) != 0 {
                    if let Ok(mut queue) = self.interrupt_queue.lock() {
                        if queue.len() < 64 {
                            queue.push_back(vector);
                        }
                    }
                }
            }
        }
    }

    /// Returns the timer vector queue shared with the CPU.
    #[must_use]
    pub fn interrupt_queue(&self) -> Arc<Mutex<VecDeque<u8>>> {
        Arc::clone(&self.interrupt_queue)
    }

    pub fn read(&mut self, offset: u64, size: u8) -> u32 {
        let value = match offset {
            LAPIC_ID => 0,
            LAPIC_VERSION => 0x0005_0014,
            LAPIC_TPR | LAPIC_APR | LAPIC_PPR => 0,
            LAPIC_EOI => 0,
            LAPIC_LDR => 0,
            LAPIC_DFR => 0xFFFF_FFFF,
            LAPIC_SVR => self.svr,
            LAPIC_ISR_BASE..=0x17F => 0,
            LAPIC_TMR_BASE..=0x1FF => 0,
            LAPIC_IRR_BASE..=0x27F => 0,
            LAPIC_ESR => 0,
            LAPIC_ICR_LO => 0,
            LAPIC_ICR_HI => 0,
            LAPIC_LVT_TIMER => self.lvt_timer,
            LAPIC_LVT_THERMAL | LAPIC_LVT_PERF | LAPIC_LVT_LINT0 | LAPIC_LVT_LINT1
            | LAPIC_LVT_ERROR => 0x0001_0000,
            LAPIC_TIMER_INITIAL => self.timer_initial,
            LAPIC_TIMER_CURRENT => self.timer_current,
            LAPIC_TIMER_DIVIDE => self.timer_divide,
            _ => 0,
        };
        // Unaligned or partial reads: return the requested slice.
        match size {
            1 => (value >> ((offset & 3) * 8)) & 0xFF,
            2 => (value >> ((offset & 2) * 8)) & 0xFFFF,
            _ => value,
        }
    }

    pub fn write(&mut self, offset: u64, size: u8, value: u32) {
        if size != 4 {
            return;
        }
        match offset {
            LAPIC_SVR => self.svr = value,
            LAPIC_EOI => {}
            LAPIC_LVT_TIMER => self.lvt_timer = value,
            LAPIC_LVT_THERMAL | LAPIC_LVT_PERF | LAPIC_LVT_LINT0 | LAPIC_LVT_LINT1
            | LAPIC_LVT_ERROR => {}
            LAPIC_TIMER_INITIAL => {
                self.timer_initial = value;
                self.timer_current = value;
                self.timer_fractional = 0;
            }
            LAPIC_TIMER_DIVIDE => self.timer_divide = value & 0b1011,
            LAPIC_TPR | LAPIC_ESR | LAPIC_ICR_LO | LAPIC_ICR_HI | LAPIC_LDR | LAPIC_DFR => {}
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lapic() -> (LocalApic, Arc<Mutex<VecDeque<u8>>>) {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        (LocalApic::new(Arc::clone(&queue)), queue)
    }

    #[test]
    fn reports_version_and_id() {
        let (mut lapic, _) = lapic();
        assert_eq!(lapic.read(LAPIC_VERSION, 4), 0x0005_0014);
        assert_eq!(lapic.read(LAPIC_ID, 4), 0);
    }

    #[test]
    fn timer_queues_the_vector_on_expiry() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_SVR, 4, 0x1FF);
        lapic.write(LAPIC_LVT_TIMER, 4, 0x20); // periodic, vector 0x20, unmasked
        lapic.write(LAPIC_TIMER_INITIAL, 4, 3);
        lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b1011); // divide by 1
        lapic.tick();
        lapic.tick();
        assert!(queue.lock().unwrap().is_empty());
        lapic.tick();
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x20));
        // periodic reload
        lapic.tick();
        lapic.tick();
        lapic.tick();
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x20));
    }

    #[test]
    fn masked_timer_does_not_fire() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_SVR, 4, 0x1FF);
        lapic.write(LAPIC_LVT_TIMER, 4, 0x0001_0020); // masked
        lapic.write(LAPIC_TIMER_INITIAL, 4, 2);
        lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b1011);
        lapic.tick();
        lapic.tick();
        assert!(queue.lock().unwrap().is_empty());
    }
}
