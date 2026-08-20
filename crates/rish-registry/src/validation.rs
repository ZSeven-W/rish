use std::str::FromStr;

use thiserror::Error;

use crate::{
    Descriptor, Digest, DigestParseError, DigestValidationError, HeaderError, HeaderMap, MediaType,
    MediaTypeParseError, ModelValidationError, RegistryResponse, RegistryStreamResponse,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ValidationPolicy {
    /// Require `Content-Length`; when present, it is always checked.
    pub require_content_length: bool,
    /// Require `Content-Type`; when present, it is always checked.
    pub require_content_type: bool,
    /// Require `Docker-Content-Digest`; when present, it is always checked.
    pub require_registry_digest: bool,
}

impl ValidationPolicy {
    #[must_use]
    pub const fn strict_headers() -> Self {
        Self {
            require_content_length: true,
            require_content_type: true,
            require_registry_digest: true,
        }
    }

    /// Requires representation metadata while treating the registry digest as
    /// optional for a response already anchored by a trusted descriptor.
    ///
    /// Docker Hub redirects blobs to a CDN whose final response retains the
    /// exact `Content-Length` and uses `application/octet-stream`, but omits
    /// `Docker-Content-Digest`. If that header is present it is still parsed
    /// and required to match by the common validation path below.
    #[must_use]
    pub const fn descriptor_headers() -> Self {
        Self {
            require_content_length: true,
            require_content_type: true,
            require_registry_digest: false,
        }
    }
}

/// Proof that a complete registry response matched a trusted descriptor.
#[derive(Clone, Copy, Debug)]
pub struct ValidatedResponse<'response> {
    response: &'response RegistryResponse,
}

impl<'response> ValidatedResponse<'response> {
    #[must_use]
    pub fn response(&self) -> &'response RegistryResponse {
        self.response
    }

    #[must_use]
    pub fn body(&self) -> &'response [u8] {
        &self.response.body
    }
}

impl RegistryResponse {
    pub fn validate_descriptor(
        &self,
        descriptor: &Descriptor,
        policy: ValidationPolicy,
    ) -> Result<ValidatedResponse<'_>, ResponseValidationError> {
        descriptor.validate()?;
        if self.status != 200 {
            return Err(ResponseValidationError::UnexpectedStatus(self.status));
        }
        let actual_size =
            u64::try_from(self.body.len()).map_err(|_| ResponseValidationError::BodyTooLarge)?;
        if actual_size != descriptor.size {
            return Err(ResponseValidationError::DescriptorSizeMismatch {
                expected: descriptor.size,
                actual: actual_size,
            });
        }

        validate_descriptor_metadata(self.status, &self.headers, descriptor, policy)?;
        descriptor.digest.verify(&self.body)?;

        Ok(ValidatedResponse { response: self })
    }
}

impl<B> RegistryStreamResponse<B> {
    /// Parses a canonical `Content-Length` without consuming the body.
    pub fn content_length(&self) -> Result<Option<u64>, ResponseValidationError> {
        self.headers
            .get_single("content-length")?
            .map(parse_content_length)
            .transpose()
    }

    /// Validates all descriptor-derived response metadata without reading the
    /// body. A caller should invoke this before passing `body` to a parser or
    /// content-addressed store.
    pub fn validate_descriptor_metadata(
        &self,
        descriptor: &Descriptor,
        policy: ValidationPolicy,
    ) -> Result<(), ResponseValidationError> {
        validate_descriptor_metadata(self.status, &self.headers, descriptor, policy)
    }
}

fn validate_descriptor_metadata(
    status: u16,
    headers: &HeaderMap,
    descriptor: &Descriptor,
    policy: ValidationPolicy,
) -> Result<(), ResponseValidationError> {
    descriptor.validate()?;
    if status != 200 {
        return Err(ResponseValidationError::UnexpectedStatus(status));
    }

    match headers.get_single("content-length")? {
        Some(value) => {
            let content_length = parse_content_length(value)?;
            if content_length != descriptor.size {
                return Err(ResponseValidationError::ContentLengthMismatch {
                    header: content_length,
                    actual: descriptor.size,
                });
            }
        }
        None if policy.require_content_length => {
            return Err(ResponseValidationError::MissingContentLength);
        }
        None => {}
    }

    validate_content_type(headers, descriptor, policy)?;
    validate_registry_digest(headers, descriptor, policy)?;
    Ok(())
}

