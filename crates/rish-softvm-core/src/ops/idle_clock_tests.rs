use crate::arch::registers::{Efer, RFlags};
use crate::arch::segments::{Descriptor, SegmentSelector};
use crate::devices::acpi_pm::PM_BASE;
use crate::devices::lapic::LAPIC_BASE;
use crate::devices::pit8254::PIT_BASE_FREQUENCY_HZ;
use crate::{Cpu, CpuError};

const CODE: u64 = 0x1000;
const HANDLER: u64 = 0x4000;
const IDLE_TICKS: u64 = 64;
const TICK_NANOSECONDS: u64 = 10;
const TIMER_CURRENT: u64 = LAPIC_BASE + 0x390;

fn cpu(interrupts_enabled: bool) -> Cpu {
    let mut cpu = Cpu::new(1, 0).unwrap();
    cpu.regs.efer |= Efer::LMA;
    cpu.regs.cs = Descriptor::decode(0x00AF_9B00_0000_FFFF).load(SegmentSelector(8));
    cpu.regs.rflags.set(RFlags::IF, interrupts_enabled);
    cpu.regs.rip = CODE;
    cpu.memory.write(CODE, &[0xF4]).unwrap(); // hlt
    cpu
}

fn pm_timer(cpu: &mut Cpu) -> u32 {
    cpu.io_read(PM_BASE + 8, 4).unwrap()
}

fn lapic_countdown(cpu: &mut Cpu, initial: u32) {
    // Divide by one. Leave the LVT masked unless a test needs a wake-up IRQ.
    cpu.memory.write_u32(LAPIC_BASE + 0x3E0, 0xB).unwrap();
    cpu.memory.write_u32(LAPIC_BASE + 0x380, initial).unwrap();
}

#[test]
fn idle_clock_hlt_advances_tsc_without_retiring_instructions() {
    let mut cpu = cpu(true);
    cpu.tsc = 0x1003;
    cpu.step().unwrap();
    assert!(cpu.waiting_for_interrupt);
    assert_eq!(cpu.tsc, 0x1004, "HLT itself retires normally");
    let retired = cpu.regs.instructions_retired;

    for idle_step in 1..=5 {
        cpu.step().unwrap();
        assert_eq!(cpu.tsc, 0x1004 + idle_step * IDLE_TICKS);
        assert_eq!(cpu.regs.instructions_retired, retired);
        assert_eq!(cpu.regs.rip, CODE + 1);
        assert!(cpu.waiting_for_interrupt);
        assert!(!cpu.halted);
    }
}

#[test]
fn idle_clock_pm_timer_advances_and_preserves_its_24_bit_wrap() {
    let mut cpu = cpu(true);
    cpu.tsc = 0x00FF_FFE0 << 2;
    cpu.step().unwrap();
    assert_eq!(pm_timer(&mut cpu), 0x00FF_FFE0);

    for expected in [0x00FF_FFF0, 0, 0x10, 0x20] {
        cpu.step().unwrap();
        assert_eq!(pm_timer(&mut cpu), expected);
    }
    assert_eq!(cpu.regs.instructions_retired, 1);
}

#[test]
fn idle_clock_preserves_pit_and_lapic_tick_rates() {
    let mut cpu = cpu(true);
    lapic_countdown(&mut cpu, 10_000);
    cpu.step().unwrap();
    assert_eq!(cpu.memory.read_u32(TIMER_CURRENT).unwrap(), 10_000);
    assert_eq!(cpu.pit.channel0_count(), u16::MAX);

    for idle_step in 1..=8 {
        cpu.step().unwrap();
        let elapsed = idle_step * IDLE_TICKS;
        let pit_ticks = elapsed * TICK_NANOSECONDS * PIT_BASE_FREQUENCY_HZ / 1_000_000_000;
        assert_eq!(
            cpu.memory.read_u32(TIMER_CURRENT).unwrap(),
            10_000 - elapsed as u32
        );
        assert_eq!(cpu.pit.channel0_count(), u16::MAX - pit_ticks as u16);
    }
}

