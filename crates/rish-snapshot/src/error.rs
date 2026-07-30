use std::io;
use std::path::PathBuf;

use rish_content::StoreError;
use rish_layer::LayerError;
use rish_registry::{MediaType, ModelValidationError};
use thiserror::Error;

/// Failure while validating, assembling, or publishing a rootfs snapshot.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("invalid snapshot ID: {0:?}")]
    InvalidSnapshotId(String),

    #[error("layer descriptor count ({descriptors}) does not match diff-id count ({diff_ids})")]
    LayerCountMismatch { descriptors: usize, diff_ids: usize },

    #[error("layer {index} descriptor is invalid: {source}")]
    InvalidDescriptor {
        index: usize,
        #[source]
        source: ModelValidationError,
    },

    #[error("layer {index} uses unsupported media type {media_type}")]
    UnsupportedLayerMediaType { index: usize, media_type: MediaType },

    #[error("layer {index} content digest uses unsupported algorithm {algorithm:?}")]
    UnsupportedContentDigest { index: usize, algorithm: String },

    #[error("layer {index} diff-id uses unsupported algorithm {algorithm:?}")]
    UnsupportedDiffId { index: usize, algorithm: String },

    #[error("content-store root is not a real directory: {0}")]
    UnsafeStoreRoot(PathBuf),

    #[error("snapshot directory is not a real directory: {0}")]
    UnsafeSnapshotsDirectory(PathBuf),

    #[error("snapshot target is a symbolic link: {0}")]
    UnsafeSnapshotTarget(PathBuf),

    #[error("snapshot already exists: {0}")]
    SnapshotAlreadyExists(PathBuf),

    #[error("content-store operation failed for layer {index}: {source}")]
    Content {
        index: usize,
        #[source]
        source: StoreError,
    },

    #[error("failed to apply layer {index}: {source}")]
    Apply {
        index: usize,
        #[source]
        source: LayerError,
    },

    #[error("snapshot filesystem operation failed: {0}")]
    Io(#[from] io::Error),

    #[error("atomic no-replace publication is unavailable: {0}")]
    AtomicPublishUnavailable(io::Error),

    #[error("failed to atomically publish snapshot: {0}")]
    Publish(io::Error),
}
