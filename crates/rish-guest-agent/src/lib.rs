//! Bootstrap control-plane agent that runs inside the Linux guest.
//!
//! Request dispatch is synchronous, but process execution is not. `Exec`
//! starts a supervised process and returns immediately; callers drive
//! completion, output, cancellation, and deadlines through [`GuestAgent::poll`].

mod oci_runtime;
mod probe;
mod supervisor;

use std::collections::{BTreeMap, BTreeSet};

use rish_guest_protocol::{
    Capability, CapabilityStatus, Envelope, ErrorCode, Event, EventKind, GuestCapabilities,
    GuestLimits, HandshakeOutcome, Hello, HelloAck, MIN_NEGOTIATED_FRAME_SIZE, Message, Operation,
    PeerInfo, PeerRole, ProtocolVersion, RemoteError, Request, RequestId, Response,
    ResponsePayload, SUPPORTED_PROTOCOL_VERSIONS, capability_name, negotiate_version,
};
use thiserror::Error;

pub use oci_runtime::{OciLifecycleReply, OciRuntimeBackend, OciRuntimeConfig};
pub use supervisor::{
    DEFAULT_MAX_CONCURRENT_EXEC, DEFAULT_STREAM_CHUNK_SIZE, DEFAULT_STREAM_OUTPUT_LIMIT,
    NativeExecutionConfig, NativeOperationHandler,
};

use probe::GuestProbe;

const MAX_REQUESTED_CAPABILITIES: usize = 64;
const MAX_CAPABILITY_NAME_LENGTH: usize = 128;
const MAX_EVENTS_PER_EXEC_POLL: usize = 4;
const ABSOLUTE_MAX_EVENTS_PER_POLL: usize = 256;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("unexpected message from host: {0}")]
    UnexpectedMessage(&'static str),

    #[error("host must complete a handshake before sending requests")]
    HandshakeRequired,

    #[error("host attempted to negotiate an already established session")]
    DuplicateHandshake,

    #[error("session protocol version mismatch: expected {expected}, received {received}")]
    ProtocolVersionMismatch {
        expected: ProtocolVersion,
        received: ProtocolVersion,
    },
}

#[derive(Debug)]
pub struct HandlerReply {
    pub response: ResponsePayload,
    pub events: Vec<EventKind>,
}

/// A deferred event and the request that owns it.
#[derive(Debug)]
pub struct HandlerEvent {
    pub request_id: Option<RequestId>,
    pub event: EventKind,
}

/// Operation backend used by [`GuestAgent`].
///
/// `handle` must not wait for a long-running operation. Deferred work is
/// advanced one event at a time by `poll_event`; returning one item per call
/// gives the session a hard output budget without an unbounded handler queue.
pub trait OperationHandler {
    fn handle(
        &mut self,
        request_id: &RequestId,
        operation: &Operation,
    ) -> Result<HandlerReply, RemoteError>;

    fn poll_event(&mut self) -> Option<HandlerEvent> {
        None
    }
}

pub struct GuestAgent<H> {
    handler: H,
    peer: PeerInfo,
    capabilities: GuestCapabilities,
    limits: GuestLimits,
    session_id: String,
    negotiated_version: Option<ProtocolVersion>,
    negotiated_max_frame_size: Option<u32>,
    next_event_sequence: u64,
}

