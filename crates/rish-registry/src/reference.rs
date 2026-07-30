use std::fmt;
use std::net::Ipv6Addr;
use std::str::FromStr;

use thiserror::Error;

use crate::Digest;

const DEFAULT_REGISTRY: &str = "docker.io";
const DEFAULT_NAMESPACE: &str = "library";
const DEFAULT_TAG: &str = "latest";
const MAX_REPOSITORY_LENGTH: usize = 255;
const MAX_TAG_LENGTH: usize = 128;

/// A normalized Docker/OCI image reference.
///
/// Familiar references receive Docker's conventional defaults:
/// `alpine` becomes `docker.io/library/alpine:latest`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ImageReference {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

impl ImageReference {
    #[must_use]
    pub fn registry(&self) -> &str {
        &self.registry
    }

    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    #[must_use]
    pub fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }

    /// The immutable digest, or the tag used for the manifest request.
    #[must_use]
    pub fn manifest_reference(&self) -> String {
        self.digest.as_ref().map_or_else(
            || self.tag.as_deref().unwrap_or(DEFAULT_TAG).to_owned(),
            ToString::to_string,
        )
    }

    #[must_use]
    pub fn manifest_path(&self) -> String {
        format!(
            "/v2/{}/manifests/{}",
            self.repository,
            self.manifest_reference()
        )
    }

    #[must_use]
    pub fn blob_path(&self, digest: &Digest) -> String {
        format!("/v2/{}/blobs/{digest}", self.repository)
    }

    #[must_use]
    pub fn pull_scope(&self) -> String {
        format!("repository:{}:pull", self.repository)
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.registry, self.repository)?;
        if let Some(tag) = &self.tag {
            write!(formatter, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(formatter, "@{digest}")?;
        }
        Ok(())
    }
}

impl FromStr for ImageReference {
    type Err = ImageReferenceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(ImageReferenceError::Empty);
        }
        if value.trim() != value || value.contains("://") || value.contains(['?', '#']) {
            return Err(ImageReferenceError::InvalidSyntax(value.to_owned()));
        }

        let (name_and_tag, digest) = split_digest(value)?;
        let (name, tag) = split_tag(name_and_tag)?;
        let (registry, mut repository) = split_registry(name)?;

        validate_registry(&registry)?;
        validate_repository(&repository)?;
        if registry == DEFAULT_REGISTRY && !repository.contains('/') {
            repository = format!("{DEFAULT_NAMESPACE}/{repository}");
        }

        let tag = match (tag, digest.is_some()) {
            (Some(tag), _) => {
                validate_tag(tag)?;
                Some(tag.to_owned())
            }
            (None, false) => Some(DEFAULT_TAG.to_owned()),
            (None, true) => None,
        };

        Ok(Self {
            registry,
            repository,
            tag,
            digest,
        })
    }
}

fn split_digest(value: &str) -> Result<(&str, Option<Digest>), ImageReferenceError> {
    let mut parts = value.split('@');
    let name = parts.next().unwrap_or_default();
    let digest = parts.next();
    if parts.next().is_some() || name.is_empty() {
        return Err(ImageReferenceError::InvalidSyntax(value.to_owned()));
    }
    digest
        .map(|value| {
            if value.is_empty() {
                Err(ImageReferenceError::InvalidSyntax(value.to_owned()))
            } else {
                value.parse().map_err(ImageReferenceError::InvalidDigest)
            }
        })
        .transpose()
        .map(|digest| (name, digest))
}

fn split_tag(value: &str) -> Result<(&str, Option<&str>), ImageReferenceError> {
    let last_slash = value.rfind('/');
    let last_colon = value.rfind(':');
    if last_colon.is_some_and(|colon| last_slash.is_none_or(|slash| colon > slash)) {
        let colon = last_colon.unwrap();
        let (name, tag_with_colon) = value.split_at(colon);
        let tag = &tag_with_colon[1..];
        if name.is_empty() || tag.is_empty() {
            return Err(ImageReferenceError::InvalidSyntax(value.to_owned()));
        }
        Ok((name, Some(tag)))
    } else {
        Ok((value, None))
    }
}

fn split_registry(value: &str) -> Result<(String, String), ImageReferenceError> {
    let Some((first, remainder)) = value.split_once('/') else {
        return Ok((DEFAULT_REGISTRY.to_owned(), value.to_owned()));
    };
    if first.contains(['.', ':']) || first == "localhost" || first.starts_with('[') {
        if remainder.is_empty() {
            return Err(ImageReferenceError::InvalidRepository(value.to_owned()));
        }
        Ok((first.to_owned(), remainder.to_owned()))
    } else {
        Ok((DEFAULT_REGISTRY.to_owned(), value.to_owned()))
    }
}

fn validate_registry(registry: &str) -> Result<(), ImageReferenceError> {
    if registry.is_empty()
        || registry.contains(['/', '@'])
        || registry.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_uppercase() || byte.is_ascii_control()
        })
    {
        return Err(ImageReferenceError::InvalidRegistry(registry.to_owned()));
    }

    if registry.starts_with('[') {
        return validate_ipv6_registry(registry);
    }

    let (host, port) = match registry.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (registry, None),
    };
    if host.is_empty()
        || host.split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(ImageReferenceError::InvalidRegistry(registry.to_owned()));
    }
    validate_port(registry, port)
}

