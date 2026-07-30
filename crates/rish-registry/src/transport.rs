use std::collections::BTreeMap;
use std::fmt;
use std::io::{Cursor, Read};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{Descriptor, ImageReference, MediaType};

/// Conservative default for tag/digest manifest responses.
///
/// A transport must enforce the request limit while streaming bytes from the
/// network. It must not buffer a layer into memory before returning it.
pub const DEFAULT_MANIFEST_RESPONSE_LIMIT: u64 = 8 * 1024 * 1024;
/// Maximum number of repeated HTTP header fields admitted from a transport.
pub const MAX_HEADER_FIELDS: usize = 128;
/// Maximum normalized HTTP header name length.
pub const MAX_HEADER_NAME_BYTES: usize = 128;
/// Maximum size of one HTTP header value.
pub const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
/// Maximum aggregate name/value bytes, including minimal delimiter overhead.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Head,
    Post,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistryScheme {
    Http,
    #[default]
    Https,
}

impl fmt::Display for RegistryScheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Http => "http",
            Self::Https => "https",
        })
    }
}

/// Case-insensitive HTTP headers with explicit support for repeated fields.
#[derive(Clone, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct HeaderMap(BTreeMap<String, Vec<String>>);

impl HeaderMap {
    pub fn insert(
        &mut self,
        name: impl AsRef<str>,
        value: impl Into<String>,
    ) -> Result<(), HeaderError> {
        let name = normalize_header_name(name.as_ref())?;
        let value = value.into();
        validate_header_value(&value)?;
        self.validate_projected_insert(&name, &value)?;
        self.0.insert(name, vec![value]);
        Ok(())
    }

    pub fn append(
        &mut self,
        name: impl AsRef<str>,
        value: impl Into<String>,
    ) -> Result<(), HeaderError> {
        let name = normalize_header_name(name.as_ref())?;
        let value = value.into();
        validate_header_value(&value)?;
        self.validate_projected_append(&name, &value)?;
        self.0.entry(name).or_default().push(value);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .get(&name.to_ascii_lowercase())
            .and_then(|values| values.first())
            .map(String::as_str)
    }

    #[must_use]
    pub fn get_all(&self, name: &str) -> &[String] {
        self.0
            .get(&name.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    pub fn get_single(&self, name: &str) -> Result<Option<&str>, HeaderError> {
        match self.get_all(name) {
            [] => Ok(None),
            [value] => Ok(Some(value)),
            values => Err(HeaderError::DuplicateSingleton {
                name: name.to_ascii_lowercase(),
                count: values.len(),
            }),
        }
    }

    #[must_use]
    pub fn contains_key(&self, name: &str) -> bool {
        self.0.contains_key(&name.to_ascii_lowercase())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.0
            .iter()
            .map(|(name, values)| (name.as_str(), values.as_slice()))
    }

    fn validate_projected_insert(&self, name: &str, value: &str) -> Result<(), HeaderError> {
        let (mut fields, mut bytes) = self.usage();
        if let Some(values) = self.0.get(name) {
            fields = fields.saturating_sub(values.len());
            for existing in values {
                bytes = bytes.saturating_sub(header_wire_bytes(name, existing));
            }
        }
        validate_header_limits(
            fields.saturating_add(1),
            bytes.saturating_add(header_wire_bytes(name, value)),
        )
    }

    fn validate_projected_append(&self, name: &str, value: &str) -> Result<(), HeaderError> {
        let (fields, bytes) = self.usage();
        validate_header_limits(
            fields.saturating_add(1),
            bytes.saturating_add(header_wire_bytes(name, value)),
        )
    }

    fn usage(&self) -> (usize, usize) {
        self.0
            .iter()
            .fold((0_usize, 0_usize), |(fields, bytes), (name, values)| {
                (
                    fields.saturating_add(values.len()),
                    values.iter().fold(bytes, |bytes, value| {
                        bytes.saturating_add(header_wire_bytes(name, value))
                    }),
                )
            })
    }
}

impl<'de> Deserialize<'de> for HeaderMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = BTreeMap::<String, Vec<String>>::deserialize(deserializer)?;
        let mut headers = Self::default();
        for (name, values) in encoded {
            if values.is_empty() {
                return Err(serde::de::Error::custom(format!(
                    "header {name} has no values"
                )));
            }
            let normalized = normalize_header_name(&name).map_err(serde::de::Error::custom)?;
            if headers.0.contains_key(&normalized) {
                return Err(serde::de::Error::custom(format!(
                    "header {normalized} appears with multiple casings"
                )));
            }
            for value in values {
                headers
                    .append(&normalized, value)
                    .map_err(serde::de::Error::custom)?;
            }
        }
        Ok(headers)
    }
}

