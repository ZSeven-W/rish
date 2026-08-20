//! Versioned rish guest control transport over the second 16550 UART.
//!
//! The guest runs rish-guest-agent with stdin/stdout bound to the control
//! serial. This module adapts the bounded provider quanta of X86_64Machine to
//! the SessionIo contract and implements the rish_vm GuestChannel trait
//! (bootstrap handshake, live kernel evidence, command execution). The shared
//! SessionClient owns framing and exchange correlation; any dropped control
//! byte, malformed frame, deadline breach, or halted machine fails the
//! request closed.

use std::sync::{Arc, Mutex};

use rish_core::{GuestCommand, HostReply};
use rish_guest_protocol::{Envelope, SessionClient, SessionError, SessionIo};
use rish_vm::{GuestChannel, GuestKernelEvidence, GuestSession, KernelEvidenceSource, VmError};
use serde_json::Value;

use crate::{EngineLimits, MachineState, SoftVmError, X86_64Machine};

/// Hard cap on live /proc/config.gz evidence.
const MAX_KERNEL_CONFIG_BYTES: usize = 4 * 1024 * 1024;

/// SessionIo adapter over one bounded software VM machine.
struct MachineIo<'a> {
    machine: &'a X86_64Machine,
    limits: &'a EngineLimits,
}

impl SessionIo for MachineIo<'_> {
    fn write(&self, bytes: &[u8]) -> usize {
        self.machine.write_control(bytes)
    }

    fn advance(&self) -> Result<Vec<u8>, String> {
        let report = self
            .machine
            .run_units(self.limits.provider_quantum_units)
            .map_err(|error| error.to_string())?;
        match report.snapshot.state {
            MachineState::Running => {}
            MachineState::Halted | MachineState::Faulted => {
                let state = match report.snapshot.state {
                    MachineState::Halted => "halted",
                    MachineState::Faulted => "faulted",
                    _ => unreachable!("state was matched above"),
                };
                return Err(format!("guest machine {state} during a control exchange"));
            }
            MachineState::Stopped => {
                return Err("guest machine stopped during a control exchange".to_owned());
            }
        }
        Ok(self.machine.take_control())
    }

    fn dropped_output(&self) -> u64 {
        self.machine.dropped_control_output()
    }
}

/// GuestChannel implementation that speaks the framed guest protocol over
/// the provider control serial. One request is in flight at a time.
pub struct SerialGuestTransport {
    machine: Arc<X86_64Machine>,
    limits: EngineLimits,
    in_flight: Mutex<()>,
    client: Mutex<SessionClient>,
}

impl SerialGuestTransport {
    pub fn new(machine: X86_64Machine, limits: EngineLimits) -> Result<Self, SoftVmError> {
        limits.validate()?;
        let max_advances = limits.max_units_per_request / limits.provider_quantum_units;
        let client = SessionClient::new(max_advances)
            .map_err(|error| SoftVmError::ControlChannel(error.to_string()))?;
        Ok(Self {
            machine: Arc::new(machine),
            limits,
            in_flight: Mutex::new(()),
            client: Mutex::new(client),
        })
    }

    #[must_use]
    pub fn machine(&self) -> &X86_64Machine {
        &self.machine
    }

    /// Builds the byte I/O adapter for the shared session client.
    fn io(&self) -> MachineIo<'_> {
        MachineIo {
            machine: &self.machine,
            limits: &self.limits,
        }
    }

    fn lock_client(&self) -> Result<std::sync::MutexGuard<'_, SessionClient>, VmError> {
        self.client
            .lock()
            .map_err(|_| VmError::Guest("guest control session lock poisoned".to_owned()))
    }

    /// Runs a short read-only command and returns its stdout (kernel evidence).
    fn exec_text(
        client: &mut SessionClient,
        argv: &[&str],
        io: &MachineIo<'_>,
    ) -> Result<Vec<u8>, VmError> {
        let outcome = client
            .execute(
                argv.iter().map(|value| (*value).to_owned()).collect(),
                Default::default(),
                None,
                Vec::new(),
                io,
            )
            .map_err(session_error)?;
        let exit_code = outcome.exit_code.unwrap_or(-1);
        if exit_code != 0 {
            return Err(VmError::Guest(format!(
                "{} exited with {}: {}",
                argv[0],
                exit_code,
                String::from_utf8_lossy(&outcome.stderr)
            )));
        }
        if outcome.stdout.len() > MAX_KERNEL_CONFIG_BYTES {
            return Err(VmError::Guest(format!(
                "{} output exceeds the kernel evidence limit",
                argv[0]
            )));
        }
        Ok(outcome.stdout)
    }
}

