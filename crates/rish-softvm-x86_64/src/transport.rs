//! Versioned rish guest control transport over the second 16550 UART.
//!
//! The guest runs rish-guest-agent with stdin/stdout bound to the control
//! serial. This module frames host requests, drives bounded provider quanta
//! while waiting for guest frames, and implements the rish_vm GuestChannel
//! contract (bootstrap handshake, live kernel evidence, command execution).
//! Any dropped control byte, malformed frame, deadline breach, or halted
//! machine fails the request closed.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rish_core::{GuestCommand, HostReply};
use rish_guest_protocol::{
    DEFAULT_MAX_FRAME_SIZE, Envelope, Event, EventKind, FrameDecoder, FrameEncoder, FrameError,
    Message, Operation, Request, RequestId, Response, ResponseOutcome, ResponsePayload,
    StreamAction, StreamChannel, StreamRequest,
};
use rish_vm::{GuestChannel, GuestKernelEvidence, GuestSession, KernelEvidenceSource, VmError};
use serde_json::Value;

use crate::{EngineLimits, MachineState, SoftVmError, X86_64Machine};

/// Hard cap on live /proc/config.gz evidence.
const MAX_KERNEL_CONFIG_BYTES: usize = 4 * 1024 * 1024;
/// Per-stream output cap for one executed command.
const MAX_EXEC_STREAM_BYTES: usize = 64 * 1024 * 1024;
/// Absolute event count cap for one exchange.
const MAX_EVENTS_PER_EXCHANGE: usize = 100_000;

#[derive(Clone, Debug)]
struct NegotiatedSession {
    session_id: String,
}

struct Exchange {
    response: Response,
    events: Vec<Event>,
}

/// GuestChannel implementation that speaks the framed guest protocol over
/// the provider control serial. One request is in flight at a time.
pub struct SerialGuestTransport {
    machine: Arc<X86_64Machine>,
    limits: EngineLimits,
    in_flight: Mutex<()>,
    encoder: Mutex<FrameEncoder>,
    decoder: Mutex<FrameDecoder>,
    negotiated: Mutex<Option<NegotiatedSession>>,
    next_request_id: AtomicU64,
}

