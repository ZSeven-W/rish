//! Local APIC MMIO at 0xFEE00000.
//!
//! The register surface the x86_64 kernel probes at boot: ID, version, task
//! priority, the LVT entries, the in-service and request bitmaps, EOI, the
//! interrupt command register (self-IPI included), and a divided countdown
//! timer with both one-shot and periodic modes.
//!
//! Delivery is priority-ordered through the request and in-service bitmaps,
//! so a handler that has not written EOI cannot be preempted by an equal or
//! lower priority vector, which is what the Linux entry code assumes.

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
const LAPIC_RRD: u64 = 0xC0;
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

/// LVT mask bit.
const LVT_MASKED: u32 = 1 << 16;
/// LVT timer mode field: 0 one-shot, 1 periodic, 2 TSC deadline.
const LVT_TIMER_MODE: u32 = 0b11 << 17;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
/// Spurious-interrupt vector register APIC software enable.
const SVR_ENABLED: u32 = 1 << 8;
/// ICR delivery shorthand field: 0b01 selects "self".
const ICR_SHORTHAND_SELF: u32 = 0b01 << 18;
/// ICR delivery mode field, 0 = fixed.
const ICR_DELIVERY_MODE: u32 = 0b111 << 8;
/// ICR delivery-status bit, always reported idle by this model.
const ICR_DELIVERY_PENDING: u32 = 1 << 12;

/// Maximum vectors queued for the CPU before new ones are dropped. A queue
/// this deep only fills if the guest never enables interrupts again.
const QUEUE_LIMIT: usize = 64;

pub struct LocalApic {
    id: u32,
    svr: u32,
    tpr: u32,
    ldr: u32,
    dfr: u32,
    esr: u32,
    icr_lo: u32,
    icr_hi: u32,
    lvt_timer: u32,
    lvt_thermal: u32,
    lvt_perf: u32,
    lvt_lint0: u32,
    lvt_lint1: u32,
    lvt_error: u32,
    timer_initial: u32,
    timer_current: u32,
    timer_divide: u32,
    timer_fractional: u64,
    /// Interrupt request register: vectors accepted but not yet delivered.
    irr: [u32; 8],
    /// In-service register: vectors delivered and awaiting EOI.
    isr: [u32; 8],
    interrupt_queue: Arc<Mutex<VecDeque<u8>>>,
}

impl LocalApic {
    #[must_use]
    pub fn new(interrupt_queue: Arc<Mutex<VecDeque<u8>>>) -> Self {
        Self {
            id: 0,
            svr: 0xFF,
            tpr: 0,
            ldr: 0,
            dfr: 0xFFFF_FFFF,
            esr: 0,
            icr_lo: 0,
            icr_hi: 0,
            // Every LVT entry comes out of reset masked.
            lvt_timer: LVT_MASKED,
            lvt_thermal: LVT_MASKED,
            lvt_perf: LVT_MASKED,
            lvt_lint0: LVT_MASKED,
            lvt_lint1: LVT_MASKED,
            lvt_error: LVT_MASKED,
            timer_initial: 0,
            timer_current: 0,
            timer_divide: 0,
            timer_fractional: 0,
            irr: [0; 8],
            isr: [0; 8],
            interrupt_queue,
        }
    }

    #[must_use]
    pub fn software_enabled(&self) -> bool {
        self.svr & SVR_ENABLED != 0
    }

    /// Advances the countdown by a batch of guest instructions and raises the
    /// timer vector when it expires.
    pub fn tick(&mut self, instructions: u32) {
        self.advance_timer(instructions);
        self.dispatch_ready();
    }

    fn advance_timer(&mut self, instructions: u32) {
        if self.timer_initial == 0 || self.timer_current == 0 {
            return;
        }
        let divide = divisor(self.timer_divide);
        self.timer_fractional = self
            .timer_fractional
            .saturating_add(u64::from(instructions));
        let elapsed = self.timer_fractional / divide;
        if elapsed == 0 {
            return;
        }
        self.timer_fractional -= elapsed * divide;
        let periodic = self.lvt_timer & LVT_TIMER_MODE == LVT_TIMER_PERIODIC;
        let remaining = u64::from(self.timer_current);
        if elapsed < remaining {
            self.timer_current -= elapsed as u32;
            return;
        }
        if periodic {
            // Reload and account for however many whole periods elapsed.
            let period = u64::from(self.timer_initial);
            let overshoot = elapsed - remaining;
            self.timer_current = (period - (overshoot % period)) as u32;
        } else {
            // One-shot: the counter stays at zero until reprogrammed.
            self.timer_current = 0;
        }
        self.raise_lvt(self.lvt_timer);
    }

