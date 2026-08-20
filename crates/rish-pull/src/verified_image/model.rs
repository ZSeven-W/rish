use std::fmt;

use rish_registry::{Descriptor, Digest, MediaType};
use serde::{Deserialize, Serialize};

pub const VERIFIED_IMAGE_RECORD_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedDescriptor {
    pub media_type: MediaType,
    pub digest: Digest,
    pub size: u64,
}

impl From<&Descriptor> for VerifiedDescriptor {
    fn from(descriptor: &Descriptor) -> Self {
        Self {
            media_type: descriptor.media_type.clone(),
            digest: descriptor.digest.clone(),
            size: descriptor.size,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPlatform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedImageLayer {
    pub descriptor: VerifiedDescriptor,
    pub diff_id: Digest,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedProcessConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: String,
    pub user: String,
}

impl fmt::Debug for VerifiedProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedProcessConfig")
            .field("entrypoint_items", &self.entrypoint.len())
            .field("cmd_items", &self.cmd.len())
            .field("env", &"<redacted>")
            .field("working_dir", &"<redacted>")
            .field("user", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedImageRecord {
    pub schema_version: u32,
    pub normalized_reference: String,
    pub resolved_digest: Digest,
    pub index_descriptor: Option<VerifiedDescriptor>,
    pub manifest_descriptor: VerifiedDescriptor,
    pub config_descriptor: VerifiedDescriptor,
    pub platform: VerifiedPlatform,
    pub layers: Vec<VerifiedImageLayer>,
    pub process: VerifiedProcessConfig,
}

impl fmt::Debug for VerifiedImageRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedImageRecord")
            .field("schema_version", &self.schema_version)
            .field("normalized_reference", &self.normalized_reference)
            .field("resolved_digest", &self.resolved_digest)
            .field("has_index", &self.index_descriptor.is_some())
            .field("manifest_descriptor", &self.manifest_descriptor)
            .field("config_descriptor", &self.config_descriptor)
            .field("platform", &self.platform)
            .field("layer_count", &self.layers.len())
            .field("process", &"<redacted>")
            .finish()
    }
}
