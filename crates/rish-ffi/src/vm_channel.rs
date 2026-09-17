//! FFI control transport with cancellation/deadline checks at every quantum.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use rish_core::HostReply;
use rish_guest_protocol::{
    Envelope, ExecRequest, PendingStdin, SessionClient, SessionError, SessionIo, StreamChannel,
};
use rish_softvm_x86_64::{EngineLimits, MachineState, X86_64Machine};

use crate::vm_cancel::{CANCELLED, Cancellation, TIMED_OUT};

/// Input queued for a command that is already running. It has its own lock so
/// the thread queueing an answer never waits on the session lock the execution
/// holds for as long as the command runs.
#[derive(Default)]
pub(crate) struct StdinInbox {
    queued: Mutex<VecDeque<Vec<u8>>>,
    close: AtomicBool,
}

impl StdinInbox {
    pub(crate) fn write(&self, bytes: Vec<u8>) {
        if let Ok(mut queued) = self.queued.lock() {
            queued.push_back(bytes);
        }
    }

    pub(crate) fn close(&self) {
        self.close.store(true, Ordering::SeqCst);
    }

    fn reset(&self) {
        if let Ok(mut queued) = self.queued.lock() {
            queued.clear();
        }
        self.close.store(false, Ordering::SeqCst);
    }
}

impl PendingStdin for StdinInbox {
    fn take_pending(&self) -> Option<Vec<u8>> {
        self.queued.lock().ok()?.pop_front()
    }

    fn close_requested(&self) -> bool {
        self.close.load(Ordering::SeqCst)
    }
}

pub(crate) struct VmChannel {
    pub(crate) machine: Arc<X86_64Machine>,
    limits: EngineLimits,
    cancel: Arc<Cancellation>,
    client: Mutex<SessionClient>,
    pub(crate) stdin_inbox: Arc<StdinInbox>,
}

impl VmChannel {
    pub(crate) fn new(
        machine: Arc<X86_64Machine>,
        limits: EngineLimits,
        cancel: Arc<Cancellation>,
    ) -> Result<Self, String> {
        // Launch validates limits. Still guard the divisor locally so future
        // callers cannot panic before construction of the session client.
        let advances = limits
            .max_units_per_request
            .checked_div(limits.provider_quantum_units)
            .ok_or("invalid provider quantum")?;
        let client = SessionClient::new(advances).map_err(|error| error.to_string())?;
        Ok(Self {
            machine,
            limits,
            cancel,
            client: Mutex::new(client),
            stdin_inbox: Arc::new(StdinInbox::default()),
        })
    }

    fn io(&self, deadline: Option<Instant>) -> MachineIo<'_> {
        MachineIo {
            machine: &self.machine,
            limits: &self.limits,
            cancel: &self.cancel,
            deadline,
        }
    }

    pub(crate) fn bootstrap(&self, hello: &Envelope) -> Result<(), String> {
        self.cancel.check()?;
        self.client
            .lock()
            .map_err(|_| "guest control session lock poisoned")?
            .bootstrap(hello, &self.io(None))
            .map(|_| ())
            .map_err(control_error)
    }

    pub(crate) fn execute_observed(
        &self,
        request: ExecRequest,
        max_output_bytes: Option<usize>,
        observer: &mut dyn FnMut(StreamChannel, &[u8]),
    ) -> Result<HostReply, String> {
        self.cancel.check()?;
        let (request, deadline) = prepare_execution(request);
        // The ABI requires serialized execution; try_lock also fails closed on
        // accidental re-entry instead of deadlocking inside an output callback.
        let mut client = self.client.try_lock().map_err(|_| "E_VM_SESSION_BUSY")?;
        let mut output_bytes = 0_usize;
        let mut exceeded = false;
        let result =
            client.execute_observed(request, &[], &self.io(deadline), &mut |channel, bytes| {
                output_bytes = output_bytes.saturating_add(bytes.len());
                if max_output_bytes.is_some_and(|limit| output_bytes > limit) {
                    exceeded = true;
                    self.cancel.request();
                } else {
                    observer(channel, bytes);
                }
            });
        if exceeded {
            return Err("E_VM_OUTPUT_LIMIT".into());
        }
        let outcome = result.map_err(control_error)?;
        self.cancel.check()?;
        Ok(HostReply {
            exit_code: outcome
                .exit_code
                .unwrap_or(outcome.signal.map_or(-1, |value| 128 + value)),
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            payload: serde_json::Value::Null,
        })
    }

    /// Runs a command whose stdin arrives while it runs. The inbox is emptied
    /// first so an answer queued for an execution that has already ended cannot
    /// be delivered to this one.
    pub(crate) fn execute_interactive(
        &self,
        request: ExecRequest,
        max_output_bytes: Option<usize>,
        observer: &mut dyn FnMut(StreamChannel, &[u8]),
    ) -> Result<HostReply, String> {
        self.cancel.check()?;
        let (request, deadline) = prepare_execution(request);
        let mut client = self.client.try_lock().map_err(|_| "E_VM_SESSION_BUSY")?;
        self.stdin_inbox.reset();
        let mut output_bytes = 0_usize;
        let mut exceeded = false;
        let result = client.execute_interactive(
            request,
            &self.io(deadline),
            &mut |channel, bytes| {
                output_bytes = output_bytes.saturating_add(bytes.len());
                if max_output_bytes.is_some_and(|limit| output_bytes > limit) {
                    exceeded = true;
                    self.cancel.request();
                } else {
                    observer(channel, bytes);
                }
            },
            self.stdin_inbox.clone(),
        );
        if exceeded {
            return Err("E_VM_OUTPUT_LIMIT".into());
        }
        let outcome = result.map_err(control_error)?;
        self.cancel.check()?;
        Ok(HostReply {
            exit_code: outcome
                .exit_code
                .unwrap_or(outcome.signal.map_or(-1, |value| 128 + value)),
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            payload: serde_json::Value::Null,
        })
    }
}

