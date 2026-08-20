//! Host-side session state machine for the versioned guest protocol.
//!
//! This module owns framing, request correlation, stream aggregation, and
//! deadlines so the production VM transport and diagnostic boot harnesses
//! share one implementation. Byte I/O is injected through SessionIo; the
//! client never blocks, advances the guest in bounded steps, and fails closed
//! on drops, malformed frames, or deadline breaches.

use std::{collections::BTreeMap, fmt};

use crate::{
    DEFAULT_MAX_FRAME_SIZE, Envelope, Event, EventKind, FrameDecoder, FrameEncoder, FrameError,
    HandshakeOutcome, Message, Operation, ProtocolVersion, Request, RequestId, Response,
    ResponseOutcome, ResponsePayload, StreamAction, StreamChannel, StreamRequest,
};

/// Hard cap on one execution stream in bytes.
pub const MAX_EXEC_STREAM_BYTES: usize = 64 * 1024 * 1024;
/// Absolute frame count cap for one exchange.
pub const MAX_FRAMES_PER_EXCHANGE: usize = 100_000;

/// Byte I/O hooks injected by the transport layer.
///
/// write pushes host frames toward the guest and returns the accepted count.
/// advance runs the guest for one bounded step and returns guest bytes drained
/// during that step; an Err stops the exchange. dropped_output reports bytes
/// the guest lost because the host queue was full.
pub trait SessionIo {
    fn write(&self, bytes: &[u8]) -> usize;
    fn advance(&self) -> Result<Vec<u8>, String>;
    fn dropped_output(&self) -> u64;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedSession {
    pub session_id: String,
    pub version: ProtocolVersion,
    pub max_frame_size: u32,
}

#[derive(Debug)]
pub enum SessionError {
    Frame(FrameError),
    Advance(String),
    Deadline { advances: u64 },
    DroppedBytes,
    TooManyFrames,
    MissingResponse,
    UnexpectedResponse,
    GuestRejected(String),
    Stream(String),
    StreamCap,
    NotNegotiated,
    InvalidBudget,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "frame error: {error}"),
            Self::Advance(message) => write!(formatter, "guest advance failed: {message}"),
            Self::Deadline { advances } => {
                write!(formatter, "exchange exceeded the {advances} step budget")
            }
            Self::DroppedBytes => write!(
                formatter,
                "guest control bytes were dropped; the session fails closed"
            ),
            Self::TooManyFrames => write!(formatter, "guest emitted too many frames"),
            Self::MissingResponse => write!(formatter, "exchange ended without a response"),
            Self::UnexpectedResponse => write!(formatter, "unexpected response payload"),
            Self::GuestRejected(message) => {
                write!(formatter, "guest rejected the request: {message}")
            }
            Self::Stream(message) => write!(formatter, "guest stream error: {message}"),
            Self::StreamCap => write!(formatter, "execution stream exceeded its byte cap"),
            Self::NotNegotiated => write!(formatter, "session was not negotiated"),
            Self::InvalidBudget => write!(formatter, "session step budget must be non-zero"),
        }
    }
}

impl std::error::Error for SessionError {}

/// The result of one command execution, protocol-level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// One request/response exchange with the events observed while waiting.
#[derive(Clone, Debug)]
pub struct Exchange {
    pub response: Response,
    pub events: Vec<Event>,
}

/// Stateful client for one guest session.
pub struct SessionClient {
    encoder: FrameEncoder,
    decoder: FrameDecoder,
    negotiated: Option<NegotiatedSession>,
    next_request_id: u64,
    max_advances_per_request: u64,
}

impl SessionClient {
    pub fn new(max_advances_per_request: u64) -> Result<Self, SessionError> {
        if max_advances_per_request == 0 {
            return Err(SessionError::InvalidBudget);
        }
        Ok(Self {
            encoder: FrameEncoder::new(DEFAULT_MAX_FRAME_SIZE).map_err(SessionError::Frame)?,
            decoder: FrameDecoder::new(DEFAULT_MAX_FRAME_SIZE).map_err(SessionError::Frame)?,
            negotiated: None,
            next_request_id: 1,
            max_advances_per_request,
        })
    }

    #[must_use]
    pub fn negotiated(&self) -> Option<&NegotiatedSession> {
        self.negotiated.as_ref()
    }

