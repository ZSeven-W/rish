//! Diagnostic boot runner: loads the real pinned bzImage and initramfs,
//! executes the pure-Rust interpreter, and reports the first gap (missing
//! instruction, guest fault, or halt) together with console output.

use std::{env, fs, process::ExitCode};

use rish_softvm_core::bzimage::{self, BootParams};
use rish_softvm_core::{Cpu, CpuError};

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
    let mut kernel_path = None;
    let mut initrd_path = None;
    let mut cmdline = String::from(
        "console=ttyS0,115200n8 console=ttyS1,115200n8 rdinit=/init          panic=-1 oops=panic nokaslr cgroup_no_v1=all nolapic_timer          earlyprintk=serial,ttyS0,115200",
    );
    let mut memory_mib = 1024;
    let mut steps = 200_000_000_u64;
    let mut progress_every = 100_000_u64;
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--kernel" => {
                index += 1;
                kernel_path = Some(args.get(index).ok_or("--kernel needs a path")?.clone());
            }
            "--initrd" => {
                index += 1;
                initrd_path = Some(args.get(index).ok_or("--initrd needs a path")?.clone());
            }
            "--cmdline" => {
                index += 1;
                cmdline = args.get(index).ok_or("--cmdline needs text")?.clone();
            }
            "--memory-mib" => {
                index += 1;
                memory_mib = args
                    .get(index)
                    .ok_or("--memory-mib needs a number")?
                    .parse()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
            }
            "--steps" => {
                index += 1;
                steps = args
                    .get(index)
                    .ok_or("--steps needs a number")?
                    .parse()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
            }
            "--progress-every" => {
                index += 1;
                progress_every = args
                    .get(index)
                    .ok_or("--progress-every needs a number")?
                    .parse()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    let kernel_path = kernel_path.ok_or("--kernel is required")?;
    let kernel = fs::read(&kernel_path).map_err(|error| error.to_string())?;
    let initrd = match &initrd_path {
        Some(path) => Some(fs::read(path).map_err(|error| error.to_string())?),
        None => None,
    };

    println!("boot: pure-Rust x86_64 interpreter diagnostics");
    println!("kernel: {kernel_path} ({} bytes)", kernel.len());
    if let Some(path) = &initrd_path {
        println!("initrd: {path}");
    }
    println!("memory: {memory_mib} MiB, budget: {steps} instructions");

    let mut cpu = Cpu::new(memory_mib, 0).map_err(|error| error.to_string())?;
    bzimage::load(
        &mut cpu,
        &kernel,
        initrd.as_deref(),
        &BootParams {
            command_line: cmdline.clone(),
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

    let mut console = Vec::new();
    let progress_every = progress_every.max(1);
    let mut last_report = 0_u64;
    for _ in 0..steps {
        if let Err(error) = cpu.step() {
            console.extend(cpu.uart_console.drain_output());
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
    }
    console.extend(cpu.uart_console.drain_output());
    if !console.is_empty() {
        print_console(&console);
    }
    println!("budget exhausted after {steps} instructions without a gap");
    let mut trace_text = String::new();
    for (rip, rax, rbp, rsp, bytes) in &cpu.trace {
        let line = format!(
            "  {rip:#x}: rax={rax:#x} rbp={rbp:#x} rsp={rsp:#x} bytes={}",
            hex(bytes)
        );
        trace_text.push_str(&line);
        trace_text.push('\n');
    }
    let _ = fs::write("/tmp/rish-boot-trace.txt", trace_text);
    Ok(0)
}

fn report_gap(error: &CpuError, cpu: &Cpu, console: &[u8]) {
    println!("last instructions:");
    let mut trace_text = String::new();
    for (rip, rax, rbp, rsp, bytes) in &cpu.trace {
        let line = format!(
            "  {rip:#x}: rax={rax:#x} rbp={rbp:#x} rsp={rsp:#x} bytes={}",
            hex(bytes)
        );
        println!("{line}");
        trace_text.push_str(&line);
        trace_text.push('\n');
    }
    let _ = fs::write("/tmp/rish-boot-trace.txt", trace_text.clone());
    let mut report = trace_text;
    report.push_str("\n--- scan ---\n");
    // Where did the decompressed kernel land? Scan physical memory.
    println!("kernel cr3={:#x}", cpu.regs.cr3);
    println!("page walk for linear 0x1000000:");
    walk_tables(cpu, 0x1000000);
    println!("page walk for linear 0xcbad95c:");
    walk_tables(cpu, 0xcbad95c);
    println!("page walk for linear 0x100000:");
    walk_tables(cpu, 0x100000);
    println!("scanning for decompressed image:");
    println!("scanning for decompressed image:");
    scan_memory(cpu, b"\x7fELF");
    scan_memory(cpu, b"Linux version");
    scan_memory(cpu, b"RISH_X86_64");
    println!("memory at original image (0x100000):");
    println!("memory at original image (0x100000):");
    dump_region(cpu, 0x100000, 0x80);
    println!("memory at relocated image (0x1000000):");
    dump_region(cpu, 0x1000000, 0x80);
    println!("memory at kernel stack (0x35cd3d0):");
    dump_region(cpu, 0x35cd3d0, 0x60);
    println!("memory at 0xa260:");
    dump_region(cpu, 0xa260, 0x40);
    println!("memory at 0xe030:");
    dump_region(cpu, 0xe030, 0x30);
    dump_region(cpu, 0x1000000, 0x80);
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
    // Scan in 1 MiB chunks for the pattern, reporting up to 8 hits.
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
                // (also recorded by the caller via the report file)
                hits += 1;
                offset += pattern.len();
            } else {
                offset += 1;
            }
        }
        base += chunk_len;
    }
}

fn walk_tables(cpu: &Cpu, linear: u64) {
    // Walk the active 4-level tables, printing each entry.
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

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}