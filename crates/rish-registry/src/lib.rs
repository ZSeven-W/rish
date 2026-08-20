//! Portable, policy-free building blocks for talking to OCI Distribution
//! registries.
//!
//! The crate intentionally does not choose an HTTP stack or async runtime.
//! Mobile hosts can implement [`RegistryTransport`] with URLSession, OkHttp,
//! ArkTS networking, or a Rust HTTP client without changing registry semantics.

mod auth;
mod digest;
mod media_type;
mod model;
mod reference;
mod selection;
mod transport;
mod validation;

pub use auth::{AuthChallengeError, BearerChallenge};
pub use digest::{Digest, DigestParseError, DigestValidationError};
pub use media_type::{MediaType, MediaTypeParseError};
pub use model::{
    Descriptor, ImageIndex, ImageManifest, ManifestDocument, ModelValidationError, Platform,
};
pub use reference::{ImageReference, ImageReferenceError};
pub use selection::{
    GuestPlatform, MAX_GUEST_PLATFORM_TOKEN_BYTES, PlatformRequest, PlatformSelectionError,
    select_platform,
};
pub use transport::{
    DEFAULT_MANIFEST_RESPONSE_LIMIT, HeaderError, HeaderMap, HttpMethod, MAX_HEADER_BYTES,
    MAX_HEADER_FIELDS, MAX_HEADER_NAME_BYTES, MAX_HEADER_VALUE_BYTES, RegistryRequest,
    RegistryResponse, RegistryScheme, RegistryStreamResponse, RegistryTransport, TransportError,
};
pub use validation::{ResponseValidationError, ValidatedResponse, ValidationPolicy};
