//! Security-focused application of OCI filesystem layers.
//!
//! The extractor validates and bounds the complete archive before changing the
//! destination. It then applies OCI whiteouts before materializing ordinary
//! entries, so whiteouts cannot accidentally remove entries from their own
//! layer merely because of tar ordering.
//!
//! Application is not transactional by itself: I/O failures after preflight
//! may leave the supplied destination partially changed. Use `rish-snapshot`
//! to apply all layers in private staging and publish only the completed tree.
//!
//! Portable snapshots strip privileged Linux metadata and force owner `rwx`
//! on directories so later layers and failed-staging cleanup remain possible.
//! Bootable/systemd root filesystems must be unpacked inside the Linux guest.
//!
//! The destination must not be modified concurrently. Portable Rust does not
//! expose the directory-descriptor APIs needed to close every filesystem
//! time-of-check/time-of-use race on all supported mobile platforms.

#![forbid(unsafe_code)]

mod apply;
mod error;
mod path;

pub use apply::{
    ApplyOptions, ApplyReport, LayerFormat, LayerLimits, SpecialFilePolicy, apply_layer,
    apply_layer_with_format,
};
pub use error::LayerError;

#[cfg(test)]
mod tests;