impl fmt::Debug for HeaderMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = self
            .0
            .iter()
            .map(|(name, values)| {
                let values = if matches!(name.as_str(), "authorization" | "proxy-authorization") {
                    vec!["<redacted>".to_owned(); values.len()]
                } else {
                    values.clone()
                };
                (name, values)
            })
            .collect::<BTreeMap<_, _>>();
        formatter.debug_map().entries(redacted).finish()
    }
}

fn normalize_header_name(name: &str) -> Result<String, HeaderError> {
    if name.len() > MAX_HEADER_NAME_BYTES {
        return Err(HeaderError::NameTooLong {
            maximum: MAX_HEADER_NAME_BYTES,
            actual: name.len(),
        });
    }
    if name.is_empty() || !name.bytes().all(is_header_name_byte) {
        return Err(HeaderError::InvalidName(name.to_owned()));
    }
    Ok(name.to_ascii_lowercase())
}

fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn validate_header_value(value: &str) -> Result<(), HeaderError> {
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(HeaderError::ValueTooLong {
            maximum: MAX_HEADER_VALUE_BYTES,
            actual: value.len(),
        });
    }
    if value
        .bytes()
        .any(|byte| byte != b'\t' && !(b' '..=b'~').contains(&byte))
    {
        Err(HeaderError::InvalidValue)
    } else {
        Ok(())
    }
}

const fn header_wire_bytes(name: &str, value: &str) -> usize {
    name.len().saturating_add(value.len()).saturating_add(4)
}