impl<H: OperationHandler> GuestAgent<H> {
    #[must_use]
    pub fn new(
        handler: H,
        peer: PeerInfo,
        capabilities: GuestCapabilities,
        limits: GuestLimits,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            handler,
            peer,
            capabilities,
            limits,
            session_id: session_id.into(),
            negotiated_version: None,
            negotiated_max_frame_size: None,
            next_event_sequence: 1,
        }
    }

    #[must_use]
    pub fn negotiated_version(&self) -> Option<ProtocolVersion> {
        self.negotiated_version
    }

    #[must_use]
    pub fn negotiated_max_frame_size(&self) -> Option<u32> {
        self.negotiated_max_frame_size
    }

    /// Handles one inbound control envelope.
    ///
    /// Long-running operations return their initial response here and emit
    /// later events from [`Self::poll`].
    pub fn handle(&mut self, envelope: Envelope) -> Result<Vec<Envelope>, AgentError> {
        let envelope_version = envelope.version;
        if let Some(expected) = self.negotiated_version {
            if envelope_version != expected {
                return Err(AgentError::ProtocolVersionMismatch {
                    expected,
                    received: envelope_version,
                });
            }
        }

        match envelope.message {
            Message::Hello(hello) => self.handle_hello(hello, envelope_version),
            Message::Request(request) => {
                let Some(version) = self.negotiated_version else {
                    return Err(AgentError::HandshakeRequired);
                };
                self.handle_request(request, version)
            }
            Message::HelloAck(_) => Err(AgentError::UnexpectedMessage("hello_ack")),
            Message::Response(_) => Err(AgentError::UnexpectedMessage("response")),
            Message::Event(_) => Err(AgentError::UnexpectedMessage("event")),
        }
    }

    /// Advances deferred operations without waiting.
    ///
    /// The returned batch is bounded by the negotiated execution limit and an
    /// absolute protocol safety ceiling. Call this periodically even when the
    /// transport has no inbound frames so deadlines and process exits progress.
    pub fn poll(&mut self) -> Vec<Envelope> {
        let Some(version) = self.negotiated_version else {
            return Vec::new();
        };
        let budget = usize::try_from(self.limits.max_concurrent_exec)
            .unwrap_or(ABSOLUTE_MAX_EVENTS_PER_POLL)
            .saturating_mul(MAX_EVENTS_PER_EXEC_POLL)
            .clamp(1, ABSOLUTE_MAX_EVENTS_PER_POLL);
        let mut output = Vec::with_capacity(budget);
        for _ in 0..budget {
            let Some(event) = self.handler.poll_event() else {
                break;
            };
            output.push(self.event_envelope(version, event.request_id, event.event));
        }
        output
    }

    fn handle_hello(
        &mut self,
        hello: Hello,
        envelope_version: ProtocolVersion,
    ) -> Result<Vec<Envelope>, AgentError> {
        if self.negotiated_version.is_some() {
            return Err(AgentError::DuplicateHandshake);
        }
        if hello.role != PeerRole::Host {
            return Err(AgentError::UnexpectedMessage("guest hello"));
        }

        let (response_version, outcome) = self.negotiate_hello(&hello, envelope_version);
        Ok(vec![Envelope::with_version(
            response_version,
            Message::HelloAck(HelloAck {
                request_id: hello.request_id,
                outcome,
            }),
        )])
    }

    fn negotiate_hello(
        &mut self,
        hello: &Hello,
        envelope_version: ProtocolVersion,
    ) -> (ProtocolVersion, HandshakeOutcome) {
        let current = rish_guest_protocol::CURRENT_PROTOCOL_VERSION;
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&envelope_version)
            || !hello.supported_versions.contains(&envelope_version)
        {
            return (
                current,
                rejected(
                    ErrorCode::VersionMismatch,
                    format!(
                        "hello envelope version {envelope_version} is not a mutually advertised bootstrap version"
                    ),
                ),
            );
        }

        let minimum_frame_size = MIN_NEGOTIATED_FRAME_SIZE as u32;
        if hello.max_frame_size < minimum_frame_size
            || self.limits.max_frame_size < minimum_frame_size
        {
            return (
                current,
                rejected(
                    ErrorCode::InvalidRequest,
                    format!("maximum frame size must be at least {minimum_frame_size} bytes"),
                ),
            );
        }

        if let Err(error) = self.validate_requested_capabilities(&hello.requested_capabilities) {
            return (current, HandshakeOutcome::Rejected { error });
        }

        let selected_version =
            match negotiate_version(SUPPORTED_PROTOCOL_VERSIONS, &hello.supported_versions) {
                Ok(version) => version,
                Err(error) => {
                    return (
                        current,
                        rejected(ErrorCode::VersionMismatch, error.to_string()),
                    );
                }
            };
        let max_frame_size = hello.max_frame_size.min(self.limits.max_frame_size);
        let mut limits = self.limits.clone();
        limits.max_frame_size = max_frame_size;
        self.negotiated_version = Some(selected_version);
        self.negotiated_max_frame_size = Some(max_frame_size);
        (
            selected_version,
            HandshakeOutcome::Accepted {
                selected_version,
                session_id: self.session_id.clone(),
                peer: self.peer.clone(),
                capabilities: Box::new(self.capabilities.clone()),
                limits,
            },
        )
    }

    fn validate_requested_capabilities(&self, requested: &[String]) -> Result<(), RemoteError> {
        if requested.len() > MAX_REQUESTED_CAPABILITIES {
            return Err(RemoteError::new(
                ErrorCode::InvalidRequest,
                format!(
                    "requested capability count {} exceeds limit {MAX_REQUESTED_CAPABILITIES}",
                    requested.len()
                ),
            ));
        }

        let mut unique = BTreeSet::new();
        for name in requested {
            if name.trim().is_empty() {
                return Err(RemoteError::new(
                    ErrorCode::InvalidRequest,
                    "requested capability name must not be empty",
                ));
            }
            if name.len() > MAX_CAPABILITY_NAME_LENGTH {
                return Err(RemoteError::new(
                    ErrorCode::InvalidRequest,
                    format!("requested capability name exceeds {MAX_CAPABILITY_NAME_LENGTH} bytes"),
                ));
            }
            if !unique.insert(name.as_str()) {
                return Err(RemoteError::new(
                    ErrorCode::InvalidRequest,
                    format!("requested capability {name} is duplicated"),
                ));
            }
        }

        for name in requested {
            self.require_available_capability(name)?;
        }
        Ok(())
    }

    fn handle_request(
        &mut self,
        request: Request,
        version: ProtocolVersion,
    ) -> Result<Vec<Envelope>, AgentError> {
        let request_id = request.id.clone();
        let result = match required_capability(&request.operation) {
            Some(name) => match self.require_available_capability(name) {
                Ok(()) => self.handler.handle(&request.id, &request.operation),
                Err(error) => Err(error),
            },
            None => match &request.operation {
                Operation::Ping(ping) => Ok(HandlerReply {
                    response: ResponsePayload::Pong {
                        nonce: ping.nonce.clone(),
                    },
                    events: Vec::new(),
                }),
                _ => unreachable!("all non-ping operations require a capability"),
            },
        };

        let (response, events) = match result {
            Ok(reply) => (Response::success(request.id, reply.response), reply.events),
            Err(error) => (Response::error(request.id, error), Vec::new()),
        };

        let mut output = Vec::with_capacity(events.len().saturating_add(1));
        output.push(Envelope::with_version(version, Message::Response(response)));
        output.extend(
            events
                .into_iter()
                .map(|event| self.event_envelope(version, Some(request_id.clone()), event)),
        );
        Ok(output)
    }

    fn event_envelope(
        &mut self,
        version: ProtocolVersion,
        request_id: Option<RequestId>,
        event: EventKind,
    ) -> Envelope {
        let sequence = self.next_event_sequence;
        self.next_event_sequence = self.next_event_sequence.saturating_add(1);
        Envelope::with_version(
            version,
            Message::Event(Event::now(sequence, request_id, event)),
        )
    }

    fn require_available_capability(&self, name: &str) -> Result<(), RemoteError> {
        match self.capabilities.capability(name) {
            Some(Capability {
                version: 1,
                status: CapabilityStatus::Available,
                ..
            }) => Ok(()),
            Some(capability) => {
                let reason = capability
                    .reason
                    .as_deref()
                    .unwrap_or("guest reported the capability as unavailable");
                Err(RemoteError::new(
                    ErrorCode::CapabilityUnavailable,
                    format!(
                        "capability {name} version {} is {:?}: {reason}; this agent supports version 1",
                        capability.version, capability.status
                    ),
                ))
            }
            None => Err(RemoteError::new(
                ErrorCode::CapabilityUnavailable,
                format!("capability {name} was not negotiated"),
            )),
        }
    }
}