    /// Exchanges the bootstrap Hello and applies the negotiated limits.
    pub fn bootstrap<I: SessionIo>(
        &mut self,
        hello: &Envelope,
        io: &I,
    ) -> Result<Envelope, SessionError> {
        let budget = self.max_advances_per_request;
        self.write_frame(hello, io, budget)?;
        let frames = self.pump(io, budget, &|envelope| {
            matches!(&envelope.message, Message::HelloAck(_))
        })?;
        let ack = frames
            .into_iter()
            .find(|envelope| matches!(&envelope.message, Message::HelloAck(_)))
            .ok_or(SessionError::MissingResponse)?;
        if let Message::HelloAck(ack_message) = &ack.message {
            if let HandshakeOutcome::Accepted {
                selected_version,
                session_id,
                limits,
                ..
            } = &ack_message.outcome
            {
                let max_frame_size = limits.max_frame_size as usize;
                self.decoder.set_expected_version(Some(*selected_version));
                self.decoder
                    .set_max_frame_size(max_frame_size)
                    .map_err(SessionError::Frame)?;
                self.encoder
                    .set_max_frame_size(max_frame_size)
                    .map_err(SessionError::Frame)?;
                self.negotiated = Some(NegotiatedSession {
                    session_id: session_id.clone(),
                    version: *selected_version,
                    max_frame_size: limits.max_frame_size,
                });
            }
        }
        Ok(ack)
    }