fn validate_header_limits(fields: usize, bytes: usize) -> Result<(), HeaderError> {
    if fields > MAX_HEADER_FIELDS {
        return Err(HeaderError::TooManyFields {
            maximum: MAX_HEADER_FIELDS,
            actual: fields,
        });
    }
    if bytes > MAX_HEADER_BYTES {
        return Err(HeaderError::MapTooLarge {
            maximum: MAX_HEADER_BYTES,
            actual: bytes,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryRequest {
    pub method: HttpMethod,
    pub scheme: RegistryScheme,
    pub authority: String,
    pub path_and_query: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// Hard upper bound for the response body.
    ///
    /// [`RegistryTransport`] implementations must reject a larger declared
    /// `Content-Length` and stop reading once this many bytes have arrived.
    #[serde(default = "default_manifest_response_limit")]
    pub max_response_bytes: u64,
}

impl RegistryRequest {
    #[must_use]
    pub fn manifest(image: &ImageReference) -> Self {
        let mut headers = HeaderMap::default();
        headers
            .insert("accept", MediaType::manifest_accept_header())
            .expect("static Accept header is valid");
        Self {
            method: HttpMethod::Get,
            scheme: RegistryScheme::Https,
            authority: image.registry().to_owned(),
            path_and_query: image.manifest_path(),
            headers,
            body: Vec::new(),
            max_response_bytes: DEFAULT_MANIFEST_RESPONSE_LIMIT,
        }
    }

    #[must_use]
    pub fn blob(image: &ImageReference, descriptor: &Descriptor) -> Self {
        Self {
            method: HttpMethod::Get,
            scheme: RegistryScheme::Https,
            authority: image.registry().to_owned(),
            path_and_query: image.blob_path(&descriptor.digest),
            headers: HeaderMap::default(),
            body: Vec::new(),
            max_response_bytes: descriptor.size,
        }
    }

    /// Applies a policy-specific response limit for a manifest or blob.
    #[must_use]
    pub const fn with_response_body_limit(mut self, maximum: u64) -> Self {
        self.max_response_bytes = maximum;
        self
    }

    pub fn set_bearer_token(&mut self, token: &str) -> Result<(), HeaderError> {
        if token.is_empty()
            || !token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
            })
        {
            return Err(HeaderError::InvalidBearerToken);
        }
        self.headers
            .insert("authorization", format!("Bearer {token}"))
    }

    #[must_use]
    pub fn uri(&self) -> String {
        format!(
            "{}://{}{}",
            self.scheme, self.authority, self.path_and_query
        )
    }
}

/// Fully buffered response for bounded manifest/config handling and tests.
///
/// Production layer paths should use [`RegistryStreamResponse`] instead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl RegistryResponse {
    /// Converts a buffered response into a stream, primarily for tests and
    /// small manifest/config adapters.
    #[must_use]
    pub fn into_stream(self) -> RegistryStreamResponse<Cursor<Vec<u8>>> {
        RegistryStreamResponse {
            status: self.status,
            headers: self.headers,
            body: Cursor::new(self.body),
        }
    }
}

/// Registry response metadata plus a body that can be consumed incrementally.
///
/// A transport must return after headers are available and before buffering
/// the complete body. Callers can therefore validate status and integrity
/// headers before the first body read.
#[derive(Debug)]
pub struct RegistryStreamResponse<B> {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: B,
}

impl<B> RegistryStreamResponse<B> {
    #[must_use]
    pub fn map_body<M>(self, map: impl FnOnce(B) -> M) -> RegistryStreamResponse<M> {
        RegistryStreamResponse {
            status: self.status,
            headers: self.headers,
            body: map(self.body),
        }
    }
}

/// Deliberately transport-agnostic. Implementations may bridge to native
/// URLSession/OkHttp/ArkTS networking or use a Rust HTTP stack.
///
/// Implementations are part of the trusted runtime boundary: they must enforce
/// [`RegistryRequest::max_response_bytes`] incrementally while bytes arrive.
/// Checking only after constructing a complete body is not sufficient
/// protection against an untrusted registry. If more bytes arrive, `Body`
/// must return an I/O error; it must never disguise an over-limit response as
/// a clean EOF.
pub trait RegistryTransport {
    type Body: Read;

    fn execute(
        &self,
        request: &RegistryRequest,
    ) -> Result<RegistryStreamResponse<Self::Body>, TransportError>;
}

const fn default_manifest_response_limit() -> u64 {
    DEFAULT_MANIFEST_RESPONSE_LIMIT
}

#[derive(Clone, Debug, Error, Eq, PartialEq, Serialize, Deserialize)]
#[error("registry transport failed: {message}")]
pub struct TransportError {
    pub message: String,
    pub retryable: bool,
}

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum HeaderError {
    #[error("invalid HTTP header name: {0}")]
    InvalidName(String),
    #[error("HTTP header name is too long: maximum {maximum} bytes, got {actual}")]
    NameTooLong { maximum: usize, actual: usize },
    #[error("HTTP header value contains a forbidden byte")]
    InvalidValue,
    #[error("HTTP header value is too long: maximum {maximum} bytes, got {actual}")]
    ValueTooLong { maximum: usize, actual: usize },
    #[error("too many HTTP header fields: maximum {maximum}, got {actual}")]
    TooManyFields { maximum: usize, actual: usize },
    #[error("HTTP headers are too large: maximum {maximum} bytes, got {actual}")]
    MapTooLarge { maximum: usize, actual: usize },
    #[error("HTTP singleton header {name} appeared {count} times")]
    DuplicateSingleton { name: String, count: usize },
    #[error("bearer token is empty or contains a forbidden byte")]
    InvalidBearerToken,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::Digest;

    use super::*;

    fn descriptor() -> Descriptor {
        Descriptor {
            media_type: MediaType::OciImageLayerGzip,
            digest: Digest::sha256(b"blob"),
            size: 4,
            urls: Vec::new(),
            annotations: BTreeMap::new(),
            data: None,
            platform: None,
            artifact_type: None,
        }
    }

    #[test]
    fn builds_manifest_and_blob_requests_without_an_http_dependency() {
        let image: ImageReference = "ghcr.io/openminis/runtime:v1".parse().unwrap();
        let manifest = RegistryRequest::manifest(&image);
        let blob = RegistryRequest::blob(&image, &descriptor());

        assert_eq!(
            manifest.uri(),
            "https://ghcr.io/v2/openminis/runtime/manifests/v1"
        );
        assert!(
            manifest
                .headers
                .get("ACCEPT")
                .unwrap()
                .contains("image.index")
        );
        assert!(blob.path_and_query.contains("/blobs/sha256:"));
        assert_eq!(blob.max_response_bytes, descriptor().size);
        assert_eq!(manifest.max_response_bytes, DEFAULT_MANIFEST_RESPONSE_LIMIT);
    }

    #[test]
    fn header_names_are_case_insensitive_and_repetition_is_explicit() {
        let mut headers = HeaderMap::default();
        headers.insert("Content-Type", "application/json").unwrap();
        headers
            .append("WWW-Authenticate", "Bearer realm=\"a\"")
            .unwrap();
        headers
            .append("www-authenticate", "Basic realm=\"b\"")
            .unwrap();

        assert_eq!(headers.get("content-type"), Some("application/json"));
        assert_eq!(headers.get_all("WWW-AUTHENTICATE").len(), 2);
        assert!(matches!(
            headers.get_single("www-authenticate"),
            Err(HeaderError::DuplicateSingleton { count: 2, .. })
        ));
    }

    #[test]
    fn headers_reject_request_smuggling_bytes_and_debug_redacts_tokens() {
        let mut headers = HeaderMap::default();
        assert!(headers.insert("bad name", "x").is_err());
        assert!(headers.insert("x-test", "safe\r\ninjected: yes").is_err());
        headers.insert("authorization", "Bearer secret").unwrap();

        let debug = format!("{headers:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn headers_enforce_per_field_count_and_aggregate_limits() {
        let mut headers = HeaderMap::default();
        assert!(matches!(
            headers.insert(
                "x-name",
                "a".repeat(MAX_HEADER_VALUE_BYTES.saturating_add(1))
            ),
            Err(HeaderError::ValueTooLong { .. })
        ));

        for _ in 0..MAX_HEADER_FIELDS {
            headers.append("x-repeated", "a").unwrap();
        }
        assert!(matches!(
            headers.append("x-repeated", "a"),
            Err(HeaderError::TooManyFields { .. })
        ));

        let mut headers = HeaderMap::default();
        let value = "a".repeat(1024);
        while headers.append("x-large", &value).is_ok() {}
        assert!(matches!(
            headers.append("x-large", &value),
            Err(HeaderError::MapTooLarge { .. })
        ));
    }

    #[test]
    fn deserialization_cannot_bypass_header_validation() {
        assert!(
            serde_json::from_str::<HeaderMap>(
                r#"{"content-type":["ok"],"Content-Type":["also-ok"]}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<HeaderMap>(r#"{"x-test":["safe\r\ninjected: true"]}"#).is_err()
        );

        let headers: HeaderMap =
            serde_json::from_str(r#"{"content-type":["application/json"]}"#).unwrap();
        assert_eq!(
            headers.iter().next(),
            Some(("content-type", ["application/json".to_owned()].as_slice()))
        );
    }

    #[test]
    fn transport_can_be_supplied_by_the_mobile_host() {
        struct Mock;

        impl RegistryTransport for Mock {
            type Body = Cursor<Vec<u8>>;

            fn execute(
                &self,
                _request: &RegistryRequest,
            ) -> Result<RegistryStreamResponse<Self::Body>, TransportError> {
                Ok(RegistryResponse {
                    status: 200,
                    headers: HeaderMap::default(),
                    body: b"ok".to_vec(),
                }
                .into_stream())
            }
        }

        let image: ImageReference = "alpine".parse().unwrap();
        let mut response = Mock.execute(&RegistryRequest::manifest(&image)).unwrap();
        let mut body = Vec::new();
        response.body.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"ok");
    }
}
