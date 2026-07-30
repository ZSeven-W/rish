use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::digest::{DigestParseError, Sha256Digest};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    InvalidDigest(DigestParseError),
    BlobTooLarge {
        maximum: u64,
        observed_at_least: u64,
    },
    CommittedBytesQuotaExceeded {
        maximum: u64,
        committed: u64,
        attempted: u64,
    },
    DigestMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    SizeMismatch {
        expected: u64,
        actual: u64,
    },
    BlobNotFound(Sha256Digest),
    CorruptBlob {
        digest: Sha256Digest,
        reason: String,
    },
    InvalidPinName(String),
    CorruptPin {
        path: PathBuf,
        reason: String,
    },
    UnsafeFilesystemEntry {
        path: PathBuf,
        reason: String,
    },
    LeaseReleased,
    LockPoisoned,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "content store I/O error: {error}"),
            Self::InvalidDigest(error) => write!(formatter, "invalid digest: {error}"),
            Self::BlobTooLarge {
                maximum,
                observed_at_least,
            } => write!(
                formatter,
                "blob exceeds the {maximum}-byte limit (read at least {observed_at_least} bytes)"
            ),
            Self::CommittedBytesQuotaExceeded {
                maximum,
                committed,
                attempted,
            } => write!(
                formatter,
                "CAS committed-byte quota exceeded: maximum {maximum} bytes, \
                 {committed} already committed, attempted to add {attempted}"
            ),
            Self::DigestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "digest mismatch: expected {expected}, got {actual}"
                )
            }
            Self::SizeMismatch { expected, actual } => {
                write!(
                    formatter,
                    "size mismatch: expected {expected}, got {actual}"
                )
            }
            Self::BlobNotFound(digest) => write!(formatter, "blob not found: {digest}"),
            Self::CorruptBlob { digest, reason } => {
                write!(formatter, "stored blob {digest} is corrupt: {reason}")
            }
            Self::InvalidPinName(name) => write!(formatter, "invalid pin name: {name:?}"),
            Self::CorruptPin { path, reason } => {
                write!(formatter, "corrupt pin at {}: {reason}", path.display())
            }
            Self::UnsafeFilesystemEntry { path, reason } => write!(
                formatter,
                "unsafe content-store entry at {}: {reason}",
                path.display()
            ),
            Self::LeaseReleased => write!(formatter, "lease has already been released"),
            Self::LockPoisoned => write!(formatter, "content-store lock is poisoned"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidDigest(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DigestParseError> for StoreError {
    fn from(error: DigestParseError) -> Self {
        Self::InvalidDigest(error)
    }
}