fn rejected(code: ErrorCode, message: impl Into<String>) -> HandshakeOutcome {
    HandshakeOutcome::Rejected {
        error: RemoteError::new(code, message),
    }
}

fn required_capability(operation: &Operation) -> Option<&'static str> {
    match operation {
        Operation::Exec(_) | Operation::Stream(_) | Operation::Cancel(_) => {
            Some(capability_name::EXEC)
        }
        Operation::OciPrepare(_)
        | Operation::OciRun(_)
        | Operation::OciStop(_)
        | Operation::OciDelete(_) => Some(capability_name::OCI),
        Operation::PortForward(_) => Some(capability_name::PORT_FORWARDING),
        Operation::Checkpoint(_) => Some(capability_name::CHECKPOINT_RESTORE),
        Operation::Ping(_) => None,
    }
}

#[must_use]
pub fn bootstrap_agent() -> GuestAgent<NativeOperationHandler> {
    build_agent(GuestProbe::conservative())
}

/// Builds the agent used by the guest PID 1 process.
///
/// Unlike [`bootstrap_agent`], this constructor reads the guest's live
/// `/proc`, `/sys`, and `/run` mounts. A capability is advertised only after
/// those observations and the runtime executable pass the same fail-closed
/// policy used by the handler.
#[must_use]
pub fn bootstrap_guest_agent() -> GuestAgent<NativeOperationHandler> {
    build_agent(GuestProbe::detect())
}

