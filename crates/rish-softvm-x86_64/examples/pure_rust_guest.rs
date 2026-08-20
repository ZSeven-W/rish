//! End-to-end pure-Rust docker guest harness.
//!
//! Boots the pinned Alpine docker diagnostic guest with the in-repository
//! x86_64 interpreter, waits for the RISH_X86_64_BOOT_OK console marker, then
//! runs the production evidence chain (probe, Hello handshake, live kernel
//! evidence, capability mapping) and executes commands through dockerd in
//! the guest. This is the same code path the mobile apps use via the
//! ExperimentalPureRust provider; macOS only hosts this development run.

use std::{env, path::PathBuf, process::ExitCode, sync::Arc, time::Instant};

use rish_core::{Capability, Platform};
use rish_softvm_x86_64::{
    EngineLimits, MachineProvider, PureRustProvider, SerialGuestTransport, X86_64SoftwareEngine,
};
use rish_vm::{
    GuestChannel, GuestKernelContract, VmAcceleration, VmCandidate, VmConfig, VmDevice, VmEngine,
    VmError, VmProbe,
};

const BOOT_OK_MARKER: &[u8] = b"RISH_X86_64_BOOT_OK";
const BOOT_FAILED_MARKER: &[u8] = b"RISH_X86_64_BOOT_FAILED";

struct Options {
    kernel: PathBuf,
    initrd: PathBuf,
    root_disk: PathBuf,
    memory_mib: u32,
    boot_budget_units: u64,
    handshake_budget_units: u64,
    command: Vec<String>,
}

fn parse_args() -> Result<Options, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut kernel = None;
    let mut initrd = None;
    let mut root_disk = None;
    let mut memory_mib = 1024_u32;
    let mut boot_budget_units = 6_000_000_000_u64;
    let mut handshake_budget_units = 6_000_000_000_u64;
    let mut command = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--kernel" => {
                index += 1;
                kernel = Some(args.get(index).ok_or("--kernel needs a path")?.into());
            }
            "--initrd" => {
                index += 1;
                initrd = Some(args.get(index).ok_or("--initrd needs a path")?.into());
            }
            "--root-disk" => {
                index += 1;
                root_disk = Some(args.get(index).ok_or("--root-disk needs a path")?.into());
            }
            "--memory-mib" => {
                index += 1;
                memory_mib = args
                    .get(index)
                    .ok_or("--memory-mib needs a number")?
                    .parse()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
            }
            "--boot-budget" => {
                index += 1;
                boot_budget_units = args
                    .get(index)
                    .ok_or("--boot-budget needs a number")?
                    .parse()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
            }
            "--handshake-budget" => {
                index += 1;
                handshake_budget_units = args
                    .get(index)
                    .ok_or("--handshake-budget needs a number")?
                    .parse()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
            }
            "--exec" => {
                command = args[index + 1..].to_vec();
                break;
            }
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    Ok(Options {
        kernel: kernel.ok_or("--kernel is required")?,
        initrd: initrd.ok_or("--initrd is required")?,
        root_disk: root_disk.ok_or("--root-disk is required (any regular file)")?,
        memory_mib,
        boot_budget_units,
        handshake_budget_units,
        command,
    })
}

/// One engine whose boot channel was already produced by the stage-1 boot
/// loop: the production VmCandidate evidence chain runs over it without
/// booting the guest a second time.
struct PrebootedEngine {
    real: X86_64SoftwareEngine,
    channel: std::sync::Mutex<Option<Box<dyn GuestChannel>>>,
}

impl VmEngine for PrebootedEngine {
    fn probe(&self) -> VmProbe {
        self.real.probe()
    }

    fn boot(&self, _config: &VmConfig) -> Result<Box<dyn GuestChannel>, VmError> {
        self.channel
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or_else(|| VmError::Boot("prebooted guest channel was already consumed".to_owned()))
    }
}

