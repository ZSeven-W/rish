use thiserror::Error;

use crate::MissingCapability;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("missing capability: {0}")]
    MissingCapability(#[from] MissingCapability),

    #[error("unknown guest executable: {0}")]
    UnknownExecutable(String),

    #[error("host bridge failed: {0}")]
    HostBridge(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("operation is not implemented: {0}")]
    NotImplemented(String),

    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}
