use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{Descriptor, ImageIndex, Platform};

pub const MAX_GUEST_PLATFORM_TOKEN_BYTES: usize = 32;

/// Guest platforms accepted by the mobile image-pull boundary.
///
/// This is deliberately a closed enum: accepting an image for storage does
/// not imply that an execution backend for its architecture is available.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum GuestPlatform {
    #[default]
    #[serde(rename = "linux/arm64/v8")]
    LinuxArm64V8,
    #[serde(rename = "linux/amd64")]
    LinuxAmd64,
}

impl GuestPlatform {
    #[must_use]
    pub const fn os(self) -> &'static str {
        "linux"
    }

    #[must_use]
    pub const fn architecture(self) -> &'static str {
        match self {
            Self::LinuxArm64V8 => "arm64",
            Self::LinuxAmd64 => "amd64",
        }
    }

    #[must_use]
    pub const fn variant(self) -> Option<&'static str> {
        match self {
            Self::LinuxArm64V8 => Some("v8"),
            Self::LinuxAmd64 => None,
        }
    }

    /// Returns an exact index-selection request for this guest platform.
    #[must_use]
    pub fn selection_request(self) -> PlatformRequest {
        match self {
            Self::LinuxArm64V8 => PlatformRequest::new("linux", "arm64")
                .with_variant_preferences(["v8"])
                .allow_variantless_fallback(false),
            Self::LinuxAmd64 => {
                PlatformRequest::new("linux", "amd64").allow_variantless_fallback(true)
            }
        }
    }
}

impl<'de> Deserialize<'de> for GuestPlatform {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct GuestPlatformVisitor;

        impl Visitor<'_> for GuestPlatformVisitor {
            type Value = GuestPlatform;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a supported bounded guest platform token")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_GUEST_PLATFORM_TOKEN_BYTES {
                    return Err(E::custom("guest platform token exceeds its byte limit"));
                }
                match value {
                    "linux/arm64/v8" => Ok(GuestPlatform::LinuxArm64V8),
                    "linux/amd64" => Ok(GuestPlatform::LinuxAmd64),
                    _ => Err(E::custom("unsupported guest platform")),
                }
            }
        }

        deserializer.deserialize_str(GuestPlatformVisitor)
    }
}

/// Ordered platform constraints for selecting a manifest from an OCI index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformRequest {
    os: String,
    architecture: String,
    variant_preferences: Vec<String>,
    allow_variantless_fallback: bool,
    os_version: Option<String>,
    required_os_features: Vec<String>,
}

impl PlatformRequest {
    #[must_use]
    pub fn new(os: impl Into<String>, architecture: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            architecture: architecture.into(),
            variant_preferences: Vec::new(),
            allow_variantless_fallback: true,
            os_version: None,
            required_os_features: Vec::new(),
        }
    }

    /// A Linux ARM64 request. ARM64 `v8` is preferred by default; a more
    /// specific host variant is considered before `v8`, followed by a
    /// variantless manifest.
    #[must_use]
    pub fn linux_arm64(preferred_variant: Option<&str>) -> Self {
        let mut request = Self::new("linux", "arm64");
        if let Some(variant) = preferred_variant.filter(|variant| !variant.is_empty()) {
            request.variant_preferences.push(variant.to_owned());
        }
        if !request
            .variant_preferences
            .iter()
            .any(|variant| variant == "v8")
        {
            request.variant_preferences.push("v8".to_owned());
        }
        request
    }

    #[must_use]
    pub fn with_variant_preferences<I, S>(mut self, variants: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.variant_preferences = variants
            .into_iter()
            .map(Into::into)
            .filter(|variant| !variant.is_empty())
            .collect();
        self
    }

    #[must_use]
    pub fn allow_variantless_fallback(mut self, allow: bool) -> Self {
        self.allow_variantless_fallback = allow;
        self
    }

    #[must_use]
    pub fn with_os_version(mut self, os_version: impl Into<String>) -> Self {
        self.os_version = Some(os_version.into());
        self
    }

    #[must_use]
    pub fn requiring_os_features<I, S>(mut self, features: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_os_features = features.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    #[must_use]
    pub fn variant_preferences(&self) -> &[String] {
        &self.variant_preferences
    }
}

/// Selects the best compatible descriptor, preserving index order as the
/// deterministic tie-breaker.
pub fn select_platform<'index>(
    index: &'index ImageIndex,
    request: &PlatformRequest,
) -> Result<&'index Descriptor, PlatformSelectionError> {
    index
        .manifests
        .iter()
        .filter(|descriptor| descriptor.media_type.is_manifest())
        .filter_map(|descriptor| {
            descriptor
                .platform
                .as_ref()
                .and_then(|platform| score(platform, request).map(|score| (score, descriptor)))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, descriptor)| descriptor)
        .ok_or_else(|| PlatformSelectionError::NoMatch {
            os: request.os.clone(),
            architecture: request.architecture.clone(),
            variants: request.variant_preferences.clone(),
        })
}