/// Kernel and capability requirements the docker diagnostic guest actually
/// satisfies: the pinned virt kernel's built-in symbols plus the agent's
/// advertised exec capability.
fn docker_guest_contract() -> GuestKernelContract {
    GuestKernelContract::new(
        [
            "CONFIG_NAMESPACES",
            "CONFIG_BINFMT_ELF",
            "CONFIG_PID_NS",
            "CONFIG_USER_NS",
            "CONFIG_UTS_NS",
            "CONFIG_IPC_NS",
            "CONFIG_NET_NS",
            "CONFIG_CGROUPS",
            "CONFIG_MEMCG",
            "CONFIG_DEVTMPFS",
            "CONFIG_MODULES",
            "CONFIG_TMPFS",
        ],
        [Capability::LinuxElf, Capability::CommandOffload],
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pure-rust-guest: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    let limits = EngineLimits {
        max_units_per_request: options.handshake_budget_units,
        ..EngineLimits::default()
    };
    let provider = PureRustProvider::new();
    let build = provider.build_info().clone();
    println!(
        "pure-rust-guest: provider kind={:?} interpreter={} revision={} build={}",
        build.kind, build.qemu_version, build.source_revision, build.build_id
    );
    let engine = X86_64SoftwareEngine::new(Arc::new(provider), limits.clone())
        .map_err(|error| error.to_string())?;

    let config = VmConfig {
        architecture: "x86_64".to_owned(),
        vcpus: 1,
        memory_mib: options.memory_mib,
        kernel_path: options.kernel.to_string_lossy().into_owned(),
        initrd_path: Some(options.initrd.to_string_lossy().into_owned()),
        root_disk_path: options.root_disk.to_string_lossy().into_owned(),
        acceleration: VmAcceleration::Interpreter,
        devices: vec![VmDevice::Console],
        command_line: String::new(), // engine default: pinned docker guest cmdline
    };

    // Stage 1: boot until the init script reports on the console.
    let machine = engine.launch(&config).map_err(|error| error.to_string())?;
    let started = Instant::now();
    let mut executed = 0_u64;
    let mut console = Vec::new();
    loop {
        let report = machine
            .run_units(500_000)
            .map_err(|error| error.to_string())?;
        executed += report.executed_units;
        console.extend_from_slice(&report.console);
        print_console(&mut console);
        if contains(&console, BOOT_FAILED_MARKER) {
            return Err("guest init reported RISH_X86_64_BOOT_FAILED".to_owned());
        }
        if contains(&console, BOOT_OK_MARKER) {
            println!(
                "[pure-rust-guest] boot ok marker after {executed} units in {:?}",
                started.elapsed()
            );
            break;
        }
        match report.snapshot.state {
            rish_softvm_x86_64::MachineState::Running => {}
            other => {
                return Err(format!(
                    "guest machine {other:?} during boot (rip={:?})",
                    report.snapshot.pc
                ));
            }
        }
        if executed > options.boot_budget_units {
            return Err(format!(
                "boot budget exhausted after {executed} units without the boot ok marker"
            ));
        }
    }

    // Stage 2: the production evidence chain over the control serial, on
    // the already-booted machine (no second boot).
    let channel =
        SerialGuestTransport::new(machine, limits.clone()).map_err(|error| error.to_string())?;
    let prebooted = PrebootedEngine {
        real: X86_64SoftwareEngine::new(Arc::new(PureRustProvider::new()), limits)
            .map_err(|error| error.to_string())?,
        channel: std::sync::Mutex::new(Some(Box::new(channel))),
    };
    let candidate = VmCandidate::new(Platform::Android, config, docker_guest_contract());
    let booted = candidate
        .boot(&prebooted)
        .map_err(|error| error.to_string())?;
    println!(
        "guest session: id={} kernel={}",
        booted.session().id(),
        booted.kernel_evidence().source().kernel_release()
    );
    let profile = booted.profile();
    println!("verified capability profile: {:?}", profile.capabilities());

    if options.command.is_empty() {
        return Ok(());
    }
    let command = rish_core::GuestCommand {
        program: options.command[0].clone(),
        args: options.command[1..].to_vec(),
        env: Default::default(),
        cwd: "/".to_owned(),
        stdin: Vec::new(),
    };
    let reply = booted
        .execute(&command)
        .map_err(|error| error.to_string())?;
    print!("{}", String::from_utf8_lossy(&reply.stdout));
    eprint!("{}", String::from_utf8_lossy(&reply.stderr));
    if reply.exit_code != 0 {
        return Err(format!("guest command exited with {}", reply.exit_code));
    }
    Ok(())
}

fn contains(buffer: &[u8], marker: &[u8]) -> bool {
    buffer.windows(marker.len()).any(|window| window == marker)
}

fn print_console(buffer: &mut Vec<u8>) {
    if buffer.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(buffer);
    print!("{text}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    buffer.clear();
}
