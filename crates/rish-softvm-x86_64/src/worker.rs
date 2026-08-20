use std::{
    any::Any,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use crate::{
    BootSnapshot, EngineLimits, MachineProvider, MachineState, ProviderBuildInfo, ProviderRequest,
    SoftVmError, ValidatedArtifacts,
    provider::{ProviderMachine, ProviderSnapshot},
    serial::{ControlChannel, HostSerial, io_pair},
};

pub(crate) enum WorkerCommand {
    Snapshot {
        reply: SyncSender<Result<BootSnapshot, String>>,
    },
    Step {
        units: u64,
        reply: SyncSender<Result<StepOutcome, String>>,
    },
    Shutdown,
}

pub(crate) struct StepOutcome {
    pub snapshot: BootSnapshot,
    pub executed_units: u64,
    pub cancelled: bool,
}

pub(crate) struct WorkerHandle {
    pub commands: SyncSender<WorkerCommand>,
    pub serial: HostSerial,
    pub control: Arc<ControlChannel>,
    pub cancel: Arc<AtomicBool>,
    pub initial_snapshot: BootSnapshot,
}

pub(crate) fn spawn_worker(
    provider: Arc<dyn MachineProvider>,
    request: ProviderRequest,
    limits: &EngineLimits,
) -> Result<WorkerHandle, SoftVmError> {
    let build = provider.build_info().clone();
    let artifacts = request.artifacts.clone();
    let memory_mib = request.memory_mib;
    let vcpus = request.vcpus;
    let (command_tx, command_rx) = mpsc::sync_channel(4);
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let (serial, control, provider_io) = io_pair(
        limits.max_console_bytes,
        limits.max_control_bytes,
        Arc::clone(&cancel),
    );
    let worker_cancel = Arc::clone(&cancel);
    let quantum = limits.provider_quantum_units;

    thread::Builder::new()
        .name("rish-x86-64-tcti".to_owned())
        .spawn(move || {
            let startup = panic::catch_unwind(AssertUnwindSafe(|| {
                let mut machine = provider.create(request, provider_io)?;
                let provider_snapshot = machine.snapshot()?;
                let snapshot = snapshot(&build, &artifacts, memory_mib, vcpus, provider_snapshot);
                Ok::<_, SoftVmError>((machine, snapshot))
            }));
            let (mut machine, mut snapshot) = match startup {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    let _ = startup_tx.send(Err(error.to_string()));
                    return;
                }
                Err(payload) => {
                    let _ = startup_tx.send(Err(panic_message(payload)));
                    return;
                }
            };
            if startup_tx.send(Ok(snapshot.clone())).is_err() {
                return;
            }
            worker_loop(
                machine.as_mut(),
                &mut snapshot,
                command_rx,
                worker_cancel,
                quantum,
                &build,
                &artifacts,
                memory_mib,
                vcpus,
            );
        })
        .map_err(|error| SoftVmError::Worker(error.to_string()))?;

    let initial_snapshot = startup_rx
        .recv_timeout(limits.startup_timeout)
        .map_err(|error| SoftVmError::Worker(format!("startup response failed: {error}")))?
        .map_err(SoftVmError::Worker)?;

    Ok(WorkerHandle {
        commands: command_tx,
        serial,
        control,
        cancel,
        initial_snapshot,
    })
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    machine: &mut dyn ProviderMachine,
    snapshot_value: &mut BootSnapshot,
    commands: Receiver<WorkerCommand>,
    cancel: Arc<AtomicBool>,
    quantum: u64,
    build: &ProviderBuildInfo,
    artifacts: &ValidatedArtifacts,
    memory_mib: u32,
    vcpus: u32,
) {
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Snapshot { reply } => {
                let result = machine.snapshot().map(|provider_snapshot| {
                    let value = snapshot(build, artifacts, memory_mib, vcpus, provider_snapshot);
                    *snapshot_value = value.clone();
                    value
                });
                let _ = reply.send(result.map_err(|error| error.to_string()));
            }
            WorkerCommand::Step { units, reply } => {
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    run_bounded(
                        machine, units, quantum, &cancel, build, artifacts, memory_mib, vcpus,
                    )
                }))
                .map_err(panic_message)
                .and_then(|result| result.map_err(|error| error.to_string()));
                if let Ok(value) = &outcome {
                    *snapshot_value = value.snapshot.clone();
                }
                let failed = outcome.is_err();
                let _ = reply.send(outcome);
                if failed {
                    break;
                }
            }
            WorkerCommand::Shutdown => {
                let _ = machine.request_stop();
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_bounded(
    machine: &mut dyn ProviderMachine,
    requested: u64,
    quantum: u64,
    cancel: &AtomicBool,
    build: &ProviderBuildInfo,
    artifacts: &ValidatedArtifacts,
    memory_mib: u32,
    vcpus: u32,
) -> Result<StepOutcome, SoftVmError> {
    let mut executed = 0_u64;
    let mut latest = machine.snapshot()?;
    while executed < requested
        && !cancel.load(Ordering::Relaxed)
        && latest.state == MachineState::Running
    {
        let budget = quantum.min(requested - executed);
        let result = machine.run_quantum(budget)?;
        if result.executed_units == 0 && result.snapshot.state == MachineState::Running {
            return Err(SoftVmError::ProviderContract(
                "provider made no progress while reporting a running VM".to_owned(),
            ));
        }
        executed = executed
            .checked_add(result.executed_units)
            .ok_or_else(|| SoftVmError::ProviderContract("unit counter overflow".to_owned()))?;
        latest = result.snapshot;
    }
    Ok(StepOutcome {
        snapshot: snapshot(build, artifacts, memory_mib, vcpus, latest),
        executed_units: executed,
        cancelled: executed < requested && cancel.load(Ordering::Relaxed),
    })
}

fn snapshot(
    build: &ProviderBuildInfo,
    artifacts: &ValidatedArtifacts,
    memory_mib: u32,
    vcpus: u32,
    provider: ProviderSnapshot,
) -> BootSnapshot {
    BootSnapshot {
        architecture: "x86_64".to_owned(),
        provider_build_id: build.build_id.clone(),
        provider_source_revision: build.source_revision.clone(),
        memory_mib,
        vcpus,
        state: provider.state,
        pc: provider.pc,
        total_units: provider.total_units,
        kernel_format: artifacts.kernel_format,
        kernel_bytes: artifacts.kernel.bytes(),
        initrd_bytes: artifacts.initrd.as_ref().map(|file| file.bytes()),
        root_disk_bytes: artifacts.root_disk.bytes(),
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "provider panicked with a non-string payload".to_owned()
    }
}