fn score(platform: &Platform, request: &PlatformRequest) -> Option<usize> {
    if platform.os != request.os || platform.architecture != request.architecture {
        return None;
    }
    if request
        .os_version
        .as_ref()
        .is_some_and(|version| platform.os_version.as_ref() != Some(version))
    {
        return None;
    }
    if !request
        .required_os_features
        .iter()
        .all(|required| platform.os_features.contains(required))
    {
        return None;
    }

    match platform.variant.as_deref() {
        Some(variant) => request
            .variant_preferences
            .iter()
            .position(|preferred| preferred == variant),
        None if request.allow_variantless_fallback => Some(request.variant_preferences.len()),
        None => None,
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PlatformSelectionError {
    #[error(
        "image index has no compatible {os}/{architecture} manifest (preferred variants: {variants:?})"
    )]
    NoMatch {
        os: String,
        architecture: String,
        variants: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{Digest, MediaType};

    use super::*;

    fn manifest(os: &str, architecture: &str, variant: Option<&str>, marker: u8) -> Descriptor {
        Descriptor {
            media_type: MediaType::OciImageManifest,
            digest: Digest::sha256(&[marker]),
            size: 1,
            urls: Vec::new(),
            annotations: BTreeMap::new(),
            data: None,
            platform: Some(Platform {
                architecture: architecture.to_owned(),
                os: os.to_owned(),
                variant: variant.map(str::to_owned),
                os_version: None,
                os_features: Vec::new(),
                features: Vec::new(),
            }),
            artifact_type: None,
        }
    }

    fn index(manifests: Vec<Descriptor>) -> ImageIndex {
        ImageIndex {
            schema_version: 2,
            media_type: Some(MediaType::OciImageIndex),
            manifests,
            artifact_type: None,
            subject: None,
            annotations: BTreeMap::new(),
        }
    }

    #[test]
    fn linux_arm64_prefers_specific_variant_then_v8_then_generic() {
        let index = index(vec![
            manifest("linux", "arm64", None, 1),
            manifest("linux", "arm64", Some("v8"), 2),
            manifest("linux", "arm64", Some("v8.2"), 3),
        ]);

        let selected =
            select_platform(&index, &PlatformRequest::linux_arm64(Some("v8.2"))).unwrap();
        assert_eq!(selected.digest, Digest::sha256(&[3]));

        let selected = select_platform(&index, &PlatformRequest::linux_arm64(None)).unwrap();
        assert_eq!(selected.digest, Digest::sha256(&[2]));
    }

    #[test]
    fn linux_arm64_falls_back_to_variantless_but_not_unlisted_variant() {
        let generic = index(vec![
            manifest("linux", "amd64", None, 1),
            manifest("linux", "arm64", None, 2),
        ]);
        assert_eq!(
            select_platform(&generic, &PlatformRequest::linux_arm64(None))
                .unwrap()
                .digest,
            Digest::sha256(&[2])
        );

        let incompatible = index(vec![manifest("linux", "arm64", Some("v9"), 3)]);
        assert!(
            select_platform(&incompatible, &PlatformRequest::linux_arm64(Some("v8.2"))).is_err()
        );
    }

    #[test]
    fn variantless_fallback_can_be_disabled() {
        let index = index(vec![manifest("linux", "arm64", None, 1)]);
        let request = PlatformRequest::linux_arm64(None).allow_variantless_fallback(false);
        assert!(select_platform(&index, &request).is_err());
    }

    #[test]
    fn non_manifest_artifacts_and_missing_platform_are_ignored() {
        let mut artifact = manifest("linux", "arm64", Some("v8"), 1);
        artifact.media_type = MediaType::Other("application/vnd.example.signature".to_owned());
        let mut missing = manifest("linux", "arm64", Some("v8"), 2);
        missing.platform = None;

        assert!(
            select_platform(
                &index(vec![artifact, missing]),
                &PlatformRequest::linux_arm64(None)
            )
            .is_err()
        );
    }

    #[test]
    fn platform_features_and_os_version_are_hard_constraints() {
        let mut matching = manifest("linux", "arm64", Some("v8"), 1);
        let platform = matching.platform.as_mut().unwrap();
        platform.os_version = Some("6.6".to_owned());
        platform.os_features = vec!["cgroup-v2".to_owned(), "kvm".to_owned()];
        let request = PlatformRequest::linux_arm64(None)
            .with_os_version("6.6")
            .requiring_os_features(["cgroup-v2"]);

        assert!(select_platform(&index(vec![matching]), &request).is_ok());
    }

    #[test]
    fn guest_platform_selection_is_exact_for_arm64_v8_and_amd64() {
        let candidates = index(vec![
            manifest("linux", "arm64", None, 1),
            manifest("linux", "arm64", Some("v8"), 2),
            manifest("linux", "arm64", Some("v9"), 3),
            manifest("linux", "amd64", None, 4),
            manifest("linux", "amd64", Some("future"), 5),
        ]);

        let arm64 = select_platform(
            &candidates,
            &GuestPlatform::LinuxArm64V8.selection_request(),
        )
        .unwrap();
        assert_eq!(arm64.digest, Digest::sha256(&[2]));

        let amd64 =
            select_platform(&candidates, &GuestPlatform::LinuxAmd64.selection_request()).unwrap();
        assert_eq!(amd64.digest, Digest::sha256(&[4]));
    }

    #[test]
    fn unsupported_guest_platform_or_variant_is_rejected_by_the_typed_boundary() {
        assert!(serde_json::from_str::<GuestPlatform>("\"linux/riscv64\"").is_err());
        assert!(serde_json::from_str::<GuestPlatform>("\"linux/arm64/v9\"").is_err());

        let incompatible = index(vec![
            manifest("linux", "arm64", Some("v9"), 1),
            manifest("linux", "amd64", Some("future"), 2),
        ]);
        assert!(
            select_platform(
                &incompatible,
                &GuestPlatform::LinuxArm64V8.selection_request()
            )
            .is_err()
        );
        assert!(
            select_platform(
                &incompatible,
                &GuestPlatform::LinuxAmd64.selection_request()
            )
            .is_err()
        );
    }
}