fn install_timer_interrupt(cpu: &mut Cpu) {
    cpu.regs.gdt_base = 0x2000;
    cpu.regs.gdt_limit = 0xFF;
    cpu.memory.write_u64(0x2008, 0x00AF_9B00_0000_FFFF).unwrap();
    cpu.regs.idt_base = 0x3000;
    cpu.regs.idt_limit = 0xFFF;
    let gate = 0x3000 + 0x30 * 16;
    cpu.memory
        .write_u64(gate, (8 << 16) | 0x8E00_0000_0000 | HANDLER)
        .unwrap();
    cpu.memory.write_u64(gate + 8, 0).unwrap();
    cpu.regs.set_rsp(0x8000);
    cpu.memory.write(HANDLER, &[0x90; 64]).unwrap(); // nop
    cpu.memory.write_u32(LAPIC_BASE + 0xF0, 0x1FF).unwrap();
    cpu.memory.write_u32(LAPIC_BASE + 0x320, 0x30).unwrap();
    lapic_countdown(cpu, 128);
}

#[test]
fn idle_clock_timer_irq_wakes_hlt_and_normal_steps_charge_only_one_tick() {
    let mut cpu = cpu(true);
    install_timer_interrupt(&mut cpu);
    cpu.step().unwrap(); // hlt
    cpu.step().unwrap(); // 64 idle ticks, timer still pending
    assert!(cpu.waiting_for_interrupt);
    cpu.step().unwrap(); // another 64 ticks raises the timer IRQ
    assert!(cpu.waiting_for_interrupt);
    assert_eq!(cpu.memory.read_u32(TIMER_CURRENT).unwrap(), 0);
    let wake_tsc = cpu.tsc;
    let wake_retired = cpu.regs.instructions_retired;

    // The wake-up step delivers the real LAPIC timer IRQ and executes a NOP.
    // Continue across the normal 64-instruction device-service boundary.
    for instructions in 1..=64 {
        cpu.step().unwrap();
        assert_eq!(cpu.tsc, wake_tsc + instructions);
        assert_eq!(cpu.regs.instructions_retired, wake_retired + instructions);
        assert_eq!(cpu.regs.rip, HANDLER + instructions);
        assert_eq!(cpu.interrupts_delivered, 1);
        assert!(!cpu.waiting_for_interrupt);
    }
    assert_eq!(wake_tsc, 1 + 2 * IDLE_TICKS);
}

#[test]
fn idle_clock_interrupts_disabled_halt_does_not_advance_any_clock() {
    let mut cpu = cpu(false);
    lapic_countdown(&mut cpu, 10_000);
    cpu.step().unwrap();
    assert!(cpu.halted);
    assert!(!cpu.waiting_for_interrupt);
    assert_eq!(cpu.tsc, 1);
    let pm_before = pm_timer(&mut cpu);

    for _ in 0..5 {
        assert_eq!(cpu.step(), Err(CpuError::Halted));
        assert_eq!(cpu.tsc, 1);
        assert_eq!(pm_timer(&mut cpu), pm_before);
        assert_eq!(cpu.pit.channel0_count(), u16::MAX);
        assert_eq!(cpu.memory.read_u32(TIMER_CURRENT).unwrap(), 10_000);
        assert_eq!(cpu.regs.instructions_retired, 1);
        assert_eq!(cpu.regs.rip, CODE + 1);
    }
}

#[test]
fn idle_clock_tsc_saturates_consistently_with_the_normal_instruction_path() {
    let mut cpu = cpu(true);
    cpu.tsc = u64::MAX - 32;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.tsc, u64::MAX);
    cpu.step().unwrap();
    assert_eq!(cpu.tsc, u64::MAX);
    assert_eq!(pm_timer(&mut cpu), 0x00FF_FFFF);
    assert_eq!(cpu.regs.instructions_retired, 1);
}
