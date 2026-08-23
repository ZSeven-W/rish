//! Diagnostic boot runner: loads the real pinned bzImage and initramfs,
//! executes the pure-Rust interpreter, and reports the first gap (missing
//! instruction, guest fault, or halt) together with console output.
//!
//! Checkpointing: --checkpoint writes machine state (RAM plus CPU registers)
//! either periodically (--checkpoint-every) or at the gap; --resume replays
//! that state without re-running the decompressor.
//!
//! --watch ADDRESS reports every change to the eight bytes at a physical
//! address, which is how a clobbered kernel structure is traced back to the
//! instruction that wrote it. Watches cost a memory read per instruction
//! each, so they are off unless asked for.

use std::{env, fs, io::Write as _, process::ExitCode, time::Instant};

use rish_softvm_core::bzimage::{self, BootParams};
use rish_softvm_core::{Cpu, CpuError};

#[path = "boot_state/mod.rs"]
mod boot_state;

/// Command line the pinned diagnostic guest boots with.
const DEFAULT_COMMAND_LINE: &str = "console=ttyS0,115200n8 console=ttyS1,115200n8 \
rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all \
earlyprintk=serial,ttyS0,115200";

struct Options {
    kernel: Option<String>,
    initrd: Option<String>,
    cmdline: String,
    memory_mib: usize,
    steps: u64,
    progress_every: u64,
    checkpoint: Option<String>,
    checkpoint_every: u64,
    resume: Option<String>,
    trace: usize,
    io_log: usize,
    watches: Vec<u64>,
    dumps: Vec<u64>,
    stop_at: Option<u64>,
    break_rip: Option<u64>,
    stop_when_rsp_in: Option<(u64, u64)>,
    stop_on_marker: bool,
}

fn parse_args() -> Result<Options, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut options = Options {
        kernel: None,
        initrd: None,
        cmdline: String::from(DEFAULT_COMMAND_LINE),
        memory_mib: 1024,
        steps: 200_000_000,
        progress_every: 100_000_000,
        checkpoint: None,
        checkpoint_every: 0,
        resume: None,
        trace: 0,
        io_log: 0,
        watches: Vec::new(),
        dumps: Vec::new(),
        stop_at: None,
        break_rip: None,
        stop_when_rsp_in: None,
        stop_on_marker: true,
    };
    let mut index = 0;
    while index < args.len() {
        let next = |index: &mut usize, label: &str| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("{label} needs a value"))
        };
        match args[index].as_str() {
            "--kernel" => options.kernel = Some(next(&mut index, "--kernel")?),
            "--initrd" => options.initrd = Some(next(&mut index, "--initrd")?),
            "--cmdline" => options.cmdline = next(&mut index, "--cmdline")?,
            "--memory-mib" => {
                options.memory_mib =
                    parse_number(&next(&mut index, "--memory-mib")?, "--memory-mib")? as usize;
            }
            "--steps" => options.steps = parse_number(&next(&mut index, "--steps")?, "--steps")?,
            "--progress-every" => {
                options.progress_every =
                    parse_number(&next(&mut index, "--progress-every")?, "--progress-every")?;
            }
            "--checkpoint" => options.checkpoint = Some(next(&mut index, "--checkpoint")?),
            "--checkpoint-every" => {
                options.checkpoint_every = parse_number(
                    &next(&mut index, "--checkpoint-every")?,
                    "--checkpoint-every",
                )?;
            }
            "--resume" => options.resume = Some(next(&mut index, "--resume")?),
            "--trace" => {
                options.trace = parse_number(&next(&mut index, "--trace")?, "--trace")? as usize;
            }
            "--io-log" => {
                options.io_log = parse_number(&next(&mut index, "--io-log")?, "--io-log")? as usize;
            }
            "--watch" => {
                let text = next(&mut index, "--watch")?;
                options.watches.push(parse_address(&text)?);
            }
            "--dump" => {
                let text = next(&mut index, "--dump")?;
                options.dumps.push(parse_address(&text)?);
            }
            "--stop-at" => {
                options.stop_at = Some(parse_number(&next(&mut index, "--stop-at")?, "--stop-at")?);
            }
            "--break-rip" => {
                let text = next(&mut index, "--break-rip")?;
                options.break_rip = Some(parse_address(&text)?);
            }
            "--stop-when-rsp-in" => {
                let text = next(&mut index, "--stop-when-rsp-in")?;
                let (low, high) = text
                    .split_once(':')
                    .ok_or("--stop-when-rsp-in needs LOW:HIGH")?;
                options.stop_when_rsp_in = Some((parse_address(low)?, parse_address(high)?));
            }
            "--run-past-marker" => options.stop_on_marker = false,
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    Ok(options)
}

