//! A small, cross-platform, content-addressed blob store for OCI content.
//!
//! Blob names are parsed SHA-256 digests rather than caller-provided paths.
//! Ingests are streamed into a private temporary directory, verified, synced,
//! and atomically promoted into `blobs/sha256/<hex>`.
//!
//! Persistent pins and process-local leases are roots for garbage collection.
//! Callers that understand an OCI object graph must pin or lease every reachable
//! descriptor (or pass the graph closure as additional GC roots).
//!
//! Unique committed blobs share a configurable process-local byte quota. The
//! default is 4 GiB for mobile app storage. Existing blobs are counted when the
//! store opens, duplicate digests are not charged twice, and successful GC
//! deletion releases capacity. A store already above a newly lowered quota can
//! still open for reads and GC, but unique commits fail with
//! [`StoreError::CommittedBytesQuotaExceeded`].

mod digest;
mod error;
mod filesystem;
mod quota;
mod store;

pub use digest::{DigestParseError, Sha256Digest};
pub use error::{Result, StoreError};
pub use store::{
    BlobDescriptor, ContentStore, DEFAULT_MAX_BLOB_SIZE, DEFAULT_MAX_COMMITTED_BYTES, GcOptions,
    GcReport, Lease, Pin, StoreConfig,
};