impl SerialGuestTransport {
    pub fn new(machine: X86_64Machine, limits: EngineLimits) -> Result<Self, SoftVmError> {
        limits.validate()?;
        Ok(Self {
            machine: Arc::new(machine),
            limits,
            in_flight: Mutex::new(()),
            encoder: Mutex::new(
                FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE)
                    .expect("default maximum frame size is valid"),
            ),
            decoder: Mutex::new(
                FrameDecoder::new(DEFAULT_MAX_FRAME_SIZE)
                    .expect("default maximum frame size is valid"),
            ),
            negotiated: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
        })
    }

    #[must_use]
    pub fn machine(&self) -> &X86_64Machine {
        &self.machine
    }

    /// Advances the guest by at most the given unit count and returns any
    /// complete frames decoded from the control channel.
    fn run_and_drain(&self, units: u64) -> Result<(crate::RunReport, Vec<Envelope>), VmError> {
        let report = self
            .machine
            .run_units(units)
            .map_err(|error| VmError::Guest(format!("provider run failed: {error}")))?;
        let mut frames = Vec::new();
        let bytes = self.machine.take_control();
        if !bytes.is_empty() {
            let mut decoder = self
                .decoder
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            decoder.push(&bytes).map_err(|error| frame_error(&error))?;
            while let Some(frame) = decoder.next_frame().map_err(|error| frame_error(&error))? {
                frames.push(frame);
            }
        }
        Ok((report, frames))
    }

    /// Pumps guest quanta until a frame matches the completion predicate or
    /// the unit budget is spent.
    fn pump(
        &self,
        unit_budget: u64,
        done: &dyn Fn(&Envelope) -> bool,
    ) -> Result<Vec<Envelope>, VmError> {
        let mut used = 0_u64;
        let mut collected: Vec<Envelope> = Vec::new();
        let mut last_dropped = self.machine.dropped_control_output();
        loop {
            if self.machine.dropped_control_output() != last_dropped {
                return Err(VmError::Guest(
                    "guest control channel dropped bytes; the transport fails closed".to_owned(),
                ));
            }
            if used >= unit_budget {
                return Err(SoftVmError::RequestDeadline { units: unit_budget }.into());
            }
            let (report, frames) = self.run_and_drain(self.limits.provider_quantum_units)?;
            used = used
                .checked_add(report.executed_units)
                .ok_or_else(|| VmError::Guest("provider unit counter overflow".to_owned()))?;
            match report.snapshot.state {
                MachineState::Running => {}
                MachineState::Halted | MachineState::Faulted => {
                    let state = match report.snapshot.state {
                        MachineState::Halted => "halted",
                        MachineState::Faulted => "faulted",
                        _ => unreachable!("state was matched above"),
                    };
                    return Err(VmError::Guest(format!(
                        "guest machine {state} while waiting for a control frame"
                    )));
                }
                MachineState::Stopped => {
                    return Err(VmError::Guest(
                        "guest machine stopped while waiting for a control frame".to_owned(),
                    ));
                }
            }
            last_dropped = self.machine.dropped_control_output();
            for frame in frames {
                collected.push(frame);
            }
            if collected.len() > MAX_EVENTS_PER_EXCHANGE {
                return Err(VmError::Protocol(
                    "guest emitted too many control frames for one exchange".to_owned(),
                ));
            }
            if collected.iter().any(done) {
                return Ok(collected);
            }
        }
    }

    /// Writes one encoded frame, advancing the guest when its read queue
    /// cannot accept the whole frame.
    fn write_frame(&self, envelope: &Envelope) -> Result<(), VmError> {
        let frame = {
            let encoder = self
                .encoder
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            encoder
                .encode(envelope)
                .map_err(|error| frame_error(&error))?
        };
        let mut offset = 0_usize;
        let mut used = 0_u64;
        while offset < frame.len() {
            offset += self.machine.write_control(&frame[offset..]);
            if offset < frame.len() {
                if used >= self.limits.max_units_per_request {
                    return Err(SoftVmError::RequestDeadline {
                        units: self.limits.max_units_per_request,
                    }
                    .into());
                }
                let (report, _) = self.run_and_drain(self.limits.provider_quantum_units)?;
                used = used
                    .checked_add(report.executed_units)
                    .ok_or_else(|| VmError::Guest("provider unit counter overflow".to_owned()))?;
            }
        }
        Ok(())
    }

    /// Sends one request and pumps until its Response frame arrives.
    fn request(&self, operation: Operation) -> Result<Exchange, VmError> {
        let id = RequestId::new(format!(
            "vm-req-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        ))
        .map_err(|error| VmError::Protocol(format!("invalid request id: {error}")))?;
        let request_id = id.clone();
        self.write_frame(&Envelope::new(Message::Request(Request { id, operation })))?;
        let frames = self.pump(self.limits.max_units_per_request, &|envelope| {
            matches!(
                &envelope.message,
                Message::Response(response) if response.id == request_id
            )
        })?;
        let response = frames
            .iter()
            .rev()
            .find_map(|envelope| match &envelope.message {
                Message::Response(response) if response.id == request_id => Some(response.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                VmError::Protocol("request exchange ended without a matching response".to_owned())
            })?;
        let events = frames
            .into_iter()
            .filter_map(|envelope| match envelope.message {
                Message::Event(event) => Some(event),
                _ => None,
            })
            .collect();
        Ok(Exchange { response, events })
    }

    /// Pumps until ProcessExited for the execution, appending collected events.
    fn collect_until_exit(
        &self,
        execution_id: &str,
        events: &mut Vec<Event>,
    ) -> Result<(), VmError> {
        let frames = self.pump(self.limits.max_units_per_request, &|envelope| {
            matches!(
                &envelope.message,
                Message::Event(Event {
                    event: EventKind::ProcessExited {
                        execution_id: candidate,
                        ..
                    },
                    ..
                }) if candidate == execution_id
            )
        })?;
        events.extend(
            frames
                .into_iter()
                .filter_map(|envelope| match envelope.message {
                    Message::Event(event) => Some(event),
                    _ => None,
                }),
        );
        Ok(())
    }

    /// Executes one command and aggregates its exit code and streams.
    fn execute_inner(&self, command: &GuestCommand) -> Result<HostReply, VmError> {
        let mut argv = Vec::with_capacity(command.args.len().saturating_add(1));
        argv.push(command.program.clone());
        argv.extend(command.args.iter().cloned());
        let attach_stdin = !command.stdin.is_empty();
        let exec = Operation::Exec(rish_guest_protocol::ExecRequest {
            argv,
            env: command.env.clone(),
            cwd: (command.cwd != "/").then(|| command.cwd.clone()),
            user: None,
            tty: false,
            attach_stdin,
            attach_stdout: true,
            attach_stderr: true,
            timeout_ms: None,
        });
        let mut exchange = self.request(exec)?;
        let execution_id = match exchange.response.outcome {
            ResponseOutcome::Success {
                result: ResponsePayload::ExecStarted { execution_id, .. },
            } => execution_id,
            ResponseOutcome::Success { .. } => {
                return Err(VmError::Protocol(
                    "unexpected response payload for Exec".to_owned(),
                ));
            }
            ResponseOutcome::Error { error } => {
                return Err(VmError::Guest(format!(
                    "guest rejected Exec: {} ({:?})",
                    error.message, error.code
                )));
            }
        };

        if attach_stdin {
            self.stream_stdin(&execution_id, &command.stdin, &mut exchange.events)?;
        }
        let already_exited = exchange.events.iter().any(|event| {
            matches!(
                &event.event,
                EventKind::ProcessExited {
                    execution_id: candidate,
                    ..
                } if candidate == &execution_id
            )
        });
        if !already_exited {
            self.collect_until_exit(&execution_id, &mut exchange.events)?;
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exited = None;
        for event in exchange.events {
            match event.event {
                EventKind::Stream {
                    execution_id: candidate,
                    channel,
                    data_base64,
                    ..
                } if candidate == execution_id => {
                    let decoded = BASE64.decode(&data_base64).map_err(|error| {
                        VmError::Protocol(format!("invalid stream payload: {error}"))
                    })?;
                    match channel {
                        StreamChannel::Stdout => push_bounded(&mut stdout, decoded)?,
                        StreamChannel::Stderr => push_bounded(&mut stderr, decoded)?,
                        StreamChannel::Console => {}
                    }
                }
                EventKind::ProcessExited {
                    execution_id: candidate,
                    exit_code,
                    signal,
                } if candidate == execution_id => {
                    if exited.is_some() {
                        return Err(VmError::Protocol(
                            "guest reported ProcessExited twice for one execution".to_owned(),
                        ));
                    }
                    exited = Some((exit_code, signal));
                }
                _ => {}
            }
        }
        let (exit_code, signal) = exited.ok_or_else(|| {
            VmError::Guest("execution stream ended without ProcessExited".to_owned())
        })?;
        let exit_code = exit_code.unwrap_or(signal.map_or(-1, |value| 128 + value));
        Ok(HostReply {
            exit_code,
            stdout,
            stderr,
            payload: Value::Null,
        })
    }

    fn stream_stdin(
        &self,
        execution_id: &str,
        stdin: &[u8],
        events: &mut Vec<Event>,
    ) -> Result<(), VmError> {
        let write = self.request(Operation::Stream(StreamRequest {
            execution_id: execution_id.to_owned(),
            action: StreamAction::WriteStdin {
                data_base64: BASE64.encode(stdin),
            },
        }))?;
        accept_stream(&write.response, execution_id)?;
        events.extend(write.events);
        let close = self.request(Operation::Stream(StreamRequest {
            execution_id: execution_id.to_owned(),
            action: StreamAction::CloseStdin,
        }))?;
        accept_stream(&close.response, execution_id)?;
        events.extend(close.events);
        Ok(())
    }

    fn require_session(&self) -> Result<NegotiatedSession, VmError> {
        self.negotiated
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| {
                VmError::Protocol(
                    "guest control request arrived before a negotiated session".to_owned(),
                )
            })
    }

    /// Runs a short read-only command and returns its stdout (kernel evidence).
    fn exec_text(&self, argv: &[&str]) -> Result<Vec<u8>, VmError> {
        let reply = self.execute_inner(&GuestCommand {
            program: argv[0].to_owned(),
            args: argv[1..].iter().map(|value| (*value).to_owned()).collect(),
            env: Default::default(),
            cwd: "/".to_owned(),
            stdin: Vec::new(),
        })?;
        if reply.exit_code != 0 {
            return Err(VmError::Guest(format!(
                "{} exited with {}: {}",
                argv[0],
                reply.exit_code,
                String::from_utf8_lossy(&reply.stderr)
            )));
        }
        if reply.stdout.len() > MAX_KERNEL_CONFIG_BYTES {
            return Err(VmError::Guest(format!(
                "{} output exceeds the kernel evidence limit",
                argv[0]
            )));
        }
        Ok(reply.stdout)
    }
}

impl GuestChannel for SerialGuestTransport {
    fn bootstrap(&self, hello: &Envelope) -> Result<Envelope, VmError> {
        let _guard = self
            .in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.write_frame(hello)?;
        let frames = self.pump(self.limits.max_units_per_request, &|envelope| {
            matches!(&envelope.message, Message::HelloAck(_))
        })?;
        let ack = frames
            .into_iter()
            .find(|envelope| matches!(&envelope.message, Message::HelloAck(_)))
            .ok_or_else(|| {
                VmError::Protocol("bootstrap exchange ended without HelloAck".to_owned())
            })?;
        if let Message::HelloAck(ack_message) = &ack.message {
            if let rish_guest_protocol::HandshakeOutcome::Accepted {
                selected_version,
                session_id,
                limits,
                ..
            } = &ack_message.outcome
            {
                let max_frame_size = limits.max_frame_size as usize;
                {
                    let mut decoder = self
                        .decoder
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    decoder.set_expected_version(Some(*selected_version));
                    decoder
                        .set_max_frame_size(max_frame_size)
                        .map_err(|error| frame_error(&error))?;
                }
                self.encoder
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .set_max_frame_size(max_frame_size)
                    .map_err(|error| frame_error(&error))?;
                *self
                    .negotiated
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(NegotiatedSession {
                    session_id: session_id.clone(),
                });
            }
        }
        Ok(ack)
    }

    fn kernel_config(&self, session: &GuestSession) -> Result<GuestKernelEvidence, VmError> {
        let _guard = self
            .in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let negotiated = self.require_session()?;
        if negotiated.session_id != session.id() {
            return Err(VmError::KernelEvidenceSessionMismatch {
                expected: session.id().to_owned(),
                actual: negotiated.session_id,
            });
        }
        let release_bytes = self.exec_text(&["uname", "-r"])?;
        let kernel_release = String::from_utf8(release_bytes)
            .map_err(|error| VmError::Guest(format!("kernel release is not UTF-8: {error}")))?
            .trim()
            .to_owned();
        if kernel_release.is_empty() {
            return Err(VmError::Guest(
                "guest returned an empty kernel release".to_owned(),
            ));
        }
        let config_bytes = self.exec_text(&["zcat", "/proc/config.gz"])?;
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
        self.require_session()?;
        self.execute_inner(command)
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

fn push_bounded(target: &mut Vec<u8>, bytes: Vec<u8>) -> Result<(), VmError> {
    if target.len().saturating_add(bytes.len()) > MAX_EXEC_STREAM_BYTES {
        return Err(VmError::Guest(format!(
            "execution stream exceeded {MAX_EXEC_STREAM_BYTES} bytes"
        )));
    }
    target.extend_from_slice(&bytes);
    Ok(())
}

fn accept_stream(response: &Response, execution_id: &str) -> Result<(), VmError> {
    match &response.outcome {
        ResponseOutcome::Success {
            result:
                ResponsePayload::StreamAccepted {
                    execution_id: candidate,
                },
        } if candidate == execution_id => Ok(()),
        ResponseOutcome::Success { .. } => Err(VmError::Protocol(
            "unexpected response payload for Stream".to_owned(),
        )),
        ResponseOutcome::Error { error } => Err(VmError::Guest(format!(
            "guest rejected Stream for {execution_id}: {} ({:?})",
            error.message, error.code
        ))),
    }
}

fn frame_error(error: &FrameError) -> VmError {
    VmError::Protocol(format!("guest control frame error: {error}"))
}

impl From<SoftVmError> for VmError {
    fn from(error: SoftVmError) -> Self {
        VmError::Guest(error.to_string())
    }
}