fn parse_number(text: &str, label: &str) -> Result<u64, String> {
    text.replace('_', "")
        .parse()
        .map_err(|error: std::num::ParseIntError| format!("{label}: {error}"))
}

fn parse_address(text: &str) -> Result<u64, String> {
    let trimmed = text.trim_start_matches("0x");
    u64::from_str_radix(trimmed, 16).map_err(|error| format!("--watch {text}: {error}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("boot: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Serial marker the diagnostic guest prints once it has mounted its
/// filesystems.
const BOOT_OK: &[u8] = b"RISH_X86_64_BOOT_OK";
const BOOT_FAILED: &[u8] = b"RISH_X86_64_BOOT_FAILED";

fn run() -> Result<u8, String> {
    let options = parse_args()?;
    let mut cpu = load(&options)?;
    cpu.set_trace_capacity(options.trace);
    cpu.set_io_log_capacity(options.io_log);

    let mut console = Console::default();
    let mut watches = options
        .watches
        .iter()
        .map(|address| (*address, [0_u8; 8]))
        .collect::<Vec<_>>();
    for (address, value) in watches.iter_mut() {
        let _ = cpu.memory.read(*address, value);
    }

    let started = Instant::now();
    let report_dumps = |cpu: &Cpu| {
        for address in &options.dumps {
            dump_physical(cpu, *address, 256);
        }
    };
    let progress_every = options.progress_every.max(1);
    let mut last_report = cpu.regs.instructions_retired;
    let mut last_checkpoint = cpu.regs.instructions_retired;
    for _ in 0..options.steps {
        for (address, previous) in watches.iter_mut() {
            let mut current = [0_u8; 8];
            if cpu.memory.read(*address, &mut current).is_ok() && current != *previous {
                eprintln!(
                    "watch: {address:#x} changed to {} at rip={:#x} after {} instructions",
                    hex(&current),
                    cpu.regs.rip,
                    cpu.regs.instructions_retired
                );
                *previous = current;
            }
        }
        if let Err(error) = cpu.step() {
            console.absorb(&mut cpu);
            save_checkpoint(&options, &cpu)?;
            report_gap(&error, &cpu, &console);
            report_dumps(&cpu);
            summarize(&cpu, started);
            return Ok(1);
        }
        if options.break_rip == Some(cpu.regs.rip) {
            console.absorb(&mut cpu);
            console.flush();
            println!(
                "break: rip={:#x} after {} instructions",
                cpu.regs.rip, cpu.regs.instructions_retired
            );
            report_machine_state(&cpu);
            write_trace(&cpu);
            report_dumps(&cpu);
            summarize(&cpu, started);
            return Ok(1);
        }
        if let Some((low, high)) = options.stop_when_rsp_in {
            let rsp = cpu.regs.rsp();
            if rsp >= low && rsp < high {
                console.absorb(&mut cpu);
                console.flush();
                println!(
                    "stack pointer entered {low:#x}..{high:#x}: rsp={rsp:#x} rip={:#x} after {} instructions",
                    cpu.regs.rip, cpu.regs.instructions_retired
                );
                report_machine_state(&cpu);
                write_trace(&cpu);
                report_dumps(&cpu);
                summarize(&cpu, started);
                return Ok(1);
            }
        }
        let retired = cpu.regs.instructions_retired;
        if options.stop_at == Some(retired) {
            console.absorb(&mut cpu);
            console.flush();
            save_checkpoint(&options, &cpu)?;
            println!("stopped at {retired} instructions, rip={:#x}", cpu.regs.rip);
            report_machine_state(&cpu);
            // Dump the IDT gates around the ISA timer vector.
            for vector in [0x20_u64, 0x2f, 0x30, 0x31, 0x80] {
                let address = cpu.regs.idt_base.wrapping_add(vector * 16);
                match cpu.translate(address, rish_softvm_core::arch::paging::AccessKind::Read) {
                    Ok(physical) => {
                        let low = cpu.memory.read_u64(physical).unwrap_or(0);
                        let high = cpu.memory.read_u64(physical + 8).unwrap_or(0);
                        println!("idt[{vector:#x}] @ {physical:#x}: {low:#018x} {high:#018x}");
                    }
                    Err(error) => println!("idt[{vector:#x}]: translate failed: {error:?}"),
                }
            }
            write_trace(&cpu);
            report_dumps(&cpu);
            summarize(&cpu, started);
            return Ok(0);
        }
        if retired - last_report >= progress_every {
            last_report = retired;
            console.absorb(&mut cpu);
            console.flush();
            let seconds = started.elapsed().as_secs_f64().max(1e-9);
            eprintln!(
                "progress: {retired} instructions, rip={:#x}, {:.2}M inst/s",
                cpu.regs.rip,
                retired as f64 / seconds / 1e6
            );
        }
        if options.checkpoint_every != 0
            && options.checkpoint.is_some()
            && retired - last_checkpoint >= options.checkpoint_every
        {
            last_checkpoint = retired;
            save_checkpoint(&options, &cpu)?;
            eprintln!("checkpoint: saved at {retired} instructions");
        }
        if console.absorb(&mut cpu) {
            console.flush();
            if console.saw_failure {
                println!("guest reported {}", String::from_utf8_lossy(BOOT_FAILED));
                summarize(&cpu, started);
                return Ok(1);
            }
            if console.saw_marker && options.stop_on_marker {
                println!("guest reached {}", String::from_utf8_lossy(BOOT_OK));
                save_checkpoint(&options, &cpu)?;
                summarize(&cpu, started);
                return Ok(0);
            }
        }
    }
    console.absorb(&mut cpu);
    console.flush();
    save_checkpoint(&options, &cpu)?;
    println!(
        "budget exhausted after {} instructions without a gap",
        options.steps
    );
    report_dumps(&cpu);
    summarize(&cpu, started);
    write_trace(&cpu);
    Ok(0)
}

fn load(options: &Options) -> Result<Cpu, String> {
    let memory_mib = options.memory_mib;
    if let Some(path) = &options.resume {
        println!("boot: pure-Rust x86_64 interpreter diagnostics (resume)");
        println!("resume: {path}");
        let mut cpu = Cpu::new(memory_mib, 0).map_err(|error| error.to_string())?;
        let file = fs::File::open(path).map_err(|error| error.to_string())?;
        let mut reader = std::io::BufReader::new(file);
        let state_mib =
            boot_state::restore(&mut cpu, &mut reader).map_err(|error| error.to_string())?;
        println!(
            "restored {state_mib} MiB guest at rip={:#x} after {} instructions",
            cpu.regs.rip, cpu.regs.instructions_retired
        );
        return Ok(cpu);
    }
    let kernel_path = options
        .kernel
        .as_deref()
        .ok_or("--kernel is required (or use --resume)")?;
    let kernel = fs::read(kernel_path).map_err(|error| error.to_string())?;
    let initrd = match &options.initrd {
        Some(path) => Some(fs::read(path).map_err(|error| error.to_string())?),
        None => None,
    };
    println!("boot: pure-Rust x86_64 interpreter diagnostics");
    println!("kernel: {kernel_path} ({} bytes)", kernel.len());
    if let Some(path) = &options.initrd {
        println!(
            "initrd: {path} ({} bytes)",
            initrd.as_ref().map_or(0, Vec::len)
        );
    }
    println!(
        "memory: {memory_mib} MiB, budget: {} instructions",
        options.steps
    );
    let mut cpu = Cpu::new(memory_mib, 0).map_err(|error| error.to_string())?;
    bzimage::load(
        &mut cpu,
        &kernel,
        initrd.as_deref(),
        &BootParams {
            command_line: options.cmdline.clone(),
            memory_mib,
        },
    )
    .map_err(|error| error.to_string())?;
    println!(
        "entered long mode at {:#x}, rsi(boot_params)={:#x}, cr3={:#x}",
        cpu.regs.rip,
        cpu.regs.gpr(rish_softvm_core::arch::registers::index::RSI),
        cpu.regs.cr3
    );
    Ok(cpu)
}

/// Prints a physical memory region as hex plus printable ASCII, which is how
/// a guest-side buffer such as the panic message is read back.
fn dump_physical(cpu: &Cpu, base: u64, length: usize) {
    // Treat the address as virtual and translate it under the current CR3;
    // fall back to a physical read if translation fails (paging off).
    let physical = cpu
        .translate(base, rish_softvm_core::arch::paging::AccessKind::Read)
        .unwrap_or(base);
    let mut buffer = vec![0_u8; length];
    if cpu.memory.read(physical, &mut buffer).is_err() {
        eprintln!("dump {base:#x} (phys {physical:#x}): unreadable");
        return;
    }
    let text: String = buffer
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            }
        })
        .collect();
    eprintln!("dump {base:#x}: {text}");
    for (index, chunk) in buffer.chunks(32).enumerate() {
        eprintln!("  {:#x}: {}", base + index as u64 * 32, hex(chunk));
    }
}

