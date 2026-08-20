use std::sync::{
    atomic::Ordering,
    mpsc::{self, TrySendError},
};

use rish_core::{GuestCommand, HostReply};
use rish_guest_protocol::Envelope;
use rish_vm::{GuestChannel, GuestKernelEvidence, GuestSession, VmError};
use serde::{Deserialize, Serialize};

use crate::{
    EngineLimits, KernelFormat, MachineState, SoftVmError,
    worker::{StepOutcome, WorkerCommand, WorkerHandle},
};

/// Concrete state reported by the provider after artifact attachment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootSnapshot {
    pub architecture: String,
    pub provider_build_id: String,
    pub provider_source_revision: String,
    pub memory_mib: u32,
    pub vcpus: u32,
    pub state: MachineState,
    pub pc: Option<u64>,
    pub total_units: u64,
    pub kernel_format: KernelFormat,
    pub kernel_bytes: u64,
    pub initrd_bytes: Option<u64>,
    pub root_disk_bytes: u64,
}

/// Result of one bounded TCTI worker run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub requested_units: u64,
    pub executed_units: u64,
    pub cancelled: bool,
    pub snapshot: BootSnapshot,
    pub console: Vec<u8>,
    pub dropped_console_bytes: u64,
}

/// Loaded x86-64 provider handle.
///
/// The QEMU instance and all non-`Send` C state remain on the worker thread.
pub struct X86_64Machine {
    worker: WorkerHandle,
    limits: EngineLimits,
}

impl X86_64Machine {
    pub(crate) fn new(worker: WorkerHandle, limits: EngineLimits) -> Self {
        Self { worker, limits }
    }

    #[must_use]
    pub fn initial_snapshot(&self) -> &BootSnapshot {
        &self.worker.initial_snapshot
    }

    pub fn snapshot(&self) -> Result<BootSnapshot, SoftVmError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.send_command(WorkerCommand::Snapshot { reply: reply_tx })?;
        reply_rx
            .recv()
            .map_err(|_| SoftVmError::WorkerStopped)?
            .map_err(SoftVmError::Worker)
    }

    /// Advances at most `units` provider execution units.
    ///
    /// The provider ABI defines these as bounded TCTI scheduling units rather
    /// than guest instructions. Another thread may call [`Self::cancel`].
    pub fn run_units(&self, units: u64) -> Result<RunReport, SoftVmError> {
        if units > self.limits.max_units_per_run {
            return Err(SoftVmError::UnitLimit {
                requested: units,
                limit: self.limits.max_units_per_run,
            });
        }
        self.worker.cancel.store(false, Ordering::Relaxed);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.send_command(WorkerCommand::Step {
            units,
            reply: reply_tx,
        })?;
        let StepOutcome {
            snapshot,
            executed_units,
            cancelled,
        } = reply_rx
            .recv()
            .map_err(|_| SoftVmError::WorkerStopped)?
            .map_err(SoftVmError::Worker)?;
        Ok(RunReport {
            requested_units: units,
            executed_units,
            cancelled,
            snapshot,
            console: self.take_console(),
            dropped_console_bytes: self.worker.serial.dropped_output(),
        })
    }

    /// Requests cancellation. The worker checks between quanta and the C
    /// provider receives the same atomic through `should_cancel`.
    pub fn cancel(&self) {
        self.worker.cancel.store(true, Ordering::Relaxed);
    }

    /// Queues bounded UART input and returns the number of accepted bytes.
    pub fn write_console(&self, bytes: &[u8]) -> Result<usize, SoftVmError> {
        let mut accepted = 0;
        for byte in bytes {
            match self.worker.serial.input.try_send(*byte) {
                Ok(()) => accepted += 1,
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(SoftVmError::WorkerStopped);
                }
            }
        }
        Ok(accepted)
    }

    /// Drains currently buffered 16550 UART transmit bytes.
    #[must_use]
    pub fn take_console(&self) -> Vec<u8> {
        self.worker.serial.drain(self.limits.max_console_bytes)
    }

    fn send_command(&self, command: WorkerCommand) -> Result<(), SoftVmError> {
        match self.worker.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SoftVmError::WorkerBusy),
            Err(TrySendError::Disconnected(_)) => Err(SoftVmError::WorkerStopped),
        }
    }
}

impl GuestChannel for X86_64Machine {
    fn bootstrap(&self, _hello: &Envelope) -> Result<Envelope, VmError> {
        Err(VmError::Protocol(
            SoftVmError::ControlTransportUnavailable.to_string(),
        ))
    }

    fn kernel_config(&self, _session: &GuestSession) -> Result<GuestKernelEvidence, VmError> {
        Err(VmError::Guest(
            SoftVmError::ControlTransportUnavailable.to_string(),
        ))
    }

    fn execute(&self, _command: &GuestCommand) -> Result<HostReply, VmError> {
        Err(VmError::Guest(
            SoftVmError::ControlTransportUnavailable.to_string(),
        ))
    }
}

impl Drop for X86_64Machine {
    fn drop(&mut self) {
        self.worker.cancel.store(true, Ordering::Relaxed);
        let _ = self.worker.commands.try_send(WorkerCommand::Shutdown);
    }
}