    fn raise_lvt(&mut self, entry: u32) {
        if entry & LVT_MASKED != 0 || !self.software_enabled() {
            return;
        }
        let vector = (entry & 0xFF) as u8;
        if vector >= 16 {
            self.request(vector);
        }
    }

    /// Marks a vector as requested.
    pub fn request(&mut self, vector: u8) {
        let index = usize::from(vector) / 32;
        self.irr[index] |= 1 << (u32::from(vector) % 32);
    }

    /// Moves the highest-priority requested vector into service and hands it
    /// to the CPU, provided it outranks both the task priority and anything
    /// already in service.
    fn dispatch_ready(&mut self) {
        if !self.software_enabled() {
            return;
        }
        let Some(vector) = self.highest(&self.irr) else {
            return;
        };
        let priority = u32::from(vector) >> 4;
        if priority <= (self.tpr >> 4) {
            return;
        }
        if let Some(in_service) = self.highest(&self.isr) {
            if u32::from(in_service) >> 4 >= priority {
                return;
            }
        }
        let index = usize::from(vector) / 32;
        let bit = 1 << (u32::from(vector) % 32);
        self.irr[index] &= !bit;
        self.isr[index] |= bit;
        if let Ok(mut queue) = self.interrupt_queue.lock() {
            if queue.len() < QUEUE_LIMIT {
                queue.push_back(vector);
            }
        }
    }

    fn highest(&self, bitmap: &[u32; 8]) -> Option<u8> {
        for index in (0..8).rev() {
            let word = bitmap[index];
            if word != 0 {
                let bit = 31 - word.leading_zeros();
                return Some((index as u32 * 32 + bit) as u8);
            }
        }
        None
    }

    /// Clears the highest in-service vector, as a write to EOI does, and
    /// reports which vector retired so a level-triggered I/O APIC entry can
    /// drop its remote-IRR bit.
    fn end_of_interrupt(&mut self) -> Option<u8> {
        let retired = self.highest(&self.isr);
        if let Some(vector) = retired {
            self.isr[usize::from(vector) / 32] &= !(1 << (u32::from(vector) % 32));
        }
        self.dispatch_ready();
        retired
    }

    /// Returns the timer vector queue shared with the CPU.
    #[must_use]
    pub fn interrupt_queue(&self) -> Arc<Mutex<VecDeque<u8>>> {
        Arc::clone(&self.interrupt_queue)
    }

    pub fn read(&mut self, offset: u64, size: u8) -> u32 {
        let aligned = offset & !0x3;
        let value = match aligned {
            LAPIC_ID => self.id << 24,
            // Version 0x14 with six LVT entries (max LVT index 5).
            LAPIC_VERSION => 0x0005_0014,
            LAPIC_TPR => self.tpr,
            LAPIC_APR => 0,
            LAPIC_PPR => self.processor_priority(),
            LAPIC_EOI | LAPIC_RRD => 0,
            LAPIC_LDR => self.ldr,
            LAPIC_DFR => self.dfr,
            LAPIC_SVR => self.svr,
            LAPIC_ISR_BASE..=0x17F => self.isr[((aligned - LAPIC_ISR_BASE) / 16) as usize],
            // The trigger-mode register is edge-only in this model.
            LAPIC_TMR_BASE..=0x1FF => 0,
            LAPIC_IRR_BASE..=0x27F => self.irr[((aligned - LAPIC_IRR_BASE) / 16) as usize],
            LAPIC_ESR => self.esr,
            LAPIC_ICR_LO => self.icr_lo & !ICR_DELIVERY_PENDING,
            LAPIC_ICR_HI => self.icr_hi,
            LAPIC_LVT_TIMER => self.lvt_timer,
            LAPIC_LVT_THERMAL => self.lvt_thermal,
            LAPIC_LVT_PERF => self.lvt_perf,
            LAPIC_LVT_LINT0 => self.lvt_lint0,
            LAPIC_LVT_LINT1 => self.lvt_lint1,
            LAPIC_LVT_ERROR => self.lvt_error,
            LAPIC_TIMER_INITIAL => self.timer_initial,
            LAPIC_TIMER_CURRENT => self.timer_current,
            LAPIC_TIMER_DIVIDE => self.timer_divide,
            _ => 0,
        };
        // Unaligned or partial reads return the requested slice.
        match size {
            1 => (value >> ((offset & 3) * 8)) & 0xFF,
            2 => (value >> ((offset & 2) * 8)) & 0xFFFF,
            _ => value,
        }
    }

