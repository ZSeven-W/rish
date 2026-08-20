//! Transport-neutral protocol shared by a rish mobile host and Linux guest.
//!
//! The wire format is JSON prefixed by a four-byte, big-endian payload length.
//! This crate deliberately contains no socket or async-runtime dependency: the
//! same codec can be driven by vsock, virtio-serial, or an in-memory test link.

mod error;
mod framing;
mod identifier;
mod model;
mod session;

pub use error::{FrameError, IdentifierError, NegotiationError};
pub use framing::{FrameDecoder, FrameEncoder};
pub use identifier::RequestId;
pub use model::*;
pub use session::{
    Exchange, ExecOutcome, MAX_EXEC_STREAM_BYTES, MAX_FRAMES_PER_EXCHANGE, NegotiatedSession,
    SessionClient, SessionError, SessionIo, decode_base64, encode_base64,
};

/// Stable protocol discriminator carried in every envelope.
pub const PROTOCOL_ID: &str = "dev.rish.guest";

/// Current JSON schema version.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0);

/// Versions implemented by this crate, in preference order.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CURRENT_PROTOCOL_VERSION];

/// Default upper bound for one JSON payload, excluding its four-byte prefix.
pub const DEFAULT_MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;

/// Smallest payload limit accepted for an established session.
///
/// Bootstrap `Hello` and `HelloAck` frames use the default limit. This floor
/// leaves room for a 32 KiB stream chunk after base64 and JSON overhead.
pub const MIN_NEGOTIATED_FRAME_SIZE: usize = 64 * 1024;
