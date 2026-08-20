use std::collections::BTreeSet;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use rish_content::{BlobDescriptor, ContentStore, Sha256Digest};
use rish_oci::ImageConfiguration;
use rish_registry::{
    Digest, GuestPlatform, ImageIndex, ImageManifest, ImageReference, MediaType, Platform,
};
use serde::{Deserialize, Serialize};

use crate::json_limits::validate_json_shape;
use crate::{
    DEFAULT_MAX_CONFIG_BYTES, DEFAULT_MAX_LAYERS, DEFAULT_MAX_MANIFEST_BYTES, JsonLimits,
    PulledImage,
};

use super::filesystem::{
    atomic_write_private, ensure_private_directory, read_bounded_regular_file,
};
use super::pins::{
    ensure_exact_pin_set, ensure_pin_contains, pin_graph_and_record, prune_record_pins,
    rollback_pins,
};
use super::{
    VERIFIED_IMAGE_RECORD_SCHEMA_VERSION, VerifiedDescriptor, VerifiedImageLayer,
    VerifiedImageRecord, VerifiedImageRecordError, VerifiedPlatform, VerifiedProcessConfig,
};

const VERIFIED_IMAGE_DIRECTORY: &str = "verified-images";
const RECORD_VERSION_DIRECTORY: &str = "v1";
const DIGEST_DIRECTORY: &str = "sha256";
const MAX_RECORD_BYTES: u64 = 512 * 1024;
const MAX_LOCATOR_BYTES: u64 = 1024;
const MAX_REFERENCE_BYTES: usize = 1024;
const MAX_MEDIA_TYPE_BYTES: usize = 256;
const MAX_VARIANT_BYTES: usize = 128;
const MAX_PROCESS_ITEMS: usize = 512;
const MAX_PROCESS_STRING_BYTES: usize = 16 * 1024;
const MAX_PROCESS_TOTAL_STRING_BYTES: usize = 256 * 1024;
const MAX_RECORD_TOTAL_STRING_BYTES: usize = 384 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordLocator {
    schema_version: u32,
    record_digest: Digest,
    record_size: u64,
}

pub struct VerifiedImageHandle {
    pub record: VerifiedImageRecord,
    pub graph_pin: String,
    pub record_digest: Sha256Digest,
}

impl fmt::Debug for VerifiedImageHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedImageHandle")
            .field("record", &self.record)
            .field("graph_pin", &self.graph_pin)
            .field("record_digest", &self.record_digest)
            .finish()
    }
}

pub struct VerifiedImageRecordStore<'store> {
    store: &'store ContentStore,
    locator_directory: PathBuf,
}

impl<'store> VerifiedImageRecordStore<'store> {
    pub fn open(store: &'store ContentStore) -> Result<Self, VerifiedImageRecordError> {
        let records = store.root().join(VERIFIED_IMAGE_DIRECTORY);
        ensure_private_directory(store.root(), &records)?;
        let version = records.join(RECORD_VERSION_DIRECTORY);
        ensure_private_directory(store.root(), &version)?;
        let locator_directory = version.join(DIGEST_DIRECTORY);
        ensure_private_directory(store.root(), &locator_directory)?;
        Ok(Self {
            store,
            locator_directory,
        })
    }

    /// Builds and durably roots a startup record for a fully verified pull.
    ///
    /// This persists metadata only. It does not start a container or provide a
    /// Linux execution backend.
    pub fn persist(
        &self,
        image: &PulledImage,
    ) -> Result<VerifiedImageHandle, VerifiedImageRecordError> {
        let record = build_record(image)?;
        validate_record_fields(&record)?;
        let encoded = serde_json::to_vec(&record)?;
        if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return Err(VerifiedImageRecordError::RecordTooLarge {
                maximum: MAX_RECORD_BYTES,
            });
        }

        let guest_platform = verified_guest_platform(&record.platform)?;
        let graph_pin = graph_pin_name(&record.resolved_digest, guest_platform)?;
        let record_pin = record_pin_name(&record.resolved_digest, guest_platform)?;
        let previous_record =
            self.current_record_digest(&record.resolved_digest, guest_platform)?;
        // The atomic locator is the commit point. Before staging its
        // replacement, prune leftovers that are not selected by the current
        // locator. After a crash, a serialized writer therefore retains at
        // most the selected record plus one staged replacement.
        prune_record_pins(self.store, &record_pin, previous_record)?;

