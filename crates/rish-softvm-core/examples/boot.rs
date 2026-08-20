//! Diagnostic boot runner: loads the real pinned bzImage and initramfs,
//! executes the pure-Rust interpreter, and reports the first gap (missing
//! instruction, guest fault, or halt) together with console output.
//!
//! Checkpointing: --checkpoint writes a pre-kernel machine state (RAM plus
//! CPU registers) either periodically (--checkpoint-every) or at the gap;
//! --resume replays that state without re-running the decompressor.

use std::{env, fs, io::Write as _, process::ExitCode};

use rish_softvm_core::bzimage::{self, BootParams};
use rish_softvm_core::{Cpu, CpuError};

#[path = "boot_state/mod.rs"]
mod boot_state;

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
}

fn parse_args() -> Result<Options, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut options = Options {
        kernel: None,
        initrd: None,
        cmdline: String::from(
            "console=ttyS0,115200n8 console=ttyS1,115200n8 rdinit=/init          panic=-1 oops=panic nokaslr cgroup_no_v1=all nolapic_timer          earlyprintk=serial,ttyS0,115200",
        ),
        memory_mib: 1024,
        steps: 200_000_000,
        progress_every: 100_000,
        checkpoint: None,
        checkpoint_every: 0,
        resume: None,
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
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    Ok(options)
}

