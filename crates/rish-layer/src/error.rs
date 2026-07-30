use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Failure while validating or applying an OCI layer.
#[derive(Debug, Error)]
pub enum LayerError {
    #[error("I/O error while processing layer: {0}")]
    Io(#[from] io::Error),

    #[error("destination is not a real directory: {0:?}")]
    InvalidDestination(PathBuf),

    #[error("layer spool path is not a real directory: {0:?}")]
    InvalidSpoolDirectory(PathBuf),

    #[error("invalid archive path {path:?}: {reason}")]
    InvalidPath { path: PathBuf, reason: &'static str },

    #[error("archive path exceeds the {limit}-byte limit")]
    PathTooLong { limit: usize },

    #[error("link target for {path:?} exceeds the {limit}-byte limit")]
    LinkTargetTooLong { path: PathBuf, limit: usize },

    #[error("link entry {0:?} has no target")]
    MissingLinkTarget(PathBuf),

    #[error("symbolic link {path:?} escapes the layer root via target {target:?}")]
    SymlinkEscape { path: PathBuf, target: PathBuf },

    #[error("path {path:?} traverses symbolic-link component {component:?}")]
    SymlinkPathComponent { path: PathBuf, component: PathBuf },

    #[error("path component is not a directory while resolving {0:?}")]
    ParentNotDirectory(PathBuf),

    #[error("hard-link target for {path:?} is missing or is not a regular file: {target:?}")]
    InvalidHardlinkTarget { path: PathBuf, target: PathBuf },

    #[error("malformed OCI whiteout at {0:?}")]
    MalformedWhiteout(PathBuf),

    #[error("whiteout entry must not contain data: {0:?}")]
    WhiteoutHasData(PathBuf),

    #[error("layer has more than {limit} entries")]
    EntryLimitExceeded { limit: u64 },

    #[error("file {path:?} expands to {size} bytes, exceeding the {limit}-byte limit")]
    FileSizeLimitExceeded {
        path: PathBuf,
        size: u64,
        limit: u64,
    },

    #[error("layer expands to more than {limit} bytes")]
    TotalSizeLimitExceeded { limit: u64 },

    #[error("decompressed tar exceeds the {limit}-byte limit")]
    ArchiveSizeLimitExceeded { limit: u64 },

    #[error("invalid OCI diff-id: {0}")]
    InvalidDiffId(String),

    #[error("OCI diff-id mismatch: expected {expected}, computed {actual}")]
    DiffIdMismatch { expected: String, actual: String },

    #[error("special file type {entry_type:#04x} is rejected at {path:?}")]
    SpecialFileRejected { path: PathBuf, entry_type: u8 },

    #[error("unsupported tar entry type {entry_type:#04x} at {path:?}")]
    UnsupportedEntryType { path: PathBuf, entry_type: u8 },

    #[error("file data for {path:?} was truncated: expected {expected}, read {actual}")]
    TruncatedEntry {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },

    #[error("symbolic links are unsupported on this host")]
    SymlinksUnsupported,
}
