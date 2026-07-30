use std::{error::Error, fmt};

use crate::ProtocolVersion;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentifierError {
    Empty,
    TooLong { length: usize, max: usize },
    InvalidCharacter,
}

impl fmt::Display for IdentifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("request ID cannot be empty"),
            Self::TooLong { length, max } => {
                write!(formatter, "request ID length {length} exceeds limit {max}")
            }
            Self::InvalidCharacter => {
                formatter.write_str("request ID contains an invalid character")
            }
        }
    }
}

impl Error for IdentifierError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiationError {
    pub local: Vec<ProtocolVersion>,
    pub remote: Vec<ProtocolVersion>,
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "no common protocol version (local: {:?}, remote: {:?})",
            self.local, self.remote
        )
    }
}

impl Error for NegotiationError {}

#[derive(Debug)]
pub enum FrameError {
    InvalidMaximum(usize),
    FrameTooLarge {
        size: usize,
        max: usize,
    },
    BufferTooLarge {
        size: usize,
        max: usize,
    },
    Serialization(serde_json::Error),
    Deserialization(serde_json::Error),
    ProtocolMismatch {
        expected: &'static str,
        received: String,
    },
    VersionMismatch {
        expected: ProtocolVersion,
        received: ProtocolVersion,
    },
    DecoderPoisoned,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaximum(max) => {
                write!(formatter, "invalid maximum frame size {max}")
            }
            Self::FrameTooLarge { size, max } => {
                write!(formatter, "frame payload size {size} exceeds limit {max}")
            }
            Self::BufferTooLarge { size, max } => {
                write!(formatter, "decoder buffer size {size} exceeds limit {max}")
            }
            Self::Serialization(error) => write!(formatter, "cannot serialize frame: {error}"),
            Self::Deserialization(error) => {
                write!(formatter, "cannot deserialize frame: {error}")
            }
            Self::ProtocolMismatch { expected, received } => write!(
                formatter,
                "protocol discriminator mismatch: expected {expected}, received {received}"
            ),
            Self::VersionMismatch { expected, received } => write!(
                formatter,
                "protocol version mismatch: expected {expected}, received {received}"
            ),
            Self::DecoderPoisoned => {
                formatter.write_str("frame decoder is poisoned; reset it before reuse")
            }
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialization(error) | Self::Deserialization(error) => Some(error),
            _ => None,
        }
    }
}