        let stored_record = self.store.ingest_bytes(&encoded)?;
        let graph = record_graph_digests(&record)?;
        let (created_graph, created_record) = pin_graph_and_record(
            self.store,
            &graph_pin,
            &graph,
            &record_pin,
            stored_record.digest,
        )?;

        let persisted = (|| {
            ensure_exact_pin_set(self.store, &graph_pin, &graph)?;
            ensure_pin_contains(self.store, &record_pin, stored_record.digest)?;
            let locator = RecordLocator {
                schema_version: VERIFIED_IMAGE_RECORD_SCHEMA_VERSION,
                record_digest: stored_record.digest.to_string().parse().map_err(|_| {
                    VerifiedImageRecordError::InvalidField {
                        field: "record_digest",
                    }
                })?,
                record_size: stored_record.size,
            };
            let locator_bytes = serde_json::to_vec(&locator)?;
            if u64::try_from(locator_bytes.len()).unwrap_or(u64::MAX) > MAX_LOCATOR_BYTES {
                return Err(VerifiedImageRecordError::LocatorTooLarge {
                    maximum: MAX_LOCATOR_BYTES,
                });
            }
            atomic_write_private(
                &self.locator_directory,
                &self.locator_path(&record.resolved_digest, guest_platform)?,
                &locator_bytes,
            )
        })();

        if let Err(error) = persisted {
            rollback_pins(self.store, &record_pin, created_record);
            rollback_pins(self.store, &graph_pin, created_graph);
            return Err(error);
        }
        // The new locator is already durable. Obsolete record roots are
        // best-effort cleanup; the next serialized persist retries pruning
        // before it can stage another replacement.
        let _ = prune_record_pins(self.store, &record_pin, Some(stored_record.digest));

        Ok(VerifiedImageHandle {
            record,
            graph_pin,
            record_digest: stored_record.digest,
        })
    }

    /// Reopens the default Linux ARM64/v8 record after a process restart.
    ///
    /// This compatibility shorthand delegates to [`Self::reopen_for_platform`].
    pub fn reopen(
        &self,
        resolved_digest: &Digest,
    ) -> Result<VerifiedImageHandle, VerifiedImageRecordError> {
        self.reopen_for_platform(resolved_digest, GuestPlatform::default())
    }

    /// Reopens the record for one exact guest platform.
    ///
    /// A multi-platform index has one registry-resolved digest but distinct
    /// selected manifests and content graphs, so the platform is part of the
    /// durable record identity.
    pub fn reopen_for_platform(
        &self,
        resolved_digest: &Digest,
        guest_platform: GuestPlatform,
    ) -> Result<VerifiedImageHandle, VerifiedImageRecordError> {
        let requested = sha256_digest(resolved_digest, "resolved_digest")?;
        let locator_bytes = read_bounded_regular_file(
            &self.locator_path(resolved_digest, guest_platform)?,
            MAX_LOCATOR_BYTES,
        )?;
        let locator = serde_json::from_slice::<RecordLocator>(&locator_bytes)?;
        if locator.schema_version != VERIFIED_IMAGE_RECORD_SCHEMA_VERSION {
            return Err(VerifiedImageRecordError::UnsupportedSchema(
                locator.schema_version,
            ));
        }
        if locator.record_size > MAX_RECORD_BYTES {
            return Err(VerifiedImageRecordError::RecordTooLarge {
                maximum: MAX_RECORD_BYTES,
            });
        }
        let record_digest = sha256_digest(&locator.record_digest, "record_digest")?;
        let record_pin = record_pin_name(resolved_digest, guest_platform)?;
        ensure_pin_contains(self.store, &record_pin, record_digest)?;
        let record_bytes = read_verified_record_blob(
            self.store,
            BlobDescriptor::new(record_digest, locator.record_size),
        )?;
        validate_json_shape(&record_bytes, record_json_limits())
            .map_err(|_| VerifiedImageRecordError::JsonShape)?;
        let record = serde_json::from_slice::<VerifiedImageRecord>(&record_bytes)?;
        validate_record_fields(&record)?;
        if sha256_digest(&record.resolved_digest, "resolved_digest")? != requested {
            return Err(VerifiedImageRecordError::ResolvedDigestMismatch);
        }
        if verified_guest_platform(&record.platform)? != guest_platform {
            return Err(graph_mismatch("requested_platform"));
        }

        let graph_pin = graph_pin_name(&record.resolved_digest, guest_platform)?;
        let graph = record_graph_digests(&record)?;
        ensure_exact_pin_set(self.store, &graph_pin, &graph)?;
        verify_graph(self.store, &record)?;

        Ok(VerifiedImageHandle {
            record,
            graph_pin,
            record_digest,
        })
    }

    fn locator_path(
        &self,
        resolved_digest: &Digest,
        guest_platform: GuestPlatform,
    ) -> Result<PathBuf, VerifiedImageRecordError> {
        let digest = sha256_digest(resolved_digest, "resolved_digest")?;
        Ok(self.locator_directory.join(format!(
            "{}-{}.json",
            digest.encoded(),
            platform_storage_key(guest_platform)
        )))
    }

    fn current_record_digest(
        &self,
        resolved_digest: &Digest,
        guest_platform: GuestPlatform,
    ) -> Result<Option<Sha256Digest>, VerifiedImageRecordError> {
        let bytes = match read_bounded_regular_file(
            &self.locator_path(resolved_digest, guest_platform)?,
            MAX_LOCATOR_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(VerifiedImageRecordError::MissingLocator) => return Ok(None),
            Err(error) => return Err(error),
        };
        let locator = serde_json::from_slice::<RecordLocator>(&bytes)?;
        if locator.schema_version != VERIFIED_IMAGE_RECORD_SCHEMA_VERSION {
            return Err(VerifiedImageRecordError::UnsupportedSchema(
                locator.schema_version,
            ));
        }
        if locator.record_size > MAX_RECORD_BYTES {
            return Err(VerifiedImageRecordError::RecordTooLarge {
                maximum: MAX_RECORD_BYTES,
            });
        }
        sha256_digest(&locator.record_digest, "record_digest").map(Some)
    }
}