fn parse_number(text: &str, label: &str) -> Result<u64, String> {
    text.parse()
        .map_err(|error: std::num::ParseIntError| format!("{label}: {error}"))
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

fn run() -> Result<u8, String> {
    let options = parse_args()?;
    let memory_mib = options.memory_mib;
    let mut cpu = match &options.resume {
        Some(path) => {
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
            cpu
        }
        None => {
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
                println!("initrd: {path}");
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
            cpu
        }
    };

    let mut console = Vec::new();
    let progress_every = options.progress_every.max(1);
    let mut last_report = cpu.regs.instructions_retired;
    let mut last_checkpoint = cpu.regs.instructions_retired;
    let mut watch_prev = [0_u8; 16];
    let _ = cpu.memory.read(0x35bd000, &mut watch_prev);
    let mut watch_addrs: [(&str, u64, [u8; 8]); 12] = [
        ("top_level_pgt", 0x35e3000, [0; 8]),
        ("info0", 0x35df020, [0; 8]),
        ("info8", 0x35df028, [0; 8]),
        ("heap_loop_start", 0x3601698, [0; 8]),
        ("pud_page", 0x35bf000, [0; 8]),
        ("free_mem_end_ptr", 0x35cd408, [0; 8]),
        ("free_mem_ptr", 0x35cd410, [0; 8]),
        ("malloc_ptr", 0x35de458, [0; 8]),
        ("kernel_global_2a40010", 0x2a40010, [0; 8]),
        ("early_pml4_0x111", 0x30e8888, [0; 8]),
        ("fixmap_table_0", 0x3149ea0, [0; 8]),
        ("fixmap_seed", 0x2b227d0, [0; 8]),
    ];
    for slot in watch_addrs.iter_mut() {
        let _ = cpu.memory.read(slot.1, &mut slot.2);
    }
    let mut last_region: Option<u64> = None;
    for _ in 0..options.steps {
        let mut watch_cur = [0_u8; 16];
        if cpu.memory.read(0x35bd000, &mut watch_cur).is_ok() && watch_cur != watch_prev {
            eprintln!(
                "watch: 0x35bd000 changed to {} at rip={:#x} after {} instructions",
                hex(&watch_cur),
                cpu.regs.rip,
                cpu.regs.instructions_retired
            );
            watch_prev = watch_cur;
        }
        for slot in watch_addrs.iter_mut() {
            let mut cur = [0_u8; 8];
            if cpu.memory.read(slot.1, &mut cur).is_ok() && cur != slot.2 {
                eprintln!(
                    "watch: {} ({:#x}) changed to {} at rip={:#x} after {} instructions",
                    slot.0,
                    slot.1,
                    hex(&cur),
                    cpu.regs.rip,
                    cpu.regs.instructions_retired
                );
                slot.2 = cur;
            }
        }
        let region = if cpu.regs.rip < 0x1c0_0000 {
            0
        } else if cpu.regs.rip < 0x300_0000 {
            1
        } else if cpu.regs.rip < 0x400_0000 {
            2
        } else {
            3
        };
        if last_region != Some(region) {
            eprintln!(
                "watch: rip entered region {region} at {:#x} after {} instructions",
                cpu.regs.rip, cpu.regs.instructions_retired
            );
            last_region = Some(region);
        }
        if let Err(error) = cpu.step() {
            console.extend(cpu.uart_console.drain_output());
            save_checkpoint(&options, &cpu)?;
            report_gap(&error, &cpu, &console);
            return Ok(1);
        }
        let retired = cpu.regs.instructions_retired;
        if retired - last_report >= progress_every {
            last_report = retired;
            console.extend(cpu.uart_console.drain_output());
            if !console.is_empty() {
                print_console(&console);
                console.clear();
            }
            eprintln!("progress: {retired} instructions, rip={:#x}", cpu.regs.rip);
        }
        if options.checkpoint_every != 0
            && options.checkpoint.is_some()
            && retired - last_checkpoint >= options.checkpoint_every
        {
            last_checkpoint = retired;
            save_checkpoint(&options, &cpu)?;
            eprintln!("checkpoint: saved at {retired} instructions");
        }
    }
    console.extend(cpu.uart_console.drain_output());
    if !console.is_empty() {
        print_console(&console);
    }
    save_checkpoint(&options, &cpu)?;
    println!(
        "budget exhausted after {} instructions without a gap",
        options.steps
    );
    write_trace(&cpu);
    Ok(0)
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
    let mut trace_text = String::new();
    for (
        rip,
        rax,
        rcx,
        rdx,
        rbx,
        rsi,
        rdi,
        rbp,
        rsp,
        r8,
        r9,
        r10,
        r11,
        r12,
        r13,
        r14,
        r15,
        bytes,
    ) in &cpu.trace
    {
        let line = format!(
            "  {rip:#x}: rax={rax:#x} rcx={rcx:#x} rdx={rdx:#x} rbx={rbx:#x} rsi={rsi:#x} rdi={rdi:#x} rbp={rbp:#x} rsp={rsp:#x} r8={r8:#x} r9={r9:#x} r10={r10:#x} r11={r11:#x} r12={r12:#x} r13={r13:#x} r14={r14:#x} r15={r15:#x} bytes={}",
            hex(bytes)
        );
        trace_text.push_str(&line);
        trace_text.push('\n');
    }
    let _ = fs::write("/tmp/rish-boot-trace.txt", trace_text);
}

fn report_gap(error: &CpuError, cpu: &Cpu, console: &[u8]) {
    println!("last instructions:");
    for (
        rip,
        rax,
        rcx,
        rdx,
        rbx,
        rsi,
        rdi,
        rbp,
        rsp,
        r8,
        r9,
        r10,
        r11,
        r12,
        r13,
        r14,
        r15,
        bytes,
    ) in &cpu.trace
    {
        println!(
            "  {rip:#x}: rax={rax:#x} rcx={rcx:#x} rdx={rdx:#x} rbx={rbx:#x} rsi={rsi:#x} rdi={rdi:#x} rbp={rbp:#x} rsp={rsp:#x} r8={r8:#x} r9={r9:#x} r10={r10:#x} r11={r11:#x} r12={r12:#x} r13={r13:#x} r14={r14:#x} r15={r15:#x} bytes={}",
            hex(bytes)
        );
    }
    write_trace(cpu);
    println!("kernel cr3={:#x}", cpu.regs.cr3);
    if let Some(linear) = faulting_linear(error) {
        println!("page walk for faulting linear {linear:#x}:");
        walk_tables(cpu, linear);
        println!(
            "interpreter translate: {:?}",
            cpu.translate(linear, rish_softvm_core::arch::paging::AccessKind::Read)
        );
    }
    println!("scanning for decompressed image:");
    scan_memory(cpu, b"\x7fELF");
    scan_memory(cpu, b"Linux version");
    scan_memory(cpu, b"RISH_X86_64");
    println!("memory at original image (0x100000):");
    dump_region(cpu, 0x100000, 0x80);
    println!("memory at relocated image (0x1000000):");
    dump_region(cpu, 0x1000000, 0x80);
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
    println!("memory at fault rip - 0x60:");
    let fr = cpu.regs.rip.wrapping_sub(0x60);
    dump_region(cpu, fr, 0xc0);
    if !console.is_empty() {
        print_console(console);
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

fn faulting_linear(error: &CpuError) -> Option<u64> {
    let text = match error {
        CpuError::GuestFault(message) => message,
        CpuError::PageFault { linear, .. } => return Some(*linear),
        _ => return None,
    };
    let hex_start = text.find("0x")?;
    let rest = &text[hex_start + 2..];
    let end = rest.find(|c: char| !c.is_ascii_hexdigit())?;
    u64::from_str_radix(&rest[..end], 16).ok()
}

fn walk_tables(cpu: &Cpu, linear: u64) {
    let cr3 = cpu.regs.cr3 & 0x000F_FFFF_FFFF_F000;
    let pml4_index = (linear >> 39) & 0x1FF;
    let read_entry = |address: u64| -> u64 { cpu.memory.read_u64(address).unwrap_or(0) };
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

fn print_console(console: &[u8]) {
    let text = String::from_utf8_lossy(console);
    print!("{text}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
}

fn dump_region(cpu: &Cpu, base: u64, length: usize) {
    let mut buffer = vec![0_u8; length];
    let _ = cpu.memory.read(base, &mut buffer);
    for (index, chunk) in buffer.chunks(16).enumerate() {
        println!("  {:#x}: {}", base + index as u64 * 16, hex(chunk));
    }
}

fn scan_memory(cpu: &Cpu, pattern: &[u8]) {
    let mut buffer = vec![0_u8; 1024 * 1024];
    let memory_len = cpu.memory.len();
    let mut hits = 0;
    let mut base = 0_usize;
    while base < memory_len && hits < 8 {
        let chunk_len = (memory_len - base).min(buffer.len());
        let _ = cpu.memory.read(base as u64, &mut buffer[..chunk_len]);
        let mut offset = 0;
        while offset + pattern.len() <= chunk_len {
            if &buffer[offset..offset + pattern.len()] == pattern {
                println!("  hit at {:#x}", base + offset);
                hits += 1;
                offset += pattern.len();
            } else {
                offset += 1;
            }
        }
        base += chunk_len;
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