fn prepare_execution(mut request: ExecRequest) -> (ExecRequest, Option<Instant>) {
    // ABI v2 deadlines use the host monotonic clock. A guest clock advances
    // with the interpreter and can run faster than host wall time: forwarding
    // this same budget would let its supervisor SIGKILL the child early and
    // report exit 137 instead of the promised E_VM_TIMEOUT/session cancellation.
    // This only changes the FFI producer; the guest protocol still supports
    // deadlines for other callers that deliberately use guest-clock budgets.
    let deadline = request
        .timeout_ms
        .take()
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    (request, deadline)
}

fn control_error(error: SessionError) -> String {
    match error {
        SessionError::Advance(message) if matches!(message.as_str(), CANCELLED | TIMED_OUT) => {
            message
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "vm_channel_tests.rs"]
mod tests;

struct MachineIo<'a> {
    machine: &'a X86_64Machine,
    limits: &'a EngineLimits,
    cancel: &'a Cancellation,
    deadline: Option<Instant>,
}

impl MachineIo<'_> {
    fn check(&self) -> Result<(), String> {
        self.cancel.check()?;
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.cancel.timeout();
            return Err(TIMED_OUT.into());
        }
        Ok(())
    }
}

impl SessionIo for MachineIo<'_> {
    fn write(&self, bytes: &[u8]) -> usize {
        if self.check().is_err() {
            0
        } else {
            self.machine.write_control(bytes)
        }
    }

    fn advance(&self) -> Result<Vec<u8>, String> {
        self.check()?;
        let report = self
            .machine
            .run_units(self.limits.provider_quantum_units)
            .map_err(|error| error.to_string())?;
        // Keep kernel diagnostics (including an OOM kill) observable during
        // execution, using the same opt-in switch as the boot path. They are
        // diagnostic stderr, never the program's framed stdout/stderr stream.
        if !report.console.is_empty() && std::env::var_os("RISH_DBG_CONSOLE").is_some() {
            eprint!("{}", String::from_utf8_lossy(&report.console));
        }
        // run_units clears the worker flag at entry. This second token check
        // closes the race with an independent cancellation at that exact point.
        self.check()?;
        let stopped = match report.snapshot.state {
            MachineState::Running => None,
            MachineState::Halted => Some("halted"),
            MachineState::Faulted => Some("faulted"),
            MachineState::Stopped => Some("stopped"),
        };
        if let Some(state) = stopped {
            return Err(format!("guest machine {state} during a control exchange"));
        }
        Ok(self.machine.take_control())
    }

    fn dropped_output(&self) -> u64 {
        self.machine.dropped_control_output()
    }
}