fn build_record(image: &PulledImage) -> Result<VerifiedImageRecord, VerifiedImageRecordError> {
    let diff_ids = image.layer_diff_ids();
    if diff_ids.len() != image.layers.len() {
        return Err(VerifiedImageRecordError::GraphMismatch {
            component: "layer_diff_ids",
        });
    }
    let layers = image
        .layers
        .iter()
        .zip(diff_ids)
        .map(|(blob, diff_id)| VerifiedImageLayer {
            descriptor: VerifiedDescriptor::from(&blob.descriptor),
            diff_id,
        })
        .collect();
    let config = &image.image_configuration.config;
    let variant = image
        .manifest
        .descriptor
        .platform
        .as_ref()
        .and_then(|platform| platform.variant.clone());

    Ok(VerifiedImageRecord {
        schema_version: VERIFIED_IMAGE_RECORD_SCHEMA_VERSION,
        normalized_reference: image.reference.to_string(),
        resolved_digest: image.resolved_digest().clone(),
        index_descriptor: image
            .index
            .as_ref()
            .map(|blob| VerifiedDescriptor::from(&blob.descriptor)),
        manifest_descriptor: VerifiedDescriptor::from(&image.manifest.descriptor),
        config_descriptor: VerifiedDescriptor::from(&image.config.descriptor),
        platform: VerifiedPlatform {
            os: image.image_configuration.os.clone(),
            architecture: image.image_configuration.architecture.clone(),
            variant,
        },
        layers,
        process: VerifiedProcessConfig {
            entrypoint: config.entrypoint.clone(),
            cmd: config.cmd.clone(),
            env: config.env.clone(),
            working_dir: config.working_dir.clone(),
            user: config.user.clone(),
        },
    })
}

fn validate_record_fields(record: &VerifiedImageRecord) -> Result<(), VerifiedImageRecordError> {
    if record.schema_version != VERIFIED_IMAGE_RECORD_SCHEMA_VERSION {
        return Err(VerifiedImageRecordError::UnsupportedSchema(
            record.schema_version,
        ));
    }
    if record.normalized_reference.len() > MAX_REFERENCE_BYTES {
        return Err(field_limit("normalized_reference"));
    }
    let parsed = record
        .normalized_reference
        .parse::<ImageReference>()
        .map_err(|_| invalid_field("normalized_reference"))?;
    if parsed.to_string() != record.normalized_reference {
        return Err(invalid_field("normalized_reference"));
    }
    validate_descriptor(
        record.index_descriptor.as_ref(),
        DescriptorKind::OptionalIndex,
    )?;
    validate_descriptor(Some(&record.manifest_descriptor), DescriptorKind::Manifest)?;
    validate_descriptor(Some(&record.config_descriptor), DescriptorKind::Config)?;
    let resolved = record
        .index_descriptor
        .as_ref()
        .unwrap_or(&record.manifest_descriptor);
    if record.resolved_digest != resolved.digest {
        return Err(VerifiedImageRecordError::ResolvedDigestMismatch);
    }
    if record.platform.os != "linux" {
        return Err(invalid_field("platform.os"));
    }
    if record
        .platform
        .variant
        .as_ref()
        .is_some_and(|variant| variant.len() > MAX_VARIANT_BYTES || variant.contains('\0'))
    {
        return Err(field_limit("platform.variant"));
    }
    verified_guest_platform(&record.platform)?;
    if record.layers.len() > DEFAULT_MAX_LAYERS {
        return Err(field_limit("layers"));
    }
    for layer in &record.layers {
        validate_descriptor(Some(&layer.descriptor), DescriptorKind::Layer)?;
        sha256_digest(&layer.diff_id, "layers.diff_id")?;
    }
    validate_process(&record.process)
}