fn summarize(cpu: &Cpu, started: Instant) {
    let seconds = started.elapsed().as_secs_f64().max(1e-9);
    let ((tlb_hits, tlb_misses), (decode_hits, decode_misses)) = cpu.cache_counters();
    eprintln!(
        "summary: {} instructions in {seconds:.1}s ({:.2}M inst/s), tlb {:.1}% hit, decode {:.1}% hit",
        cpu.regs.instructions_retired,
        cpu.regs.instructions_retired as f64 / seconds / 1e6,
        rate(tlb_hits, tlb_misses),
        rate(decode_hits, decode_misses),
    );
    eprintln!(
        "summary: {} interrupts, {} exceptions, pic initialized={} master mask={:#04x}, irq lines={:#06x}",
        cpu.interrupts_delivered,
        cpu.exceptions_raised,
        cpu.pic.initialized(),
        cpu.pic.master_mask(),
        cpu.lines.asserted,
    );
    write_io_log(cpu);
    report_faults(cpu);
}

/// Prints the recent exception and interrupt history, which is usually the
/// shortest path from a guest panic to the interpreter gap that caused it.
fn report_faults(cpu: &Cpu) {
    if cpu.fault_log.is_empty() {
        return;
    }
    eprintln!("recent faults (newest last):");
    for event in &cpu.fault_log {
        let kind = if event.is_exception {
            "exception"
        } else {
            "interrupt"
        };
        eprintln!(
            "  {:>14} {kind} vector={:#04x} error={:#06x} rip={:#x} cr2={:#x}",
            event.retired, event.vector, event.error_code, event.rip, event.cr2
        );
    }
}

