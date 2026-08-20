//! A synchronous, transport-agnostic OCI image pull pipeline.
//!
//! Registry responses are fully validated before they are admitted to the
//! content-addressed store. Manifest and config JSON use bounded buffers;
//! layers stream directly from the transport reader into CAS. A successful
//! [`PulledImage`] owns a process-local [`rish_content::Lease`] covering every
//! reachable blob pulled for the image.

mod error;
mod json_limits;
mod policy;
mod puller;
mod verified_image;

pub use error::{BlobKind, LimitKind, PullError};
pub use json_limits::JsonLimits;
pub use policy::{
    DEFAULT_MAX_CONFIG_BYTES, DEFAULT_MAX_LAYER_BYTES, DEFAULT_MAX_LAYERS,
    DEFAULT_MAX_MANIFEST_BYTES, DEFAULT_MAX_TOTAL_BYTES, PullPolicy,
};
pub use puller::{PulledBlob, PulledImage, Puller};
pub use rish_registry::GuestPlatform;
pub use verified_image::{
    VERIFIED_IMAGE_RECORD_SCHEMA_VERSION, VerifiedDescriptor, VerifiedImageHandle,
    VerifiedImageLayer, VerifiedImageRecord, VerifiedImageRecordError, VerifiedImageRecordStore,
    VerifiedPlatform, VerifiedProcessConfig,
};

#[cfg(test)]
mod tests;