fn validate_ipv6_registry(registry: &str) -> Result<(), ImageReferenceError> {
    let Some(close) = registry.find(']') else {
        return Err(ImageReferenceError::InvalidRegistry(registry.to_owned()));
    };
    let address = &registry[1..close];
    if address.parse::<Ipv6Addr>().is_err() {
        return Err(ImageReferenceError::InvalidRegistry(registry.to_owned()));
    }
    let suffix = &registry[close + 1..];
    let port = if suffix.is_empty() {
        None
    } else if let Some(port) = suffix.strip_prefix(':') {
        Some(port)
    } else {
        return Err(ImageReferenceError::InvalidRegistry(registry.to_owned()));
    };
    validate_port(registry, port)
}

fn validate_port(registry: &str, port: Option<&str>) -> Result<(), ImageReferenceError> {
    if port.is_some_and(|value| value.parse::<u16>().map_or(true, |port| port == 0)) {
        Err(ImageReferenceError::InvalidRegistry(registry.to_owned()))
    } else {
        Ok(())
    }
}

fn validate_repository(repository: &str) -> Result<(), ImageReferenceError> {
    if repository.is_empty()
        || repository.len() > MAX_REPOSITORY_LENGTH
        || repository
            .split('/')
            .any(|part| !valid_name_component(part))
    {
        Err(ImageReferenceError::InvalidRepository(
            repository.to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn valid_name_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    if bytes.is_empty() || !is_lower_alphanumeric(bytes[0]) {
        return false;
    }

    let mut index = 1;
    while index < bytes.len() {
        if is_lower_alphanumeric(bytes[index]) {
            index += 1;
            continue;
        }

        match bytes[index] {
            b'.' => index += 1,
            b'_' => {
                index += 1;
                if bytes.get(index) == Some(&b'_') {
                    index += 1;
                }
            }
            b'-' => {
                while bytes.get(index) == Some(&b'-') {
                    index += 1;
                }
            }
            _ => return false,
        }
        if bytes
            .get(index)
            .is_none_or(|byte| !is_lower_alphanumeric(*byte))
        {
            return false;
        }
    }
    true
}

fn is_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn validate_tag(tag: &str) -> Result<(), ImageReferenceError> {
    let mut bytes = tag.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !valid_first
        || tag.len() > MAX_TAG_LENGTH
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        Err(ImageReferenceError::InvalidTag(tag.to_owned()))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ImageReferenceError {
    #[error("image reference is empty")]
    Empty,
    #[error("invalid image reference syntax: {0}")]
    InvalidSyntax(String),
    #[error("invalid registry authority: {0}")]
    InvalidRegistry(String),
    #[error("invalid image repository: {0}")]
    InvalidRepository(String),
    #[error("invalid image tag: {0}")]
    InvalidTag(String),
    #[error("invalid image digest: {0}")]
    InvalidDigest(#[source] crate::DigestParseError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn normalizes_familiar_docker_references() {
        let alpine: ImageReference = "alpine".parse().unwrap();
        let namespace: ImageReference = "team/api:1.2".parse().unwrap();

        assert_eq!(alpine.to_string(), "docker.io/library/alpine:latest");
        assert_eq!(
            alpine.manifest_path(),
            "/v2/library/alpine/manifests/latest"
        );
        assert_eq!(namespace.to_string(), "docker.io/team/api:1.2");
    }

    #[test]
    fn parses_registry_ports_ipv6_tags_and_digests() {
        let tagged: ImageReference = "registry.example:5443/team/api:v1".parse().unwrap();
        let ipv6: ImageReference = "[2001:db8::1]:5000/team/api:v1".parse().unwrap();
        let pinned: ImageReference = format!("ghcr.io/openminis/runtime:v1@{SHA256}")
            .parse()
            .unwrap();

        assert_eq!(tagged.registry(), "registry.example:5443");
        assert_eq!(ipv6.registry(), "[2001:db8::1]:5000");
        assert_eq!(pinned.tag(), Some("v1"));
        assert_eq!(pinned.digest().unwrap().to_string(), SHA256);
        assert_eq!(
            pinned.manifest_path(),
            format!("/v2/openminis/runtime/manifests/{SHA256}")
        );
    }

    #[test]
    fn digest_only_reference_does_not_gain_latest_tag() {
        let image: ImageReference = format!("example.com/a/b@{SHA256}").parse().unwrap();
        assert_eq!(image.tag(), None);
        assert_eq!(image.to_string(), format!("example.com/a/b@{SHA256}"));
    }

    #[test]
    fn rejects_ambiguous_or_noncanonical_references() {
        for invalid in [
            "",
            " https://example.com/a",
            "https://example.com/a",
            "Example.com/a",
            "example.com/Team/api",
            "example.com/a//b",
            "example.com/a..b",
            "example.com/-a",
            "example.com/a:",
            "example.com/a:-tag",
            "example.com:0/a",
            "example.com:99999/a",
            "example.com/a@",
            "example.com/a@sha256:abc",
            "example.com/a@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@x",
        ] {
            assert!(invalid.parse::<ImageReference>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn docker_name_separators_follow_distribution_grammar() {
        for valid in ["a_b", "a__b", "a-b", "a---b", "a.b"] {
            assert!(
                format!("example.com/{valid}")
                    .parse::<ImageReference>()
                    .is_ok()
            );
        }
        for invalid in ["a___b", "a_.b", "a--", "a.", "_a"] {
            assert!(
                format!("example.com/{invalid}")
                    .parse::<ImageReference>()
                    .is_err()
            );
        }
    }
}
