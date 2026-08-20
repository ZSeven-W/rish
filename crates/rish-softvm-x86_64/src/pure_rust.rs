//! Pure-Rust no-JIT x86_64 full-system interpreter provider.
//!
//! Wraps the `rish-softvm-core` Cpu as a MachineProvider. The provider
//! boots the validated Linux bzImage plus initramfs in-process on the
//! worker thread and pumps the bounded 16550 console and control channels
//! every quantum. One execution unit is one retired guest instruction;
//! there is no JIT, no executable-memory translation, and no hypervisor
//! involved.
//!
//! The interpreter currently emulates the console and control serial ports
//! only. It does not declare virtio-block or user networking, so the engine
//! rejects root-disk and network device requests instead of pretending they
//! work. The pinned docker diagnostic guest is initramfs-only and matches
//! this device surface.

use std::fs;

use rish_softvm_core::bzimage::{self, BootParams};
use rish_softvm_core::{Cpu, CpuError};

use crate::{
    KernelFormat, MachineProvider, MachineState, ProviderBuildInfo, ProviderKind, ProviderMachine,
    ProviderRequest, ProviderRun, ProviderSnapshot, SoftVmError, abi, config::GUEST_ARCHITECTURE,
    serial::ProviderIo,
};

/// Provider target identity recorded in the build info.
pub const PURE_RUST_TARGET: &str = "x86_64-softvm-pure-rust";

/// Feature bits the interpreter must declare: full-system x86_64 execution,
/// bounded runs, cancellation polling, both 16550 channels, and initramfs.
pub(crate) const PURE_RUST_REQUIRED_FEATURES: u64 = abi::FEATURE_FULL_SYSTEM
    | abi::FEATURE_X86_64
    | abi::FEATURE_BOUNDED_RUN
    | abi::FEATURE_CANCEL_POLL
    | abi::FEATURE_SERIAL_16550
    | abi::FEATURE_INITRD
    | abi::FEATURE_CONTROL_SERIAL;

pub const PURE_RUST_MIN_MEMORY_MIB: u32 = 128;
pub const PURE_RUST_MAX_MEMORY_MIB: u32 = 1024;

/// Instructions executed between cancellation polls inside one quantum.
const CANCEL_POLL_INSTRUCTIONS: u64 = 50_000;

/// Bytes pumped per host/guest channel read within a quantum.
const HOST_IO_CHUNK_BYTES: usize = 4096;

/// In-process provider wrapping the pure-Rust interpreter.
#[derive(Debug)]
pub struct PureRustProvider {
    build_info: ProviderBuildInfo,
}

impl PureRustProvider {
    /// Provider with the canonical build identity of this crate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            build_info: canonical_build_info(),
        }
    }

    /// Provider with an explicit build identity. The engine gate validates
    /// the contents; this only checks the kind.
    pub fn from_build_info(build_info: ProviderBuildInfo) -> Result<Self, SoftVmError> {
        if build_info.kind != ProviderKind::ExperimentalPureRust {
            return Err(SoftVmError::ProviderContract(
                "pure-Rust provider build info has the wrong provider kind".to_owned(),
            ));
        }
        Ok(Self { build_info })
    }

    /// The canonical build identity, for tests and evidence reporting.
    #[must_use]
    pub fn build_info_static() -> ProviderBuildInfo {
        canonical_build_info()
    }
}

impl Default for PureRustProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn canonical_build_info() -> ProviderBuildInfo {
    ProviderBuildInfo {
        kind: ProviderKind::ExperimentalPureRust,
        // The shared field records the interpreter crate version for the
        // pure-Rust provider (it holds the QEMU version for the TCTI one).
        qemu_version: rish_softvm_core::VERSION.to_owned(),
        source_revision: env!("RISH_SOURCE_REVISION").to_owned(),
        build_id: format!("pure-rust-x86_64-{}", rish_softvm_core::VERSION),
        guest_architecture: GUEST_ARCHITECTURE.to_owned(),
        host_architecture: host_architecture(),
        target_list: PURE_RUST_TARGET.to_owned(),
        compiled_features: PURE_RUST_REQUIRED_FEATURES,
        min_memory_mib: PURE_RUST_MIN_MEMORY_MIB,
        max_memory_mib: PURE_RUST_MAX_MEMORY_MIB,
        max_vcpus: 1,
    }
}

fn host_architecture() -> String {
    std::env::consts::ARCH.to_owned()
}

fn read_artifact(kind: &'static str, path: &std::path::Path) -> Result<Vec<u8>, SoftVmError> {
    fs::read(path).map_err(|source| SoftVmError::ArtifactRead {
        kind,
        path: path.to_path_buf(),
        source,
    })
}

impl MachineProvider for PureRustProvider {
    fn build_info(&self) -> &ProviderBuildInfo {
        &self.build_info
    }