fn validate_content_type(
    headers: &HeaderMap,
    descriptor: &Descriptor,
    policy: ValidationPolicy,
) -> Result<(), ResponseValidationError> {
    let Some(header) = headers.get_single("content-type")? else {
        return if policy.require_content_type {
            Err(ResponseValidationError::MissingContentType)
        } else {
            Ok(())
        };
    };
    let value = header.split(';').next().unwrap_or_default().trim();
    let media_type = MediaType::from_str(value)?;
    if media_type == descriptor.media_type
        || (!descriptor.media_type.is_manifest()
            && !descriptor.media_type.is_index()
            && media_type == MediaType::OctetStream)
    {
        Ok(())
    } else {
        Err(ResponseValidationError::ContentTypeMismatch {
            expected: descriptor.media_type.clone(),
            actual: media_type,
        })
    }
}

fn validate_registry_digest(
    headers: &HeaderMap,
    descriptor: &Descriptor,
    policy: ValidationPolicy,
) -> Result<(), ResponseValidationError> {
    let Some(header) = headers.get_single("docker-content-digest")? else {
        return if policy.require_registry_digest {
            Err(ResponseValidationError::MissingRegistryDigest)
        } else {
            Ok(())
        };
    };
    let digest = Digest::from_str(header.trim())?;
    if digest == descriptor.digest {
        Ok(())
    } else {
        Err(ResponseValidationError::RegistryDigestMismatch {
            expected: descriptor.digest.clone(),
            actual: digest,
        })
    }
}

