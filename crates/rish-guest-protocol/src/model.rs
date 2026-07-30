use std::{
    collections::BTreeMap,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    CURRENT_PROTOCOL_VERSION, NegotiationError, PROTOCOL_ID, RequestId, SUPPORTED_PROTOCOL_VERSIONS,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

/// Returns the newest version both peers explicitly advertise.
pub fn negotiate_version(
    local: &[ProtocolVersion],
    remote: &[ProtocolVersion],
) -> Result<ProtocolVersion, NegotiationError> {
    local
        .iter()
        .filter(|version| remote.contains(version))
        .copied()
        .max()
        .ok_or_else(|| NegotiationError {
            local: local.to_vec(),
            remote: remote.to_vec(),
        })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: String,
    pub version: ProtocolVersion,
    #[serde(flatten)]
    pub message: Message,
}

impl Envelope {
    #[must_use]
    pub fn new(message: Message) -> Self {
        Self {
            protocol: PROTOCOL_ID.to_owned(),
            version: CURRENT_PROTOCOL_VERSION,
            message,
        }
    }

    #[must_use]
    pub fn with_version(version: ProtocolVersion, message: Message) -> Self {
        Self {
            protocol: PROTOCOL_ID.to_owned(),
            version,
            message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message_type", content = "payload", rename_all = "snake_case")]
pub enum Message {
    Hello(Hello),
    HelloAck(HelloAck),
    Request(Request),
    Response(Response),
    Event(Event),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerRole {
    Host,
    Guest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub architecture: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub request_id: RequestId,
    pub role: PeerRole,
    pub peer: PeerInfo,
    pub supported_versions: Vec<ProtocolVersion>,
    pub requested_capabilities: Vec<String>,
    pub max_frame_size: u32,
}

impl Hello {
    pub fn host(
        request_id: RequestId,
        peer: PeerInfo,
        requested_capabilities: Vec<String>,
        max_frame_size: u32,
    ) -> Self {
        Self {
            request_id,
            role: PeerRole::Host,
            peer,
            supported_versions: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
            requested_capabilities,
            max_frame_size,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HelloAck {
    pub request_id: RequestId,
    #[serde(flatten)]
    pub outcome: HandshakeOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HandshakeOutcome {
    Accepted {
        selected_version: ProtocolVersion,
        session_id: String,
        peer: PeerInfo,
        capabilities: Box<GuestCapabilities>,
        limits: GuestLimits,
    },
    Rejected {
        error: RemoteError,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuestCapabilities {
    pub kernel_release: String,
    pub architecture: String,
    pub init_system: String,
    pub cgroup_version: Option<u8>,
    pub container_runtimes: Vec<String>,
    pub features: Vec<Capability>,
}

impl GuestCapabilities {
    #[must_use]
    pub fn capability(&self, name: &str) -> Option<&Capability> {
        self.features.iter().find(|feature| feature.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub version: u16,
    pub status: CapabilityStatus,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Available,
    Restricted,
    Unavailable,
}

pub mod capability_name {
    pub const EXEC: &str = "process.exec";
    pub const OCI: &str = "container.oci";
    pub const VIRTUAL_FILESYSTEM: &str = "filesystem.virtual";
    pub const PRIVILEGED_CONTAINERS: &str = "container.privileged";
    pub const NESTED_CONTAINERS: &str = "container.nested";
    pub const NAMESPACES: &str = "kernel.namespaces";
    pub const CGROUPS_V2: &str = "kernel.cgroups_v2";
    pub const MODULES: &str = "kernel.modules";
    pub const DEVICES: &str = "kernel.devices";
    pub const SYSTEMD: &str = "systemd";
    pub const PORT_FORWARDING: &str = "network.port_forwarding";
    pub const NETWORK_NAMESPACES: &str = "network.namespaces";
    pub const RAW_SOCKETS: &str = "network.raw_sockets";
    pub const TUN_TAP: &str = "network.tun_tap";
    pub const CHECKPOINT_RESTORE: &str = "container.checkpoint_restore";
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuestLimits {
    pub max_frame_size: u32,
    pub max_concurrent_exec: u32,
    pub max_port_forwards: u32,
    pub max_stream_chunk_size: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "parameters", rename_all = "snake_case")]
pub enum Operation {
    Exec(ExecRequest),
    Stream(StreamRequest),
    Cancel(CancelRequest),
    OciPrepare(OciPrepareRequest),
    OciRun(OciRunRequest),
    OciStop(OciStopRequest),
    OciDelete(OciDeleteRequest),
    PortForward(PortForwardRequest),
    Checkpoint(CheckpointRequest),
    Ping(PingRequest),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<UserSpec>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub attach_stdin: bool,
    #[serde(default = "default_true")]
    pub attach_stdout: bool,
    #[serde(default = "default_true")]
    pub attach_stderr: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supplementary_gids: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StreamRequest {
    pub execution_id: String,
    #[serde(flatten)]
    pub action: StreamAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stream_action", rename_all = "snake_case")]
pub enum StreamAction {
    WriteStdin { data_base64: String },
    CloseStdin,
    ResizeTty { rows: u16, columns: u16 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CancelRequest {
    pub target_request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OciPrepareRequest {
    pub container_id: String,
    pub image_reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<String>,
    pub bundle_path: String,
    pub oci_spec: Value,
    #[serde(default)]
    pub replace: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OciRunRequest {
    pub container_id: String,
    #[serde(default)]
    pub attach: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OciStopRequest {
    pub container_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OciDeleteRequest {
    pub container_id: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortForwardRequest {
    #[serde(flatten)]
    pub action: PortForwardAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "port_forward_action", rename_all = "snake_case")]
pub enum PortForwardAction {
    Add { rule: PortForwardRule },
    Remove { forwarding_id: String },
    List,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortForwardRule {
    pub forwarding_id: String,
    pub protocol: TransportProtocol,
    pub host_address: String,
    pub host_port: u16,
    pub guest_address: String,
    pub guest_port: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub action: CheckpointAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "checkpoint_action", rename_all = "snake_case")]
pub enum CheckpointAction {
    Create {
        container_id: String,
        checkpoint_id: String,
        path: String,
        leave_running: bool,
    },
    Restore {
        container_id: String,
        checkpoint_id: String,
        path: String,
    },
    Delete {
        checkpoint_id: String,
        path: String,
    },
    List,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PingRequest {
    pub nonce: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: RequestId,
    #[serde(flatten)]
    pub outcome: ResponseOutcome,
}

impl Response {
    #[must_use]
    pub fn success(id: RequestId, result: ResponsePayload) -> Self {
        Self {
            id,
            outcome: ResponseOutcome::Success { result },
        }
    }

    #[must_use]
    pub fn error(id: RequestId, error: RemoteError) -> Self {
        Self {
            id,
            outcome: ResponseOutcome::Error { error },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseOutcome {
    Success { result: ResponsePayload },
    Error { error: RemoteError },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result_type", content = "result", rename_all = "snake_case")]
pub enum ResponsePayload {
    Ack,
    Pong {
        nonce: String,
    },
    ExecStarted {
        execution_id: String,
        pid: u32,
    },
    StreamAccepted {
        execution_id: String,
    },
    Cancelled {
        target_request_id: RequestId,
    },
    OciPrepared {
        container_id: String,
        image_digest: String,
    },
    OciState {
        container_id: String,
        state: ContainerState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    PortForward {
        rule: PortForwardRule,
    },
    PortForwardList {
        rules: Vec<PortForwardRule>,
    },
    Checkpoint {
        checkpoint_id: String,
        state: CheckpointState,
    },
    CheckpointList {
        checkpoints: Vec<CheckpointInfo>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    Preparing,
    Created,
    Running,
    Stopped,
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointState {
    Creating,
    Ready,
    Restoring,
    Restored,
    Deleted,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub checkpoint_id: String,
    pub container_id: String,
    pub path: String,
    pub state: CheckpointState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Box<RemoteError>>,
}

impl RemoteError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
            cause: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionMismatch,
    InvalidRequest,
    UnsupportedOperation,
    CapabilityUnavailable,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    ResourceExhausted,
    DeadlineExceeded,
    Cancelled,
    Io,
    Oci,
    Runtime,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Monotonically increasing within a negotiated session.
    pub sequence: u64,
    pub timestamp_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(flatten)]
    pub event: EventKind,
}

impl Event {
    #[must_use]
    pub fn now(sequence: u64, request_id: Option<RequestId>, event: EventKind) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            });
        Self {
            sequence,
            timestamp_ms,
            request_id,
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "event", rename_all = "snake_case")]
pub enum EventKind {
    GuestReady {
        session_id: String,
    },
    CapabilityChanged {
        capability: Capability,
    },
    ExecutionStarted {
        execution_id: String,
        pid: u32,
    },
    Stream {
        execution_id: String,
        channel: StreamChannel,
        stream_sequence: u64,
        data_base64: String,
        eof: bool,
    },
    ProcessExited {
        execution_id: String,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    OciStateChanged {
        container_id: String,
        state: ContainerState,
        exit_code: Option<i32>,
    },
    PortForwardChanged {
        forwarding_id: String,
        active: bool,
        error: Option<RemoteError>,
    },
    CheckpointProgress {
        checkpoint_id: String,
        state: CheckpointState,
        completed_bytes: u64,
        total_bytes: Option<u64>,
    },
    Log {
        level: LogLevel,
        target: String,
        message: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        fields: BTreeMap<String, Value>,
    },
    Heartbeat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamChannel {
    Stdout,
    Stderr,
    Console,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_selects_newest_common_version() {
        let local = [ProtocolVersion::new(1, 0), ProtocolVersion::new(1, 1)];
        let remote = [ProtocolVersion::new(1, 0), ProtocolVersion::new(2, 0)];
        assert_eq!(
            negotiate_version(&local, &remote).unwrap(),
            ProtocolVersion::new(1, 0)
        );
    }

    #[test]
    fn negotiation_rejects_version_mismatch() {
        let error = negotiate_version(&[ProtocolVersion::new(1, 0)], &[ProtocolVersion::new(2, 0)])
            .unwrap_err();
        assert_eq!(error.local, vec![ProtocolVersion::new(1, 0)]);
        assert_eq!(error.remote, vec![ProtocolVersion::new(2, 0)]);
    }

    #[test]
    fn request_round_trips_with_a_stable_discriminator() {
        let request = Request {
            id: RequestId::new("host-1").unwrap(),
            operation: Operation::Ping(PingRequest {
                nonce: "abc".to_owned(),
            }),
        };
        let envelope = Envelope::new(Message::Request(request));
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"protocol\":\"dev.rish.guest\""));
        assert!(json.contains("\"operation\":\"ping\""));
        assert_eq!(serde_json::from_str::<Envelope>(&json).unwrap(), envelope);
    }

    #[test]
    fn handshake_round_trips_capabilities_and_limits() {
        let request_id = RequestId::new("hello-1").unwrap();
        let capabilities = GuestCapabilities {
            kernel_release: "6.12-rish".to_owned(),
            architecture: "aarch64".to_owned(),
            init_system: "systemd".to_owned(),
            cgroup_version: Some(2),
            container_runtimes: vec!["youki".to_owned()],
            features: vec![Capability {
                name: capability_name::NAMESPACES.to_owned(),
                version: 1,
                status: CapabilityStatus::Available,
                attributes: BTreeMap::new(),
                reason: None,
            }],
        };
        let envelope = Envelope::new(Message::HelloAck(HelloAck {
            request_id,
            outcome: HandshakeOutcome::Accepted {
                selected_version: CURRENT_PROTOCOL_VERSION,
                session_id: "guest-session-1".to_owned(),
                peer: PeerInfo {
                    name: "rish-guest-agent".to_owned(),
                    version: "0.1.0".to_owned(),
                    platform: "linux".to_owned(),
                    architecture: "aarch64".to_owned(),
                },
                capabilities: Box::new(capabilities),
                limits: GuestLimits {
                    max_frame_size: 8 * 1024 * 1024,
                    max_concurrent_exec: 32,
                    max_port_forwards: 128,
                    max_stream_chunk_size: 64 * 1024,
                },
            },
        }));

        let json = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(serde_json::from_slice::<Envelope>(&json).unwrap(), envelope);
    }

    #[test]
    fn complex_control_operations_round_trip() {
        let operations = [
            Operation::Stream(StreamRequest {
                execution_id: "exec-1".to_owned(),
                action: StreamAction::ResizeTty {
                    rows: 40,
                    columns: 120,
                },
            }),
            Operation::PortForward(PortForwardRequest {
                action: PortForwardAction::Add {
                    rule: PortForwardRule {
                        forwarding_id: "web".to_owned(),
                        protocol: TransportProtocol::Tcp,
                        host_address: "127.0.0.1".to_owned(),
                        host_port: 8080,
                        guest_address: "10.0.2.15".to_owned(),
                        guest_port: 80,
                    },
                },
            }),
            Operation::Checkpoint(CheckpointRequest {
                action: CheckpointAction::Create {
                    container_id: "demo".to_owned(),
                    checkpoint_id: "checkpoint-1".to_owned(),
                    path: "/var/lib/rish/checkpoints/checkpoint-1".to_owned(),
                    leave_running: true,
                },
            }),
        ];

        for (index, operation) in operations.into_iter().enumerate() {
            let envelope = Envelope::new(Message::Request(Request {
                id: RequestId::new(format!("request-{index}")).unwrap(),
                operation,
            }));
            let json = serde_json::to_vec(&envelope).unwrap();
            assert_eq!(serde_json::from_slice::<Envelope>(&json).unwrap(), envelope);
        }
    }

    #[test]
    fn checkpoint_action_has_one_explicit_container_key() {
        let envelope = Envelope::new(Message::Request(Request {
            id: RequestId::new("checkpoint-request").unwrap(),
            operation: Operation::Checkpoint(CheckpointRequest {
                action: CheckpointAction::List,
            }),
        }));
        let json = serde_json::to_value(&envelope).unwrap();
        let parameters = &json["payload"]["parameters"];

        assert_eq!(
            parameters["action"]["checkpoint_action"],
            Value::String("list".to_owned())
        );
        assert!(parameters.get("checkpoint_action").is_none());
        assert_eq!(serde_json::from_value::<Envelope>(json).unwrap(), envelope);
    }
}
