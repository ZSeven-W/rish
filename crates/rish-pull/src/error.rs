use rish_content::StoreError;
use rish_oci::OciError;
use rish_registry::{
    DigestParseError, HeaderError, ImageReferenceError, MediaType, MediaTypeParseError,
    ModelValidationError, PlatformSelectionError, ResponseValidationError, TransportError,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobKind {
    Index,
    Manifest,
    Config,
    Layer,
}

impl std::fmt::Display for BlobKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Index => "image index",
            Self::Manifest => "image manifest",
            Self::Config => "image config",
            Self::Layer => "image layer",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    ManifestBytes,
    ConfigBytes,
    LayerCount,
    LayerBytes,
    TotalBytes,
}

impl std::fmt::Display for LimitKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ManifestBytes => "manifest bytes",
            Self::ConfigBytes => "config bytes",
            Self::LayerCount => "layer count",
            Self::LayerBytes => "layer bytes",
            Self::TotalBytes => "total pull bytes",
        })
    }
}

#[derive(Debug, Error)]
pub enum PullError {
    #[error("registry response body read failed: {0}")]
    BodyRead(#[from] std::io::Error),

    #[error(transparent)]
    Reference(#[from] ImageReferenceError),

    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error(transparent)]
    Response(#[from] ResponseValidationError),

    #[error(transparent)]
    Header(#[from] HeaderError),

    #[error(transparent)]
    Digest(#[from] DigestParseError),

    #[error(transparent)]
    MediaType(#[from] MediaTypeParseError),

    #[error(transparent)]
    Model(#[from] ModelValidationError),

    #[error(transparent)]
    PlatformSelection(#[from] PlatformSelectionError),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Oci(#[from] OciError),

    #[error("invalid JSON in {kind}: {source}")]
    Json {
        kind: BlobKind,
        #[source]
        source: serde_json::Error,
    },

    #[error("{kind} JSON failed bounded structural preflight: {reason}")]
    JsonPreflight { kind: BlobKind, reason: String },

    #[error("{kind} uses unsupported CAS digest algorithm {algorithm}")]
    UnsupportedDigest { kind: BlobKind, algorithm: String },

    #[error("{kind} has unsupported media type {media_type}")]
    UnsupportedMediaType {
        kind: BlobKind,
        media_type: MediaType,
    },

    #[error("{kind} media type mismatch: descriptor says {expected}, document says {actual}")]
    DocumentMediaTypeMismatch {
        kind: BlobKind,
        expected: MediaType,
        actual: MediaType,
    },

    #[error("expected {expected} document, registry returned {actual}")]
    UnexpectedDocument {
        expected: BlobKind,
        actual: BlobKind,
    },

    #[error("{kind} limit exceeded: maximum {limit}, observed {actual}")]
    LimitExceeded {
        kind: LimitKind,
        limit: u64,
        actual: u64,
    },

    #[error(
        "image config platform mismatch: requested {expected_os}/{expected_architecture}, \
         got {actual_os}/{actual_architecture}"
    )]
    ImagePlatformMismatch {
        expected_os: String,
        expected_architecture: String,
        actual_os: String,
        actual_architecture: String,
    },

    #[error("image config has {diff_ids} rootfs diff IDs, but manifest declares {layers} layers")]
    LayerDiffIdCountMismatch { diff_ids: usize, layers: usize },
}