fn write_io_log(cpu: &Cpu) {
    if cpu.io_log.is_empty() {
        return;
    }
    let mut text = String::new();
    for event in &cpu.io_log {
        let direction = if event.write { "out" } else { "in " };
        text.push_str(&format!(
            "{:>14} {direction} port={:#06x} size={} value={:#x}\n",
            event.retired, event.port, event.size, event.value
        ));
    }
    let path = "/tmp/rish-boot-io.txt";
    match fs::write(path, text) {
        Ok(()) => eprintln!("io-log: wrote {} events to {path}", cpu.io_log.len()),
        Err(error) => eprintln!("io-log: could not write {path}: {error}"),
    }
}

fn rate(hits: u64, misses: u64) -> f64 {
    let total = hits + misses;
    if total == 0 {
        0.0
    } else {
        hits as f64 * 100.0 / total as f64
    }
}

/// Console accumulator that also watches for the guest's boot markers.
#[derive(Default)]
struct Console {
    pending: Vec<u8>,
    tail: Vec<u8>,
    saw_marker: bool,
    saw_failure: bool,
}

impl Console {
    /// Drains the guest UART. Returns true when a boot marker appeared.
    fn absorb(&mut self, cpu: &mut Cpu) -> bool {
        let output = cpu.uart_console.drain_output();
        if output.is_empty() {
            return false;
        }
        self.pending.extend_from_slice(&output);
        self.tail.extend_from_slice(&output);
        // Keep just enough context to match a marker split across two drains.
        let keep = BOOT_FAILED.len() * 2;
        if self.tail.len() > keep {
            let excess = self.tail.len() - keep;
            self.tail.drain(..excess);
        }
        let before = (self.saw_marker, self.saw_failure);
        if contains(&self.tail, BOOT_FAILED) {
            self.saw_failure = true;
        } else if contains(&self.tail, BOOT_OK) {
            self.saw_marker = true;
        }
        before != (self.saw_marker, self.saw_failure)
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        print!("{}", String::from_utf8_lossy(&self.pending));
        let _ = std::io::stdout().flush();
        self.pending.clear();
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn save_checkpoint(options: &Options, cpu: &Cpu) -> Result<(), String> {
    let Some(path) = &options.checkpoint else {
        return Ok(());
    };
    // Keep the previous state as a backup so a pre-gap state survives.
    let _ = fs::rename(path, format!("{path}.bak"));
    let file = fs::File::create(path).map_err(|error| error.to_string())?;
    let mut writer = std::io::BufWriter::new(file);
    boot_state::save(cpu, &mut writer).map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

fn write_trace(cpu: &Cpu) {
    if cpu.trace.is_empty() {
        return;
    }
    let mut text = String::new();
    for entry in &cpu.trace {
        text.push_str(&format_trace(entry));
        text.push('\n');
    }
    let path = "/tmp/rish-boot-trace.txt";
    match fs::write(path, text) {
        Ok(()) => eprintln!("trace: wrote {} entries to {path}", cpu.trace.len()),
        Err(error) => eprintln!("trace: could not write {path}: {error}"),
    }
}

fn format_trace(entry: &rish_softvm_core::cpu::TraceEntry) -> String {
    let (rip, rax, rcx, rdx, rbx, rsi, rdi, rbp, rsp, r8, r9, r10, r11, r12, r13, r14, r15, bytes) =
        entry;
    format!(
        "  {rip:#x}: rax={rax:#x} rcx={rcx:#x} rdx={rdx:#x} rbx={rbx:#x} rsi={rsi:#x} \
rdi={rdi:#x} rbp={rbp:#x} rsp={rsp:#x} r8={r8:#x} r9={r9:#x} r10={r10:#x} r11={r11:#x} \
r12={r12:#x} r13={r13:#x} r14={r14:#x} r15={r15:#x} bytes={}",
        hex(bytes)
    )
}

fn report_gap(error: &CpuError, cpu: &Cpu, console: &Console) {
    if !cpu.trace.is_empty() {
        println!("last instructions:");
        for entry in cpu
            .trace
            .iter()
            .rev()
            .take(32)
            .collect::<Vec<_>>()
            .iter()
            .rev()
        {
            println!("{}", format_trace(entry));
        }
        write_trace(cpu);
    }
    report_machine_state(cpu);
    if let Some(linear) = faulting_linear(error) {
        println!("page walk for faulting linear {linear:#x}:");
        walk_tables(cpu, linear);
    }
    println!("gpr dump:");
    for (i, name) in [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ]
    .iter()
    .enumerate()
    {
        println!("  {name} = {:#x}", cpu.regs.gpr[i]);
    }
    println!("memory at fault rip - 0x40:");
    dump_region(cpu, cpu.regs.rip.wrapping_sub(0x40), 0x80);
    if !console.pending.is_empty() {
        print!("{}", String::from_utf8_lossy(&console.pending));
    }
    println!(
        "gap after {} instructions at rip={:#x}",
        cpu.regs.instructions_retired, cpu.regs.rip
    );
    match error {
        CpuError::UnimplementedInstruction {
            code,
            address,
            bytes,
        } => {
            println!(
                "  unimplemented: {code} at {address:#x} bytes={}",
                hex(bytes)
            );
        }
        CpuError::GuestFault(message) => println!("  guest fault: {message}"),
        CpuError::TripleFault => println!("  triple fault"),
        CpuError::Halted => println!("  guest halted"),
        other => println!("  {other}"),
    }
}

/// Prints the control registers, descriptor tables, and segment bases, which
/// together say what mode the guest is in and where its per-CPU data lives.
fn report_machine_state(cpu: &Cpu) {
    println!(
        "cr0={:#x} cr3={:#x} cr4={:#x} efer={:#x} cpl={}",
        cpu.regs.cr0.bits(),
        cpu.regs.cr3,
        cpu.regs.cr4.bits(),
        cpu.regs.efer.bits(),
        cpu.regs.cpl()
    );
    println!(
        "cs={:#x} ss={:#x} tr={:#x} tr_base={:#x} idt={:#x}/{:#x} gdt={:#x}/{:#x}",
        cpu.regs.cs.selector.0,
        cpu.regs.ss.selector.0,
        cpu.regs.tr.selector.0,
        cpu.regs.tr_base,
        cpu.regs.idt_base,
        cpu.regs.idt_limit,
        cpu.regs.gdt_base,
        cpu.regs.gdt_limit,
    );
    println!(
        "fs.base={:#x} gs.base={:#x} kernel_gs_base={:#x} tsc={}",
        cpu.regs.fs.base, cpu.regs.gs.base, cpu.kernel_gs_base, cpu.tsc
    );
    println!(
        "rsp={:#x} rbp={:#x} rax={:#x}",
        cpu.regs.rsp(),
        cpu.regs.rbp(),
        cpu.regs.gpr(rish_softvm_core::arch::registers::index::RAX)
    );
}

fn faulting_linear(error: &CpuError) -> Option<u64> {
    match error {
        CpuError::PageFault { linear, .. } => Some(*linear),
        _ => None,
    }
}

fn walk_tables(cpu: &Cpu, linear: u64) {
    let cr3 = cpu.regs.cr3 & 0x000F_FFFF_FFFF_F000;
    let read_entry = |address: u64| -> u64 { cpu.memory.read_u64(address).unwrap_or(0) };
    let pml4_index = (linear >> 39) & 0x1FF;
    let pml4e = read_entry(cr3 + pml4_index * 8);
    println!("  pml4[{pml4_index}] = {pml4e:#x}");
    if pml4e & 1 == 0 {
        println!("  (pml4 entry not present)");
        return;
    }
    let pdpt_index = (linear >> 30) & 0x1FF;
    let pdpte = read_entry((pml4e & 0x000F_FFFF_FFFF_F000) + pdpt_index * 8);
    println!("  pdpt[{pdpt_index}] = {pdpte:#x}");
    if pdpte & 1 == 0 {
        return;
    }
    if pdpte & 0x80 != 0 {
        println!(
            "  (1 GiB page -> physical {:#x})",
            (pdpte & !0x3FFF_FFFF) | (linear & 0x3FFF_FFFF)
        );
        return;
    }
    let pd_index = (linear >> 21) & 0x1FF;
    let pde = read_entry((pdpte & 0x000F_FFFF_FFFF_F000) + pd_index * 8);
    println!("  pd[{pd_index}] = {pde:#x}");
    if pde & 1 == 0 {
        return;
    }
    if pde & 0x80 != 0 {
        println!(
            "  (2 MiB page -> physical {:#x})",
            (pde & !0x1F_FFFF) | (linear & 0x1F_FFFF)
        );
        return;
    }
    let pt_index = (linear >> 12) & 0x1FF;
    let pte = read_entry((pde & 0x000F_FFFF_FFFF_F000) + pt_index * 8);
    println!("  pt[{pt_index}] = {pte:#x}");
    if pte & 1 == 0 {
        return;
    }
    println!(
        "  -> physical {:#x}",
        (pte & 0x000F_FFFF_FFFF_F000) | (linear & 0xFFF)
    );
}

fn dump_region(cpu: &Cpu, base: u64, length: usize) {
    let mut buffer = vec![0_u8; length];
    let physical = cpu
        .translate(base, rish_softvm_core::arch::paging::AccessKind::Read)
        .unwrap_or(base);
    if cpu.memory.read(physical, &mut buffer).is_err() {
        println!("  (unreadable)");
        return;
    }
    for (index, chunk) in buffer.chunks(16).enumerate() {
        println!("  {:#x}: {}", base + index as u64 * 16, hex(chunk));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