fn build_agent(probe: GuestProbe) -> GuestAgent<NativeOperationHandler> {
    let execution_config = NativeExecutionConfig::default();
    let (handler, oci_status, oci_reason, oci_attributes) = match probe.oci_runtime.as_ref() {
        Some(runtime) => {
            let config = OciRuntimeConfig::new(&runtime.path, &runtime.bundle_root)
                .with_runtime_root(&runtime.runtime_root);
            match OciRuntimeBackend::new(config) {
                Ok(backend) => (
                    NativeOperationHandler::with_oci_backend(execution_config.clone(), backend)
                        .expect("validated OCI backend configuration must be accepted"),
                    CapabilityStatus::Available,
                    None,
                    BTreeMap::from([
                        ("runtime".to_owned(), runtime.name.clone().into()),
                        (
                            "runtime_path".to_owned(),
                            runtime.path.display().to_string().into(),
                        ),
                        ("supports_attach".to_owned(), false.into()),
                    ]),
                ),
                Err(error) => (
                    NativeOperationHandler::new(execution_config.clone())
                        .expect("default native execution configuration is valid"),
                    CapabilityStatus::Unavailable,
                    Some(format!(
                        "OCI runtime backend initialization failed: {error:?}"
                    )),
                    BTreeMap::new(),
                ),
            }
        }
        None => (
            NativeOperationHandler::new(execution_config.clone())
                .expect("default native execution configuration is valid"),
            CapabilityStatus::Unavailable,
            Some(probe.oci_reason.clone()),
            BTreeMap::new(),
        ),
    };

    let mut features = Vec::with_capacity(7);
    features.push(Capability {
        name: capability_name::EXEC.to_owned(),
        version: 1,
        status: CapabilityStatus::Available,
        attributes: execution_attributes(&execution_config),
        reason: None,
    });
    features.push(feature(
        capability_name::NAMESPACES,
        probe.namespaces_available,
        "required Linux namespace handles are unavailable",
        BTreeMap::new(),
    ));
    features.push(feature(
        capability_name::CGROUPS_V2,
        probe.cgroup_version == Some(2),
        "cgroup v2 is not mounted and readable",
        BTreeMap::from([("version".to_owned(), 2.into())]),
    ));
    features.push(Capability {
        name: capability_name::OCI.to_owned(),
        version: 1,
        status: oci_status,
        attributes: oci_attributes,
        reason: oci_reason,
    });
    features.push(Capability {
        name: capability_name::SYSTEMD.to_owned(),
        version: 1,
        status: if probe.systemd_available {
            CapabilityStatus::Available
        } else {
            CapabilityStatus::Unavailable
        },
        attributes: BTreeMap::from([("pid1".to_owned(), probe.init_system.clone().into())]),
        reason: (!probe.systemd_available).then_some(probe.systemd_reason.clone()),
    });
    features.push(Capability {
        name: capability_name::NESTED_CONTAINERS.to_owned(),
        version: 1,
        status: CapabilityStatus::Unavailable,
        attributes: BTreeMap::new(),
        reason: Some("nested container dispatch is not implemented".to_owned()),
    });

    GuestAgent::new(
        handler,
        PeerInfo {
            name: "rish-guest-agent".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        },
        GuestCapabilities {
            kernel_release: probe.kernel_release,
            architecture: std::env::consts::ARCH.to_owned(),
            init_system: probe.init_system,
            cgroup_version: probe.cgroup_version,
            container_runtimes: probe.installed_runtimes,
            features,
        },
        GuestLimits {
            max_frame_size: rish_guest_protocol::DEFAULT_MAX_FRAME_SIZE as u32,
            max_concurrent_exec: execution_config.max_concurrent_exec,
            max_port_forwards: 0,
            max_stream_chunk_size: execution_config.max_stream_chunk_size,
        },
        "bootstrap-session".to_owned(),
    )
}

fn execution_attributes(config: &NativeExecutionConfig) -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        ("execution_mode".to_owned(), "supervised_async".into()),
        ("supports_cancel".to_owned(), true.into()),
        ("supports_timeout".to_owned(), true.into()),
        ("supports_stdin_stream".to_owned(), cfg!(unix).into()),
        ("supports_tty".to_owned(), cfg!(target_os = "linux").into()),
        (
            "tty_stream_channel".to_owned(),
            if cfg!(target_os = "linux") {
                "console"
            } else {
                "unavailable"
            }
            .into(),
        ),
        (
            "max_stdout_bytes".to_owned(),
            config.max_stdout_bytes.into(),
        ),
        (
            "max_stderr_bytes".to_owned(),
            config.max_stderr_bytes.into(),
        ),
    ])
}

fn feature(
    name: &str,
    available: bool,
    unavailable_reason: &str,
    attributes: BTreeMap<String, serde_json::Value>,
) -> Capability {
    Capability {
        name: name.to_owned(),
        version: 1,
        status: if available {
            CapabilityStatus::Available
        } else {
            CapabilityStatus::Unavailable
        },
        attributes,
        reason: (!available).then(|| unavailable_reason.to_owned()),
    }
}

#[cfg(test)]
mod tests;
