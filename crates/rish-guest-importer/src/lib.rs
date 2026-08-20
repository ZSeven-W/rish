//! Full-fidelity OCI rootfs materialization inside a Linux guest.
//!
//! This crate is deliberately separate from the portable host snapshotter.
//! It requires a Linux guest running as root so ownership, set-id modes,
//! extended attributes, file capabilities, and (when explicitly negotiated)
//! device nodes can be preserved without pretending a mobile host provides
//! those semantics.
//!
//! Every compressed descriptor is size- and SHA-256-verified before it is
//! decompressed. Every uncompressed tar is SHA-256-verified against the
//! corresponding OCI diff-id before the layer mutates an isolated staging
//! rootfs. A completed rootfs is published with Linux `renameat2(2)` and
//! `RENAME_NOREPLACE`; platforms without that primitive fail closed.

mod error;
mod import;

#[cfg(any(target_os = "linux", test))]
mod archive;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", test))]
mod path;
#[cfg(any(target_os = "linux", test))]
mod verify;

use std::io::Read;
use std::path::PathBuf;

pub use error::ImportError;
pub use import::import_verified_image;
use rish_pull::VerifiedDescriptor;

/// Opens immutable blob streams for descriptors selected from a verified image
/// record.
///
/// The importer still verifies the stream's exact size and SHA-256 digest. A
/// source must never substitute a stream based only on a tag or mutable name.
pub trait DescriptorSource {
    type Reader: Read;

    fn open(&mut self, descriptor: &VerifiedDescriptor) -> Result<Self::Reader, ImportError>;
}

impl<F, R> DescriptorSource for F
where
    F: FnMut(&VerifiedDescriptor) -> Result<R, ImportError>,
    R: Read,
{
    type Reader = R;

    fn open(&mut self, descriptor: &VerifiedDescriptor) -> Result<Self::Reader, ImportError> {
        self(descriptor)
    }
}

/// Explicit guest authorization for special filesystem objects.
///
/// Leaving both fields false is the safe default. A guest supervisor must only
/// enable them after its privileged-container capability has been negotiated;
/// this type does not grant host privileges.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrivilegedGuestPolicy {
    pub(crate) allow_device_nodes: bool,
    pub(crate) allow_fifos: bool,
}

impl PrivilegedGuestPolicy {
    /// Constructs a special-file grant after the guest supervisor has
    /// negotiated the corresponding privileged-container capabilities.
    ///
    /// This constructor is intentionally named for its required call-site
    /// precondition. The importer still relies on Linux to enforce CAP_MKNOD
    /// and filesystem policy.
    #[must_use]
    pub const fn after_capability_negotiation(allow_device_nodes: bool, allow_fifos: bool) -> Self {
        Self {
            allow_device_nodes,
            allow_fifos,
        }
    }
}

/// Bounded resource policy for one complete image import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportLimits {
    pub max_layers: u64,
    pub max_entries_per_layer: u64,
    pub max_entries_total: u64,
    pub max_compressed_layer_bytes: u64,
    pub max_compressed_total_bytes: u64,
    pub max_uncompressed_layer_bytes: u64,
    pub max_uncompressed_total_bytes: u64,
    pub max_file_bytes: u64,
    pub max_regular_file_bytes_total: u64,
    pub max_path_bytes: usize,
    pub max_path_components: usize,
    pub max_link_target_bytes: usize,
    pub max_xattrs_per_entry: u64,
    pub max_xattr_name_bytes: usize,
    pub max_xattr_value_bytes: usize,
    pub max_xattr_bytes_per_layer: u64,
    pub max_metadata_bytes_per_layer: u64,
    pub max_pax_header_bytes: u64,
    pub max_extension_headers_per_layer: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_layers: 256,
            max_entries_per_layer: 100_000,
            max_entries_total: 500_000,
            max_compressed_layer_bytes: 2 * 1024 * 1024 * 1024,
            max_compressed_total_bytes: 4 * 1024 * 1024 * 1024,
            max_uncompressed_layer_bytes: 4 * 1024 * 1024 * 1024,
            max_uncompressed_total_bytes: 8 * 1024 * 1024 * 1024,
            max_file_bytes: 2 * 1024 * 1024 * 1024,
            max_regular_file_bytes_total: 8 * 1024 * 1024 * 1024,
            max_path_bytes: 4_096,
            max_path_components: 256,
            max_link_target_bytes: 4_096,
            max_xattrs_per_entry: 128,
            max_xattr_name_bytes: 255,
            max_xattr_value_bytes: 64 * 1024,
            max_xattr_bytes_per_layer: 64 * 1024 * 1024,
            max_metadata_bytes_per_layer: 64 * 1024 * 1024,
            max_pax_header_bytes: 1024 * 1024,
            max_extension_headers_per_layer: 4_096,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportOptions {
    pub limits: ImportLimits,
    pub privileged_guest: PrivilegedGuestPolicy,
}

/// Auditable totals for an atomically published rootfs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub rootfs: PathBuf,
    pub layers: u64,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub entries: u64,
    pub regular_file_bytes: u64,
    pub whiteouts: u64,
    pub device_nodes: u64,
    pub fifos: u64,
}

#[cfg(test)]
mod tests;