fn validate_process(process: &VerifiedProcessConfig) -> Result<(), VerifiedImageRecordError> {
    for (field, values) in [
        ("process.entrypoint", process.entrypoint.as_slice()),
        ("process.cmd", process.cmd.as_slice()),
        ("process.env", process.env.as_slice()),
    ] {
        if values.len() > MAX_PROCESS_ITEMS {
            return Err(field_limit(field));
        }
    }
    let mut total = 0_usize;
    for (field, value) in process
        .entrypoint
        .iter()
        .map(|value| ("process.entrypoint", value))
        .chain(process.cmd.iter().map(|value| ("process.cmd", value)))
        .chain(process.env.iter().map(|value| ("process.env", value)))
        .chain([
            ("process.working_dir", &process.working_dir),
            ("process.user", &process.user),
        ])
    {
        if value.len() > MAX_PROCESS_STRING_BYTES {
            return Err(field_limit(field));
        }
        if value.contains('\0') {
            return Err(invalid_field(field));
        }
        total = total
            .checked_add(value.len())
            .ok_or_else(|| field_limit("process.total_string_bytes"))?;
    }
    if total > MAX_PROCESS_TOTAL_STRING_BYTES {
        return Err(field_limit("process.total_string_bytes"));
    }
    for entry in &process.env {
        let Some((name, _)) = entry.split_once('=') else {
            return Err(invalid_field("process.env"));
        };
        if name.is_empty() || name.contains('=') || name.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(invalid_field("process.env"));
        }
    }
    if !process.working_dir.is_empty() && !process.working_dir.starts_with('/') {
        return Err(invalid_field("process.working_dir"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum DescriptorKind {
    OptionalIndex,
    Manifest,
    Config,
    Layer,
}

fn validate_descriptor(
    descriptor: Option<&VerifiedDescriptor>,
    kind: DescriptorKind,
) -> Result<(), VerifiedImageRecordError> {
    let Some(descriptor) = descriptor else {
        return if matches!(kind, DescriptorKind::OptionalIndex) {
            Ok(())
        } else {
            Err(invalid_field("descriptor"))
        };
    };
    sha256_digest(&descriptor.digest, "descriptor.digest")?;
    if descriptor.size > i64::MAX as u64 {
        return Err(invalid_field("descriptor.size"));
    }
    if descriptor.media_type.as_str().len() > MAX_MEDIA_TYPE_BYTES {
        return Err(field_limit("descriptor.media_type"));
    }
    let supported = match kind {
        DescriptorKind::OptionalIndex => descriptor.media_type.is_index(),
        DescriptorKind::Manifest => descriptor.media_type.is_manifest(),
        DescriptorKind::Config => matches!(
            descriptor.media_type,
            MediaType::OciImageConfig | MediaType::DockerImageConfig
        ),
        DescriptorKind::Layer => matches!(
            descriptor.media_type,
            MediaType::OciImageLayer | MediaType::OciImageLayerGzip | MediaType::DockerLayerGzip
        ),
    };
    if supported {
        Ok(())
    } else {
        Err(invalid_field("descriptor.media_type"))
    }
}

fn verify_graph(
    store: &ContentStore,
    record: &VerifiedImageRecord,
) -> Result<(), VerifiedImageRecordError> {
    if let Some(index_descriptor) = &record.index_descriptor {
        let index_bytes =
            read_verified_small_blob(store, index_descriptor, DEFAULT_MAX_MANIFEST_BYTES)?;
        validate_json_shape(&index_bytes, JsonLimits::default())
            .map_err(|_| graph_mismatch("index_json"))?;
        let index = serde_json::from_slice::<ImageIndex>(&index_bytes)
            .map_err(|_| graph_mismatch("index_json"))?;
        index.validate().map_err(|_| graph_mismatch("index"))?;
        if !index.manifests.iter().any(|descriptor| {
            VerifiedDescriptor::from(descriptor) == record.manifest_descriptor
                && platform_matches(descriptor.platform.as_ref(), &record.platform)
        }) {
            return Err(graph_mismatch("index_manifest_link"));
        }
    }

    let manifest_bytes = read_verified_small_blob(
        store,
        &record.manifest_descriptor,
        DEFAULT_MAX_MANIFEST_BYTES,
    )?;
    validate_json_shape(&manifest_bytes, JsonLimits::default())
        .map_err(|_| graph_mismatch("manifest_json"))?;
    let manifest = serde_json::from_slice::<ImageManifest>(&manifest_bytes)
        .map_err(|_| graph_mismatch("manifest_json"))?;
    manifest
        .validate()
        .map_err(|_| graph_mismatch("manifest"))?;
    if VerifiedDescriptor::from(&manifest.config) != record.config_descriptor {
        return Err(graph_mismatch("manifest_config_link"));
    }
    let manifest_layers = manifest
        .layers
        .iter()
        .map(VerifiedDescriptor::from)
        .collect::<Vec<_>>();
    let record_layers = record
        .layers
        .iter()
        .map(|layer| layer.descriptor.clone())
        .collect::<Vec<_>>();
    if manifest_layers != record_layers {
        return Err(graph_mismatch("manifest_layer_links"));
    }

    let config_bytes =
        read_verified_small_blob(store, &record.config_descriptor, DEFAULT_MAX_CONFIG_BYTES)?;
    validate_json_shape(&config_bytes, JsonLimits::default())
        .map_err(|_| graph_mismatch("config_json"))?;
    let configuration = serde_json::from_slice::<ImageConfiguration>(&config_bytes)
        .map_err(|_| graph_mismatch("config_json"))?;
    configuration
        .validate_linux_guest_metadata()
        .map_err(|_| graph_mismatch("config"))?;
    if configuration.os != record.platform.os
        || configuration.architecture != record.platform.architecture
    {
        return Err(graph_mismatch("config_platform"));
    }
    let diff_ids = configuration
        .rootfs
        .diff_ids
        .iter()
        .map(|value| {
            value
                .parse::<Digest>()
                .map_err(|_| graph_mismatch("config_diff_ids"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let recorded_diff_ids = record
        .layers
        .iter()
        .map(|layer| layer.diff_id.clone())
        .collect::<Vec<_>>();
    if diff_ids != recorded_diff_ids {
        return Err(graph_mismatch("config_diff_ids"));
    }
    let expected_process = VerifiedProcessConfig {
        entrypoint: configuration.config.entrypoint,
        cmd: configuration.config.cmd,
        env: configuration.config.env,
        working_dir: configuration.config.working_dir,
        user: configuration.config.user,
    };
    if expected_process != record.process {
        return Err(graph_mismatch("config_process"));
    }

    for layer in &record.layers {
        store.verify(blob_descriptor(&layer.descriptor)?)?;
    }
    Ok(())
}

fn platform_matches(platform: Option<&Platform>, expected: &VerifiedPlatform) -> bool {
    platform.is_some_and(|platform| {
        platform.os == expected.os
            && platform.architecture == expected.architecture
            && platform.variant == expected.variant
    })
}

fn read_verified_record_blob(
    store: &ContentStore,
    descriptor: BlobDescriptor,
) -> Result<Vec<u8>, VerifiedImageRecordError> {
    if descriptor.size > MAX_RECORD_BYTES {
        return Err(VerifiedImageRecordError::RecordTooLarge {
            maximum: MAX_RECORD_BYTES,
        });
    }
    read_and_verify(store, descriptor, MAX_RECORD_BYTES)
}

fn read_verified_small_blob(
    store: &ContentStore,
    descriptor: &VerifiedDescriptor,
    maximum: u64,
) -> Result<Vec<u8>, VerifiedImageRecordError> {
    if descriptor.size > maximum {
        return Err(graph_mismatch("bounded_blob_size"));
    }
    read_and_verify(store, blob_descriptor(descriptor)?, maximum)
}

fn read_and_verify(
    store: &ContentStore,
    descriptor: BlobDescriptor,
    maximum: u64,
) -> Result<Vec<u8>, VerifiedImageRecordError> {
    let mut file = store.open_blob(descriptor.digest)?;
    let mut bytes = Vec::with_capacity(usize::try_from(descriptor.size).unwrap_or(usize::MAX));
    file.by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_size != descriptor.size {
        return Err(rish_content::StoreError::SizeMismatch {
            expected: descriptor.size,
            actual: actual_size,
        }
        .into());
    }
    let actual_digest = Sha256Digest::calculate(&bytes);
    if actual_digest != descriptor.digest {
        return Err(rish_content::StoreError::DigestMismatch {
            expected: descriptor.digest,
            actual: actual_digest,
        }
        .into());
    }
    Ok(bytes)
}

fn blob_descriptor(
    descriptor: &VerifiedDescriptor,
) -> Result<BlobDescriptor, VerifiedImageRecordError> {
    Ok(BlobDescriptor::new(
        sha256_digest(&descriptor.digest, "descriptor.digest")?,
        descriptor.size,
    ))
}

fn record_graph_digests(
    record: &VerifiedImageRecord,
) -> Result<BTreeSet<Sha256Digest>, VerifiedImageRecordError> {
    record
        .index_descriptor
        .iter()
        .map(|descriptor| sha256_digest(&descriptor.digest, "index_descriptor.digest"))
        .chain(std::iter::once(sha256_digest(
            &record.manifest_descriptor.digest,
            "manifest_descriptor.digest",
        )))
        .chain(std::iter::once(sha256_digest(
            &record.config_descriptor.digest,
            "config_descriptor.digest",
        )))
        .chain(
            record
                .layers
                .iter()
                .map(|layer| sha256_digest(&layer.descriptor.digest, "layers.descriptor.digest")),
        )
        .collect()
}

fn graph_pin_name(
    digest: &Digest,
    platform: GuestPlatform,
) -> Result<String, VerifiedImageRecordError> {
    Ok(format!(
        "image-{}-{}",
        sha256_digest(digest, "resolved_digest")?.encoded(),
        platform_storage_key(platform)
    ))
}

fn record_pin_name(
    digest: &Digest,
    platform: GuestPlatform,
) -> Result<String, VerifiedImageRecordError> {
    Ok(format!(
        "record-{}-{}",
        sha256_digest(digest, "resolved_digest")?.encoded(),
        platform_storage_key(platform)
    ))
}

fn verified_guest_platform(
    platform: &VerifiedPlatform,
) -> Result<GuestPlatform, VerifiedImageRecordError> {
    if platform.os != "linux" {
        return Err(invalid_field("platform.os"));
    }
    match (platform.architecture.as_str(), platform.variant.as_deref()) {
        ("arm64", None | Some("v8")) => Ok(GuestPlatform::LinuxArm64V8),
        ("arm64", Some(_)) | ("amd64", Some(_)) => Err(invalid_field("platform.variant")),
        ("amd64", None) => Ok(GuestPlatform::LinuxAmd64),
        _ => Err(invalid_field("platform.architecture")),
    }
}

const fn platform_storage_key(platform: GuestPlatform) -> &'static str {
    match platform {
        GuestPlatform::LinuxArm64V8 => "linux-arm64-v8",
        GuestPlatform::LinuxAmd64 => "linux-amd64",
    }
}

fn sha256_digest(
    digest: &Digest,
    field: &'static str,
) -> Result<Sha256Digest, VerifiedImageRecordError> {
    if digest.algorithm() != Sha256Digest::ALGORITHM {
        return Err(invalid_field(field));
    }
    digest.to_string().parse().map_err(|_| invalid_field(field))
}

fn record_json_limits() -> JsonLimits {
    JsonLimits {
        max_depth: 16,
        max_total_values: 8_192,
        max_array_items: MAX_PROCESS_ITEMS.max(DEFAULT_MAX_LAYERS),
        max_object_members: 32,
        max_string_bytes: MAX_PROCESS_STRING_BYTES,
        max_total_string_bytes: MAX_RECORD_TOTAL_STRING_BYTES,
    }
}

fn field_limit(field: &'static str) -> VerifiedImageRecordError {
    VerifiedImageRecordError::FieldLimit { field }
}

fn invalid_field(field: &'static str) -> VerifiedImageRecordError {
    VerifiedImageRecordError::InvalidField { field }
}

fn graph_mismatch(component: &'static str) -> VerifiedImageRecordError {
    VerifiedImageRecordError::GraphMismatch { component }
}