    /// Sends one request and pumps until its Response frame arrives.
    pub fn request<I: SessionIo>(
        &mut self,
        operation: Operation,
        io: &I,
    ) -> Result<Exchange, SessionError> {
        self.require_negotiated()?;
        let id = RequestId::new(format!("vm-req-{}", self.next_request_id))
            .map_err(|error| SessionError::Stream(error.to_string()))?;
        self.next_request_id = self.next_request_id.saturating_add(1);
        let request_id = id.clone();
        let version = self
            .negotiated
            .as_ref()
            .expect("negotiated session was checked")
            .version;
        self.write_frame(
            &Envelope::with_version(version, Message::Request(Request { id, operation })),
            io,
            self.max_advances_per_request,
        )?;
        let frames = self.pump(io, self.max_advances_per_request, &|envelope| {
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
            .ok_or(SessionError::MissingResponse)?;
        let events = frames
            .into_iter()
            .filter_map(|envelope| match envelope.message {
                Message::Event(event) => Some(event),
                _ => None,
            })
            .collect();
        Ok(Exchange { response, events })
    }

    /// Pumps until ProcessExited for the execution, appending observed events.
    pub fn collect_until_exit<I: SessionIo>(
        &mut self,
        execution_id: &str,
        events: &mut Vec<Event>,
        io: &I,
    ) -> Result<(), SessionError> {
        let frames = self.pump(io, self.max_advances_per_request, &|envelope| {
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

    /// Executes one command and aggregates exit code and streams.
    pub fn execute<I: SessionIo>(
        &mut self,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
        stdin: Vec<u8>,
        io: &I,
    ) -> Result<ExecOutcome, SessionError> {
        let attach_stdin = !stdin.is_empty();
        let exec = Operation::Exec(crate::ExecRequest {
            argv,
            env,
            cwd,
            user: None,
            tty: false,
            attach_stdin,
            attach_stdout: true,
            attach_stderr: true,
            timeout_ms: None,
        });
        let mut exchange = self.request(exec, io)?;
        let execution_id = match exchange.response.outcome {
            ResponseOutcome::Success {
                result: ResponsePayload::ExecStarted { execution_id, .. },
            } => execution_id,
            ResponseOutcome::Success { .. } => return Err(SessionError::UnexpectedResponse),
            ResponseOutcome::Error { error } => {
                return Err(SessionError::GuestRejected(format!(
                    "{} ({:?})",
                    error.message, error.code
                )));
            }
        };

        if attach_stdin {
            self.stream_stdin(&execution_id, &stdin, &mut exchange.events, io)?;
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
            self.collect_until_exit(&execution_id, &mut exchange.events, io)?;
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
                    let decoded = decode_base64(&data_base64)?;
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
                        return Err(SessionError::Stream(
                            "guest reported ProcessExited twice".to_owned(),
                        ));
                    }
                    exited = Some((exit_code, signal));
                }
                _ => {}
            }
        }
        let (exit_code, signal) =
            exited.ok_or_else(|| SessionError::Stream("no ProcessExited".to_owned()))?;
        Ok(ExecOutcome {
            exit_code,
            signal,
            stdout,
            stderr,
        })
    }

    fn stream_stdin<I: SessionIo>(
        &mut self,
        execution_id: &str,
        stdin: &[u8],
        events: &mut Vec<Event>,
        io: &I,
    ) -> Result<(), SessionError> {
        let write = self.request(
            Operation::Stream(StreamRequest {
                execution_id: execution_id.to_owned(),
                action: StreamAction::WriteStdin {
                    data_base64: encode_base64(stdin),
                },
            }),
            io,
        )?;
        accept_stream(&write.response, execution_id)?;
        events.extend(write.events);
        let close = self.request(
            Operation::Stream(StreamRequest {
                execution_id: execution_id.to_owned(),
                action: StreamAction::CloseStdin,
            }),
            io,
        )?;
        accept_stream(&close.response, execution_id)?;
        events.extend(close.events);
        Ok(())
    }

    fn require_negotiated(&self) -> Result<(), SessionError> {
        if self.negotiated.is_none() {
            return Err(SessionError::NotNegotiated);
        }
        Ok(())
    }

    fn write_frame<I: SessionIo>(
        &mut self,
        envelope: &Envelope,
        io: &I,
        max_advances: u64,
    ) -> Result<(), SessionError> {
        let frame = self.encoder.encode(envelope).map_err(SessionError::Frame)?;
        let mut offset = 0_usize;
        let mut advances = 0_u64;
        while offset < frame.len() {
            offset += io.write(&frame[offset..]);
            if offset < frame.len() {
                if advances >= max_advances {
                    return Err(SessionError::Deadline { advances });
                }
                io.advance().map_err(SessionError::Advance)?;
                advances = advances.saturating_add(1);
            }
        }
        Ok(())
    }

    fn pump<I: SessionIo>(
        &mut self,
        io: &I,
        max_advances: u64,
        done: &dyn Fn(&Envelope) -> bool,
    ) -> Result<Vec<Envelope>, SessionError> {
        let mut advances = 0_u64;
        let mut collected: Vec<Envelope> = Vec::new();
        let mut last_dropped = io.dropped_output();
        loop {
            if io.dropped_output() != last_dropped {
                return Err(SessionError::DroppedBytes);
            }
            if advances >= max_advances {
                return Err(SessionError::Deadline { advances });
            }
            let bytes = io.advance().map_err(SessionError::Advance)?;
            if !bytes.is_empty() {
                self.decoder.push(&bytes).map_err(SessionError::Frame)?;
                while let Some(frame) = self.decoder.next_frame().map_err(SessionError::Frame)? {
                    collected.push(frame);
                }
            }
            if collected.len() > MAX_FRAMES_PER_EXCHANGE {
                return Err(SessionError::TooManyFrames);
            }
            if collected.iter().any(done) {
                return Ok(collected);
            }
            last_dropped = io.dropped_output();
            advances = advances.saturating_add(1);
        }
    }
}

fn push_bounded(target: &mut Vec<u8>, bytes: Vec<u8>) -> Result<(), SessionError> {
    if target.len().saturating_add(bytes.len()) > MAX_EXEC_STREAM_BYTES {
        return Err(SessionError::StreamCap);
    }
    target.extend_from_slice(&bytes);
    Ok(())
}

/// Encodes bytes with the standard base64 alphabet used on the wire.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes the standard base64 alphabet used on the wire.
pub fn decode_base64(value: &str) -> Result<Vec<u8>, SessionError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| SessionError::Stream(format!("invalid base64 payload: {error}")))
}

fn accept_stream(response: &Response, execution_id: &str) -> Result<(), SessionError> {
    match &response.outcome {
        ResponseOutcome::Success {
            result:
                ResponsePayload::StreamAccepted {
                    execution_id: candidate,
                },
        } if candidate == execution_id => Ok(()),
        ResponseOutcome::Success { .. } => Err(SessionError::UnexpectedResponse),
        ResponseOutcome::Error { error } => Err(SessionError::Stream(format!(
            "{} ({:?})",
            error.message, error.code
        ))),
    }
}
