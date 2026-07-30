use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppletError {
    #[error("{command}: invalid arguments: {message}")]
    InvalidArguments { command: String, message: String },

    #[error("portable applet is not implemented: {0}")]
    UnknownApplet(String),

    #[error("sandbox path is unsafe: {0}")]
    UnsafePath(String),

    #[error("portable applet output exceeds {limit} bytes")]
    OutputLimit { limit: usize },

    #[error("portable applet input exceeds {limit} bytes")]
    InputLimit { limit: usize },

    #[error("portable applet visited more than {limit} filesystem entries")]
    EntryLimit { limit: usize },

    #[error("portable applet exceeded {limit} bounded work units")]
    WorkLimit { limit: usize },

    #[error("portable applet filesystem is read-only")]
    ReadOnlyFilesystem,

    #[error("portable applet execution lock is unavailable")]
    ExecutionLock,

    #[error("I/O failed for {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
}

impl AppletError {
    #[must_use]
    pub fn usage(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidArguments {
            command: command.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn io(path: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, AppletError>;
