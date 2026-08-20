use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("the full-fidelity guest importer only runs inside Linux")]
    UnsupportedPlatform,
    #[error("the guest importer must run with effective uid 0")]
    GuestRootRequired,
    #[error("verified image schema version {0} is unsupported")]
    UnsupportedRecordSchema(u32),
    #[error("verified image targets {0}, not Linux")]
    NonLinuxImage(String),
    #[error("image has {actual} layers, exceeding the {limit}-layer limit")]
    LayerLimitExceeded { actual: u64, limit: u64 },
    #[error("unsupported layer media type: {0}")]
    UnsupportedLayerMediaType(String),
    #[error("foreign/non-distributable layers cannot be imported")]
    ForeignLayerRejected,
    #[error("descriptor digest algorithm must be sha256, got {0}")]
    UnsupportedDigestAlgorithm(String),
    #[error("descriptor size {actual} exceeds the {limit}-byte layer limit")]
    CompressedLayerLimitExceeded { actual: u64, limit: u64 },
    #[error("compressed image data exceeds the {limit}-byte total limit")]
    CompressedTotalLimitExceeded { limit: u64 },
    #[error("descriptor body size mismatch: expected {expected}, got {actual}")]
    DescriptorSizeMismatch { expected: u64, actual: u64 },
    #[error("descriptor digest mismatch: expected {expected}, got {actual}")]
    DescriptorDigestMismatch { expected: String, actual: String },
    #[error("layer diff-id algorithm must be sha256, got {0}")]
    UnsupportedDiffIdAlgorithm(String),
    #[error("layer diff-id mismatch: expected {expected}, got {actual}")]
    DiffIdMismatch { expected: String, actual: String },
    #[error("layer expands beyond the {limit}-byte archive limit")]
    UncompressedLayerLimitExceeded { limit: u64 },
    #[error("uncompressed image data exceeds the {limit}-byte total limit")]
    UncompressedTotalLimitExceeded { limit: u64 },
    #[error("layer has more than {limit} entries")]
    LayerEntryLimitExceeded { limit: u64 },
    #[error("image has more than {limit} entries")]
    TotalEntryLimitExceeded { limit: u64 },
    #[error("regular file {path:?} has {actual} bytes, exceeding the {limit}-byte limit")]
    FileLimitExceeded {
        path: PathBuf,
        actual: u64,
        limit: u64,
    },
    #[error("regular files expand beyond the {limit}-byte image limit")]
    RegularFileTotalLimitExceeded { limit: u64 },
    #[error("regular file {path:?} was truncated: expected {expected}, copied {actual}")]
    TruncatedFile {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error(
        "layer regular-file byte count changed after preflight: expected {expected}, got {actual}"
    )]
    LayerByteCountMismatch { expected: u64, actual: u64 },
    #[error("invalid archive path {path:?}: {reason}")]
    InvalidPath { path: PathBuf, reason: &'static str },
    #[error("archive path exceeds the {limit}-byte limit")]
    PathLimitExceeded { limit: usize },
    #[error("archive path has more than {limit} components")]
    PathDepthLimitExceeded { limit: usize },
    #[error("link target for {path:?} exceeds the {limit}-byte limit")]
    LinkTargetLimitExceeded { path: PathBuf, limit: usize },
    #[error("link entry {0:?} has no target")]
    MissingLinkTarget(PathBuf),
    #[error("path {path:?} traverses symbolic-link component {component:?}")]
    SymlinkPathComponent { path: PathBuf, component: PathBuf },
    #[error("parent component is not a directory while resolving {0:?}")]
    ParentNotDirectory(PathBuf),
    #[error("hard-link target for {path:?} is missing or not a regular file: {target:?}")]
    InvalidHardlinkTarget { path: PathBuf, target: PathBuf },
    #[error(
        "hard-link entry {0:?} carries inode metadata that cannot be represented independently"
    )]
    UnsupportedHardlinkMetadata(PathBuf),
    #[error("malformed OCI whiteout at {0:?}")]
    MalformedWhiteout(PathBuf),
    #[error("OCI whiteout at {0:?} contains data or has an invalid type")]
    InvalidWhiteout(PathBuf),
    #[error("unsupported tar entry type {entry_type:#04x} at {path:?}")]
    UnsupportedEntryType { path: PathBuf, entry_type: u8 },
    #[error("GNU sparse entries are unsupported and cannot be preserved faithfully: {0:?}")]
    SparseEntryRejected(PathBuf),
    #[error("uid or gid in {0:?} exceeds Linux's 32-bit id range")]
    IdOutOfRange(PathBuf),
    #[error("timestamp in {0:?} is outside Linux's signed 64-bit range")]
    TimestampOutOfRange(PathBuf),
    #[error("symbolic-link mode at {path:?} is {mode:#o}; Linux cannot preserve it")]
    UnsupportedSymlinkMode { path: PathBuf, mode: u32 },
    #[error("extended attribute name is invalid at {0:?}")]
    InvalidXattrName(PathBuf),
    #[error("entry {path:?} has more than {limit} extended attributes")]
    XattrCountLimitExceeded { path: PathBuf, limit: u64 },
    #[error("extended attribute name at {path:?} exceeds {limit} bytes")]
    XattrNameLimitExceeded { path: PathBuf, limit: usize },
    #[error("extended attribute value at {path:?} exceeds {limit} bytes")]
    XattrValueLimitExceeded { path: PathBuf, limit: usize },
    #[error("layer extended attributes exceed the {limit}-byte limit")]
    XattrLayerLimitExceeded { limit: u64 },
    #[error("layer path/link/xattr metadata exceeds the {limit}-byte limit")]
    MetadataLayerLimitExceeded { limit: u64 },
    #[error("PAX extension header has {actual} bytes, exceeding the {limit}-byte limit")]
    PaxHeaderLimitExceeded { actual: u64, limit: u64 },
    #[error("layer has more than {limit} tar extension headers")]
    ExtensionHeaderLimitExceeded { limit: u64 },
    #[error("global PAX headers are unsupported because their scope cannot be preserved safely")]
    GlobalPaxRejected,
    #[error("duplicate extended attribute {name:?} at {path:?}")]
    DuplicateXattr { path: PathBuf, name: Vec<u8> },
    #[error("duplicate PAX metadata key {key:?} at {path:?}")]
    DuplicatePaxKey { path: PathBuf, key: Vec<u8> },
    #[error("invalid PAX timestamp {key:?} at {path:?}")]
    InvalidTimestamp { path: PathBuf, key: Vec<u8> },
    #[error("invalid numeric PAX metadata {key:?} at {path:?}")]
    InvalidPaxNumeric { path: PathBuf, key: Vec<u8> },
    #[error("invalid security.capability value at {path:?}: {reason}")]
    InvalidFileCapability { path: PathBuf, reason: &'static str },
    #[error("security.capability is only supported on a regular entry, not {path:?}")]
    UnsupportedCapabilityTarget { path: PathBuf },
    #[error("unsupported metadata encoding {key:?} at {path:?}; refusing to drop metadata")]
    UnsupportedMetadataEncoding { path: PathBuf, key: Vec<u8> },
    #[error("device node at {0:?} requires explicit privileged guest authorization")]
    DevicePolicyRequired(PathBuf),
    #[error("FIFO at {0:?} requires explicit privileged guest authorization")]
    FifoPolicyRequired(PathBuf),
    #[error("device node at {0:?} is missing major/minor numbers")]
    MissingDeviceNumber(PathBuf),
    #[error("rootfs parent is not a private root-owned real directory: {0:?}")]
    UnsafeRootfsParent(PathBuf),
    #[error("rootfs destination has no valid parent or basename: {0:?}")]
    InvalidDestination(PathBuf),
    #[error("rootfs destination already exists: {0:?}")]
    DestinationExists(PathBuf),
    #[error("atomic no-replace publication is unavailable in this Linux guest")]
    AtomicPublishUnsupported,
    #[error("filesystem operation failed at {path:?}: {source}")]
    Filesystem {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("blob source rejected descriptor: {0}")]
    BlobSource(String),
    #[error("archive I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(any(target_os = "linux", test))]
impl ImportError {
    pub(crate) fn filesystem(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Filesystem {
            path: path.into(),
            source,
        }
    }
}
