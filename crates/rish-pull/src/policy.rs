use crate::JsonLimits;

/// Maximum accepted bytes for an image manifest or index.
pub const DEFAULT_MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum accepted bytes for an image configuration.
pub const DEFAULT_MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum number of layer descriptors in one image manifest.
pub const DEFAULT_MAX_LAYERS: usize = 128;
/// Maximum compressed bytes accepted for one layer.
pub const DEFAULT_MAX_LAYER_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum aggregate bytes referenced by one pull.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Resource limits applied before bytes are admitted to the content store.
///
/// Manifest and config values can tighten, but never raise, their immutable
/// 8 MiB parser limits. Layers are never buffered by the puller.
///
/// The total limit counts each response the pipeline would fetch. Repeated
/// layer descriptors therefore count repeatedly, matching transport exposure
/// rather than deduplicated on-disk usage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PullPolicy {
    pub max_manifest_bytes: u64,
    pub max_config_bytes: u64,
    pub max_layers: usize,
    pub max_layer_bytes: u64,
    pub max_total_bytes: u64,
    pub json_limits: JsonLimits,
}

impl Default for PullPolicy {
    fn default() -> Self {
        Self {
            max_manifest_bytes: DEFAULT_MAX_MANIFEST_BYTES,
            max_config_bytes: DEFAULT_MAX_CONFIG_BYTES,
            max_layers: DEFAULT_MAX_LAYERS,
            max_layer_bytes: DEFAULT_MAX_LAYER_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            json_limits: JsonLimits::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_pull_defaults_fit_the_default_content_store() {
        let pull = PullPolicy::default();
        let store = rish_content::StoreConfig::new("unused");

        assert_eq!(pull.max_layer_bytes, 512 * 1024 * 1024);
        assert_eq!(pull.max_total_bytes, 2 * 1024 * 1024 * 1024);
        assert!(pull.max_layer_bytes <= store.max_blob_size);
        assert!(pull.max_total_bytes <= store.max_committed_bytes);
    }
}