    fn create(
        &self,
        request: ProviderRequest,
        io: ProviderIo,
    ) -> Result<Box<dyn ProviderMachine>, SoftVmError> {
        if request.vcpus != 1 {
            return Err(SoftVmError::InvalidConfig(
                "the pure-Rust interpreter runs exactly one vCPU".to_owned(),
            ));
        }
        let memory_mib = usize::try_from(request.memory_mib)
            .map_err(|_| SoftVmError::InvalidConfig("guest memory size overflow".to_owned()))?;
        if memory_mib < PURE_RUST_MIN_MEMORY_MIB as usize
            || memory_mib > PURE_RUST_MAX_MEMORY_MIB as usize
        {
            return Err(SoftVmError::InvalidConfig(format!(
                "pure-Rust provider memory range is {}..={} MiB",
                PURE_RUST_MIN_MEMORY_MIB, PURE_RUST_MAX_MEMORY_MIB
            )));
        }
        if request.artifacts.kernel_format != KernelFormat::LinuxBzImage {
            return Err(SoftVmError::InvalidKernel(
                "the pure-Rust provider requires a 64-bit Linux bzImage".to_owned(),
            ));
        }
        let kernel = read_artifact("kernel", request.artifacts.kernel.path())?;
        let initrd = match &request.artifacts.initrd {
            Some(file) => Some(read_artifact("initrd", file.path())?),
            None => None,
        };
        let mut cpu = Cpu::new(memory_mib, 0)
            .map_err(|error| SoftVmError::InvalidConfig(error.to_string()))?;
        bzimage::load(
            &mut cpu,
            &kernel,
            initrd.as_deref(),
            &BootParams {
                command_line: request.command_line.clone(),
                memory_mib,
            },
        )
        .map_err(|error| SoftVmError::InvalidKernel(error.to_string()))?;
        Ok(Box::new(PureRustMachine {
            cpu,
            io: Box::new(io),
            stopped: false,
            fault: None,
        }))
    }
}

/// One interpreter instance, confined to its worker thread.
struct PureRustMachine {
    cpu: Cpu,
    io: Box<ProviderIo>,
    stopped: bool,
    fault: Option<String>,
}

impl PureRustMachine {
    /// Moves host-queued console and control bytes into the guest UARTs.
    fn pump_input(&mut self) {
        let mut buffer = [0_u8; HOST_IO_CHUNK_BYTES];
        loop {
            let read = self.io.read_console(&mut buffer);
            if read == 0 {
                break;
            }
            self.cpu.uart_console.push_input(&buffer[..read]);
        }
        loop {
            let read = self.io.control.read_input(&mut buffer);
            if read == 0 {
                break;
            }
            self.cpu.uart_control.push_input(&buffer[..read]);
        }
    }

    /// Moves guest UART output into the host queues.
    fn pump_output(&mut self) {
        loop {
            let bytes = self.cpu.uart_console.drain_output();
            if bytes.is_empty() {
                break;
            }
            self.io.write_console(&bytes);
        }
        loop {
            let bytes = self.cpu.uart_control.drain_output();
            if bytes.is_empty() {
                break;
            }
            self.io.control.write_output(&bytes);
        }
    }

    fn state(&self) -> MachineState {
        if self.stopped {
            MachineState::Stopped
        } else if self.fault.is_some() {
            MachineState::Faulted
        } else if self.cpu.halted {
            MachineState::Halted
        } else {
            MachineState::Running
        }
    }
}

impl ProviderMachine for PureRustMachine {
    fn snapshot(&mut self) -> Result<ProviderSnapshot, SoftVmError> {
        Ok(ProviderSnapshot {
            state: self.state(),
            pc: Some(self.cpu.regs.rip),
            total_units: self.cpu.regs.instructions_retired,
        })
    }

    fn run_quantum(&mut self, max_units: u64) -> Result<ProviderRun, SoftVmError> {
        self.pump_input();
        let mut executed = 0_u64;
        while executed < max_units {
            if self.stopped || self.io.should_cancel() {
                break;
            }
            let chunk = CANCEL_POLL_INSTRUCTIONS.min(max_units - executed);
            match self.cpu.run_instructions(chunk) {
                Ok(ran) => {
                    executed = executed.saturating_add(ran);
                    if self.cpu.halted {
                        break;
                    }
                }
                Err(CpuError::Halted) => break,
                Err(error) => {
                    self.fault = Some(error.to_string());
                    let message = format!(
                        "\n[rish-softvm pure-Rust fault after {} instructions at {:#x}: {error}]\n",
                        self.cpu.regs.instructions_retired, self.cpu.regs.rip
                    );
                    self.io.write_console(message.as_bytes());
                    break;
                }
            }
        }
        self.pump_output();
        Ok(ProviderRun {
            executed_units: executed,
            snapshot: self.snapshot()?,
        })
    }

    fn request_stop(&mut self) -> Result<(), SoftVmError> {
        self.stopped = true;
        Ok(())
    }
}