    /// Processor priority: the greater of the task priority and the priority
    /// of the highest in-service vector.
    fn processor_priority(&self) -> u32 {
        let in_service = self
            .highest(&self.isr)
            .map_or(0, |v| (u32::from(v) >> 4) << 4);
        let task = self.tpr & 0xF0;
        task.max(in_service)
    }

    /// Handles a register write. Returns the vector retired by an EOI, if
    /// this write was one.
    pub fn write(&mut self, offset: u64, size: u8, value: u32) -> Option<u8> {
        if size != 4 {
            return None;
        }
        match offset {
            LAPIC_ID => self.id = value >> 24,
            LAPIC_TPR => {
                self.tpr = value & 0xFF;
                self.dispatch_ready();
            }
            LAPIC_EOI => return self.end_of_interrupt(),
            LAPIC_LDR => self.ldr = value & 0xFF00_0000,
            LAPIC_DFR => self.dfr = value | 0x0FFF_FFFF,
            LAPIC_SVR => {
                self.svr = value;
                if self.software_enabled() {
                    self.dispatch_ready();
                }
            }
            LAPIC_ESR => self.esr = 0,
            LAPIC_ICR_HI => self.icr_hi = value,
            LAPIC_ICR_LO => self.send_ipi(value),
            LAPIC_LVT_TIMER => self.lvt_timer = value,
            LAPIC_LVT_THERMAL => self.lvt_thermal = value,
            LAPIC_LVT_PERF => self.lvt_perf = value,
            LAPIC_LVT_LINT0 => self.lvt_lint0 = value,
            LAPIC_LVT_LINT1 => self.lvt_lint1 = value,
            LAPIC_LVT_ERROR => self.lvt_error = value,
            LAPIC_TIMER_INITIAL => {
                self.timer_initial = value;
                self.timer_current = value;
                self.timer_fractional = 0;
            }
            LAPIC_TIMER_DIVIDE => self.timer_divide = value & 0b1011,
            _ => {}
        }
        None
    }

    /// Handles a write to the interrupt command register.
    ///
    /// This is a uniprocessor model, so the only delivery that can land is one
    /// the CPU sends to itself: either the self shorthand or a physical
    /// destination that names this APIC.
    fn send_ipi(&mut self, value: u32) {
        self.icr_lo = value;
        let fixed = value & ICR_DELIVERY_MODE == 0;
        if !fixed {
            // INIT, SIPI, NMI and friends have no second CPU to reach.
            return;
        }
        let to_self = value & (0b11 << 18) == ICR_SHORTHAND_SELF
            || (value & (0b11 << 18) == 0 && (self.icr_hi >> 24) == self.id);
        if !to_self {
            return;
        }
        let vector = (value & 0xFF) as u8;
        if vector >= 16 {
            self.request(vector);
            self.dispatch_ready();
        }
    }
}

