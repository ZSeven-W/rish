use std::{io, path::PathBuf};

use thiserror::Error;

/// Fail-closed errors from the AMD64 TCTI adapter.
#[derive(Debug, Error)]
pub enum SoftVmError {
    #[error("invalid x86_64 software VM configuration: {0}")]
    InvalidConfig(String),

    #[error("TCTI provider is unavailable: {0}")]
    ProviderUnavailable(String),

    #[error("TCTI provider contract rejected: {0}")]
    ProviderContract(String),

    #[error("TCTI provider call {operation} failed with status {status}")]
    ProviderCall {
        operation: &'static str,
        status: i32,
    },

    #[error("failed to read {kind} artifact {path}: {source}")]
    ArtifactRead {
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{kind} artifact is {actual} bytes, limit is {limit} bytes")]
    ArtifactTooLarge {
        kind: &'static str,
        actual: u64,
        limit: u64,
    },

    #[error("invalid x86_64 kernel image: {0}")]
    InvalidKernel(String),

    #[error("software VM worker failed: {0}")]
    Worker(String),

    #[error("software VM worker is busy")]
    WorkerBusy,

    #[error("software VM worker stopped")]
    WorkerStopped,

    #[error("requested {requested} units, per-run limit is {limit}")]
    UnitLimit { requested: u64, limit: u64 },

    #[error(
        "TCTI UART is available, but the versioned rish guest control transport is not connected"
    )]
    ControlTransportUnavailable,
}
