use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Digest, MediaType};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub media_type: MediaType,
    pub digest: Digest,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<MediaType>,
}

impl Descriptor {
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        if self.size > i64::MAX as u64 {
            return Err(ModelValidationError::DescriptorSizeOutOfRange(self.size));
        }
        if self
            .urls
            .iter()
            .any(|url| url.is_empty() || url.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err(ModelValidationError::InvalidDescriptorUrl);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Platform {
    pub architecture: String,
    pub os: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(
        default,
        rename = "os.version",
        skip_serializing_if = "Option::is_none"
    )]
    pub os_version: Option<String>,
    #[serde(default, rename = "os.features", skip_serializing_if = "Vec::is_empty")]
    pub os_features: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageManifest {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<MediaType>,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<MediaType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Descriptor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl ImageManifest {
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        validate_schema(self.schema_version)?;
        if self
            .media_type
            .as_ref()
            .is_some_and(|media_type| !media_type.is_manifest())
        {
            return Err(ModelValidationError::UnexpectedMediaType(
                self.media_type.clone().unwrap(),
            ));
        }
        self.config.validate()?;
        for layer in &self.layers {
            layer.validate()?;
        }
        if let Some(subject) = &self.subject {
            subject.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageIndex {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<MediaType>,
    pub manifests: Vec<Descriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<MediaType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Descriptor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl ImageIndex {
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        validate_schema(self.schema_version)?;
        if self
            .media_type
            .as_ref()
            .is_some_and(|media_type| !media_type.is_index())
        {
            return Err(ModelValidationError::UnexpectedMediaType(
                self.media_type.clone().unwrap(),
            ));
        }
        for manifest in &self.manifests {
            manifest.validate()?;
        }
        if let Some(subject) = &self.subject {
            subject.validate()?;
        }
        Ok(())
    }
}

/// A manifest response decoded by shape. Call [`ManifestDocument::validate`]
/// after deserialization before trusting descriptor metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestDocument {
    Manifest(Box<ImageManifest>),
    Index(Box<ImageIndex>),
}

impl ManifestDocument {
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        match self {
            Self::Manifest(manifest) => manifest.validate(),
            Self::Index(index) => index.validate(),
        }
    }
}

fn validate_schema(schema_version: u32) -> Result<(), ModelValidationError> {
    if schema_version == 2 {
        Ok(())
    } else {
        Err(ModelValidationError::UnsupportedSchemaVersion(
            schema_version,
        ))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ModelValidationError {
    #[error("unsupported image schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("media type does not match document shape: {0}")]
    UnexpectedMediaType(MediaType),
    #[error("descriptor size exceeds the OCI signed 64-bit range: {0}")]
    DescriptorSizeOutOfRange(u64),
    #[error("descriptor contains an invalid URL")]
    InvalidDescriptorUrl,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(media_type: MediaType) -> Descriptor {
        Descriptor {
            media_type,
            digest: Digest::sha256(b"payload"),
            size: 7,
            urls: Vec::new(),
            annotations: BTreeMap::new(),
            data: None,
            platform: None,
            artifact_type: None,
        }
    }

    #[test]
    fn decodes_oci_index_fields_and_platform_dotted_keys() {
        let json = format!(
            r#"{{
              "schemaVersion": 2,
              "mediaType": "{}",
              "manifests": [{{
                "mediaType": "{}",
                "digest": "{}",
                "size": 7,
                "platform": {{
                  "architecture": "arm64",
                  "os": "linux",
                  "variant": "v8",
                  "os.version": "6.6",
                  "os.features": ["cgroups"]
                }}
              }}]
            }}"#,
            MediaType::OCI_IMAGE_INDEX,
            MediaType::OCI_IMAGE_MANIFEST,
            Digest::sha256(b"payload")
        );
        let document: ManifestDocument = serde_json::from_str(&json).unwrap();
        let ManifestDocument::Index(index) = document else {
            panic!("expected image index");
        };

        assert_eq!(
            index.manifests[0]
                .platform
                .as_ref()
                .unwrap()
                .variant
                .as_deref(),
            Some("v8")
        );
        assert_eq!(
            index.manifests[0].platform.as_ref().unwrap().os_features,
            ["cgroups"]
        );
        assert_eq!(index.validate(), Ok(()));
    }

    #[test]
    fn validates_shape_media_type_and_schema() {
        let manifest = ImageManifest {
            schema_version: 1,
            media_type: Some(MediaType::OciImageIndex),
            config: descriptor(MediaType::OciImageConfig),
            layers: Vec::new(),
            artifact_type: None,
            subject: None,
            annotations: BTreeMap::new(),
        };
        assert_eq!(
            manifest.validate(),
            Err(ModelValidationError::UnsupportedSchemaVersion(1))
        );

        let manifest = ImageManifest {
            schema_version: 2,
            ..manifest
        };
        assert_eq!(
            manifest.validate(),
            Err(ModelValidationError::UnexpectedMediaType(
                MediaType::OciImageIndex
            ))
        );
    }

    #[test]
    fn rejects_descriptor_size_outside_oci_range() {
        let descriptor = Descriptor {
            size: i64::MAX as u64 + 1,
            ..descriptor(MediaType::OciImageLayer)
        };
        assert!(matches!(
            descriptor.validate(),
            Err(ModelValidationError::DescriptorSizeOutOfRange(_))
        ));
    }
}