/// Divide configuration register encoding to a divisor.
fn divisor(configuration: u32) -> u64 {
    match configuration & 0b1011 {
        0b0000 => 2,
        0b0001 => 4,
        0b0010 => 8,
        0b0011 => 16,
        0b1000 => 32,
        0b1001 => 64,
        0b1010 => 128,
        0b1011 => 1,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lapic() -> (LocalApic, Arc<Mutex<VecDeque<u8>>>) {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let mut lapic = LocalApic::new(Arc::clone(&queue));
        lapic.write(LAPIC_SVR, 4, 0x1FF);
        (lapic, queue)
    }

    fn periodic(vector: u8) -> u32 {
        LVT_TIMER_PERIODIC | u32::from(vector)
    }

    #[test]
    fn reports_version_and_id() {
        let (mut lapic, _) = lapic();
        assert_eq!(lapic.read(LAPIC_VERSION, 4), 0x0005_0014);
        assert_eq!(lapic.read(LAPIC_ID, 4), 0);
    }

    #[test]
    fn periodic_timer_reloads_and_keeps_firing() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_LVT_TIMER, 4, periodic(0x20));
        lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b1011); // divide by 1
        lapic.write(LAPIC_TIMER_INITIAL, 4, 3);
        lapic.tick(2);
        assert!(queue.lock().unwrap().is_empty());
        lapic.tick(1);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x20));
        lapic.write(LAPIC_EOI, 4, 0);
        lapic.tick(3);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x20));
    }

    #[test]
    fn one_shot_timer_fires_exactly_once() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_LVT_TIMER, 4, 0x20); // one-shot, unmasked
        lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b1011);
        lapic.write(LAPIC_TIMER_INITIAL, 4, 4);
        lapic.tick(4);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x20));
        lapic.write(LAPIC_EOI, 4, 0);
        lapic.tick(100);
        assert!(queue.lock().unwrap().is_empty());
        assert_eq!(lapic.read(LAPIC_TIMER_CURRENT, 4), 0);
    }

    #[test]
    fn a_batched_tick_matches_the_same_number_of_single_ticks() {
        let (mut batched, batched_queue) = lapic();
        let (mut single, single_queue) = lapic();
        for lapic in [&mut batched, &mut single] {
            lapic.write(LAPIC_LVT_TIMER, 4, periodic(0x30));
            lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b0000); // divide by 2
            lapic.write(LAPIC_TIMER_INITIAL, 4, 5);
        }
        batched.tick(64);
        for _ in 0..64 {
            single.tick(1);
        }
        assert_eq!(
            batched.read(LAPIC_TIMER_CURRENT, 4),
            single.read(LAPIC_TIMER_CURRENT, 4)
        );
        assert!(!batched_queue.lock().unwrap().is_empty());
        assert!(!single_queue.lock().unwrap().is_empty());
    }

    #[test]
    fn masked_timer_does_not_fire() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_LVT_TIMER, 4, LVT_MASKED | 0x20);
        lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b1011);
        lapic.write(LAPIC_TIMER_INITIAL, 4, 2);
        lapic.tick(2);
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn a_software_disabled_apic_delivers_nothing() {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let mut lapic = LocalApic::new(Arc::clone(&queue));
        lapic.write(LAPIC_LVT_TIMER, 4, periodic(0x20));
        lapic.write(LAPIC_TIMER_DIVIDE, 4, 0b1011);
        lapic.write(LAPIC_TIMER_INITIAL, 4, 1);
        lapic.tick(4);
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn in_service_vector_blocks_an_equal_priority_request_until_eoi() {
        let (mut lapic, queue) = lapic();
        lapic.request(0x30);
        lapic.tick(0);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x30));
        // Same priority class (0x3x) must wait for EOI.
        lapic.request(0x31);
        lapic.tick(0);
        assert!(queue.lock().unwrap().is_empty());
        lapic.write(LAPIC_EOI, 4, 0);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x31));
    }

    #[test]
    fn a_higher_priority_request_preempts_one_in_service() {
        let (mut lapic, queue) = lapic();
        lapic.request(0x30);
        lapic.tick(0);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x30));
        lapic.request(0x50);
        lapic.tick(0);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x50));
    }

    #[test]
    fn task_priority_holds_off_lower_vectors() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_TPR, 4, 0x40);
        lapic.request(0x30);
        lapic.tick(0);
        assert!(queue.lock().unwrap().is_empty());
        lapic.write(LAPIC_TPR, 4, 0x00);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0x30));
    }

    #[test]
    fn self_ipi_delivers_its_vector() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_ICR_LO, 4, ICR_SHORTHAND_SELF | 0xF2);
        assert_eq!(queue.lock().unwrap().pop_front(), Some(0xF2));
    }

    #[test]
    fn a_physical_ipi_to_another_apic_is_dropped() {
        let (mut lapic, queue) = lapic();
        lapic.write(LAPIC_ICR_HI, 4, 3 << 24);
        lapic.write(LAPIC_ICR_LO, 4, 0xF2);
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn in_service_register_is_readable_and_cleared_by_eoi() {
        let (mut lapic, _queue) = lapic();
        lapic.request(0x30);
        lapic.tick(0);
        assert_eq!(lapic.read(LAPIC_ISR_BASE + 0x10, 4), 1 << 16);
        assert_eq!(lapic.read(LAPIC_PPR, 4), 0x30);
        lapic.write(LAPIC_EOI, 4, 0);
        assert_eq!(lapic.read(LAPIC_ISR_BASE + 0x10, 4), 0);
        assert_eq!(lapic.read(LAPIC_PPR, 4), 0);
    }
}