pub(crate) fn parse_content_length(value: &str) -> Result<u64, ResponseValidationError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ResponseValidationError::InvalidContentLength);
    }
    value
        .parse::<u64>()
        .map_err(|_| ResponseValidationError::InvalidContentLength)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResponseValidationError {
    #[error("registry returned unexpected HTTP status {0}")]
    UnexpectedStatus(u16),
    #[error("response body length cannot be represented as u64")]
    BodyTooLarge,
    #[error("response is missing Content-Length")]
    MissingContentLength,
    #[error("descriptor size mismatch: expected {expected}, got {actual}")]
    DescriptorSizeMismatch { expected: u64, actual: u64 },
    #[error("Content-Length is not a canonical unsigned integer")]
    InvalidContentLength,
    #[error("Content-Length mismatch: header said {header}, body has {actual} bytes")]
    ContentLengthMismatch { header: u64, actual: u64 },
    #[error("response is missing Content-Type")]
    MissingContentType,
    #[error("Content-Type mismatch: expected {expected}, got {actual}")]
    ContentTypeMismatch {
        expected: MediaType,
        actual: MediaType,
    },
    #[error("response is missing Docker-Content-Digest")]
    MissingRegistryDigest,
    #[error("registry digest mismatch: expected {expected}, got {actual}")]
    RegistryDigestMismatch { expected: Digest, actual: Digest },
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error(transparent)]
    DigestParse(#[from] DigestParseError),
    #[error(transparent)]
    DigestValidation(#[from] DigestValidationError),
    #[error(transparent)]
    MediaType(#[from] MediaTypeParseError),
    #[error(transparent)]
    Model(#[from] ModelValidationError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{HeaderMap, MediaType};

    use super::*;

    fn fixture() -> (Descriptor, RegistryResponse) {
        let body = b"verified blob".to_vec();
        let digest = Digest::sha256(&body);
        let descriptor = Descriptor {
            media_type: MediaType::OciImageLayer,
            digest: digest.clone(),
            size: body.len() as u64,
            urls: Vec::new(),
            annotations: BTreeMap::new(),
            data: None,
            platform: None,
            artifact_type: None,
        };
        let mut headers = HeaderMap::default();
        headers
            .insert("content-type", MediaType::OCI_IMAGE_LAYER)
            .unwrap();
        headers
            .insert("content-length", body.len().to_string())
            .unwrap();
        headers
            .insert("docker-content-digest", digest.to_string())
            .unwrap();
        let response = RegistryResponse {
            status: 200,
            headers,
            body,
        };
        (descriptor, response)
    }

    #[test]
    fn validates_body_size_digest_and_integrity_headers() {
        let (descriptor, response) = fixture();
        let validated = response
            .validate_descriptor(&descriptor, ValidationPolicy::strict_headers())
            .unwrap();
        assert_eq!(validated.body(), b"verified blob");
    }

    #[test]
    fn body_digest_is_always_verified_even_when_headers_are_optional() {
        let (descriptor, mut response) = fixture();
        response.headers = HeaderMap::default();
        response.body[0] ^= 1;

        assert!(matches!(
            response.validate_descriptor(&descriptor, ValidationPolicy::default()),
            Err(ResponseValidationError::DigestValidation(
                DigestValidationError::Mismatch { .. }
            ))
        ));
    }

    #[test]
    fn rejects_size_content_type_and_registry_digest_mismatches() {
        let (mut descriptor, response) = fixture();
        descriptor.size += 1;
        assert!(matches!(
            response.validate_descriptor(&descriptor, ValidationPolicy::default()),
            Err(ResponseValidationError::DescriptorSizeMismatch { .. })
        ));

        let (descriptor, mut response) = fixture();
        response
            .headers
            .insert("content-type", MediaType::OCI_IMAGE_CONFIG)
            .unwrap();
        assert!(matches!(
            response.validate_descriptor(&descriptor, ValidationPolicy::default()),
            Err(ResponseValidationError::ContentTypeMismatch { .. })
        ));

        let (descriptor, mut response) = fixture();
        response
            .headers
            .insert(
                "docker-content-digest",
                Digest::sha256(b"other").to_string(),
            )
            .unwrap();
        assert!(matches!(
            response.validate_descriptor(&descriptor, ValidationPolicy::default()),
            Err(ResponseValidationError::RegistryDigestMismatch { .. })
        ));
    }

    #[test]
    fn strict_policy_requires_both_integrity_headers() {
        let (descriptor, mut response) = fixture();
        response.headers = HeaderMap::default();
        assert_eq!(
            response
                .validate_descriptor(&descriptor, ValidationPolicy::strict_headers())
                .unwrap_err(),
            ResponseValidationError::MissingContentLength
        );
    }

    #[test]
    fn descriptor_policy_accepts_docker_cdn_metadata_without_registry_digest() {
        let (descriptor, mut response) = fixture();
        let mut headers = HeaderMap::default();
        headers
            .insert("content-type", MediaType::OCTET_STREAM)
            .unwrap();
        headers
            .insert("content-length", descriptor.size.to_string())
            .unwrap();
        response.headers = headers;

        assert!(
            response
                .validate_descriptor(&descriptor, ValidationPolicy::descriptor_headers())
                .is_ok()
        );
    }

    #[test]
    fn descriptor_policy_still_requires_size_type_and_matches_optional_digest() {
        let (descriptor, mut response) = fixture();
        let mut missing_length = HeaderMap::default();
        missing_length
            .insert("content-type", MediaType::OCTET_STREAM)
            .unwrap();
        response.headers = missing_length;
        assert_eq!(
            response
                .validate_descriptor(&descriptor, ValidationPolicy::descriptor_headers())
                .unwrap_err(),
            ResponseValidationError::MissingContentLength
        );

        let (descriptor, mut response) = fixture();
        let mut missing_type = HeaderMap::default();
        missing_type
            .insert("content-length", descriptor.size.to_string())
            .unwrap();
        response.headers = missing_type;
        assert_eq!(
            response
                .validate_descriptor(&descriptor, ValidationPolicy::descriptor_headers())
                .unwrap_err(),
            ResponseValidationError::MissingContentType
        );

        let (descriptor, mut response) = fixture();
        response
            .headers
            .insert(
                "docker-content-digest",
                Digest::sha256(b"other").to_string(),
            )
            .unwrap();
        assert!(matches!(
            response.validate_descriptor(&descriptor, ValidationPolicy::descriptor_headers()),
            Err(ResponseValidationError::RegistryDigestMismatch { .. })
        ));
    }

    #[test]
    fn streaming_validation_checks_headers_without_reading_body() {
        struct PanicReader;

        impl std::io::Read for PanicReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                panic!("streaming validation must not read the response body")
            }
        }

        let (descriptor, response) = fixture();
        let stream = RegistryStreamResponse {
            status: response.status,
            headers: response.headers,
            body: PanicReader,
        };
        stream
            .validate_descriptor_metadata(&descriptor, ValidationPolicy::strict_headers())
            .unwrap();
    }

    #[test]
    fn strict_streaming_validation_requires_canonical_content_length() {
        let (descriptor, mut response) = fixture();
        response
            .headers
            .insert("content-length", format!("0{}", descriptor.size))
            .unwrap();
        let stream = response.into_stream();

        assert_eq!(
            stream
                .validate_descriptor_metadata(&descriptor, ValidationPolicy::strict_headers())
                .unwrap_err(),
            ResponseValidationError::InvalidContentLength
        );
    }

    #[test]
    fn opaque_blob_content_type_is_valid_for_non_manifest_descriptors() {
        let (descriptor, mut response) = fixture();
        response
            .headers
            .insert("content-type", MediaType::OCTET_STREAM)
            .unwrap();

        assert!(
            response
                .validate_descriptor(&descriptor, ValidationPolicy::strict_headers())
                .is_ok()
        );
    }

    #[test]
    fn redirects_and_partial_responses_must_be_handled_by_transport() {
        let (descriptor, mut response) = fixture();
        response.status = 307;
        assert_eq!(
            response
                .validate_descriptor(&descriptor, ValidationPolicy::default())
                .unwrap_err(),
            ResponseValidationError::UnexpectedStatus(307)
        );
    }
}