impl GuestChannel for SerialGuestTransport {
    fn bootstrap(&self, hello: &Envelope) -> Result<Envelope, VmError> {
        let _guard = self
            .in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let io = self.io();
        let mut client = self.lock_client()?;
        if client.negotiated().is_some() {
            return Err(VmError::Protocol(
                "guest control session was already negotiated".to_owned(),
            ));
        }
        client.bootstrap(hello, &io).map_err(session_error)
    }

    fn kernel_config(&self, session: &GuestSession) -> Result<GuestKernelEvidence, VmError> {
        let _guard = self
            .in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let io = self.io();
        let mut client = self.lock_client()?;
        let negotiated = client
            .negotiated()
            .ok_or_else(|| {
                VmError::Protocol("guest control session was not negotiated".to_owned())
            })?
            .clone();
        if negotiated.session_id != session.id() {
            return Err(VmError::KernelEvidenceSessionMismatch {
                expected: session.id().to_owned(),
                actual: negotiated.session_id,
            });
        }
        let release_bytes = Self::exec_text(&mut client, &["uname", "-r"], &io)?;
        let kernel_release = String::from_utf8(release_bytes)
            .map_err(|error| VmError::Guest(format!("kernel release is not UTF-8: {error}")))?
            .trim()
            .to_owned();
        if kernel_release.is_empty() {
            return Err(VmError::Guest(
                "guest returned an empty kernel release".to_owned(),
            ));
        }
        let config_bytes = Self::exec_text(&mut client, &["zcat", "/proc/config.gz"], &io)?;
        let config_text = String::from_utf8(config_bytes)
            .map_err(|error| VmError::Guest(format!("kernel config is not UTF-8: {error}")))?;
        let enabled: std::collections::BTreeSet<String> = config_text
            .lines()
            .filter_map(parse_enabled_kconfig)
            .collect();
        GuestKernelEvidence::new(
            session.id(),
            KernelEvidenceSource::ProcConfigGzip { kernel_release },
            enabled,
        )
    }

    fn execute(&self, command: &GuestCommand) -> Result<HostReply, VmError> {
        let _guard = self
            .in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let io = self.io();
        let mut client = self.lock_client()?;
        if client.negotiated().is_none() {
            return Err(VmError::Protocol(
                "guest control session was not negotiated".to_owned(),
            ));
        }
        let mut argv = Vec::with_capacity(command.args.len().saturating_add(1));
        argv.push(command.program.clone());
        argv.extend(command.args.iter().cloned());
        let outcome = client
            .execute(
                argv,
                command.env.clone(),
                (command.cwd != "/").then(|| command.cwd.clone()),
                command.stdin.clone(),
                &io,
            )
            .map_err(session_error)?;
        let exit_code = outcome
            .exit_code
            .unwrap_or(outcome.signal.map_or(-1, |value| 128 + value));
        Ok(HostReply {
            exit_code,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            payload: Value::Null,
        })
    }
}

fn parse_enabled_kconfig(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("CONFIG_") {
        return None;
    }
    let (name, value) = line.split_once('=')?;
    if value != "y" || name.len() <= "CONFIG_".len() {
        return None;
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    Some(name.to_owned())
}

fn session_error(error: SessionError) -> VmError {
    match error {
        SessionError::Frame(_)
        | SessionError::MissingResponse
        | SessionError::UnexpectedResponse
        | SessionError::TooManyFrames
        | SessionError::NotNegotiated
        | SessionError::InvalidBudget => VmError::Protocol(error.to_string()),
        _ => VmError::Guest(error.to_string()),
    }
}

impl From<SoftVmError> for VmError {
    fn from(error: SoftVmError) -> Self {
        VmError::Guest(error.to_string())
    }
}
