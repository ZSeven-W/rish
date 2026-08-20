use std::fmt;
use std::io::Read;

use rish_content::{BlobDescriptor, ContentStore, Lease, Sha256Digest};
use rish_oci::ImageConfiguration;
use rish_registry::{
    Descriptor, Digest, GuestPlatform, ImageManifest, ImageReference, ManifestDocument, MediaType,
    PlatformRequest, RegistryRequest, RegistryStreamResponse, RegistryTransport, ValidationPolicy,
    select_platform,
};

use crate::json_limits::validate_json_shape;
use crate::{
    BlobKind, DEFAULT_MAX_CONFIG_BYTES, DEFAULT_MAX_MANIFEST_BYTES, JsonLimits, LimitKind,
    PullError, PullPolicy,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PulledBlob {
    /// The registry descriptor whose size, media type, and digest were checked.
    pub descriptor: Descriptor,
    /// The corresponding immutable object in the local SHA-256 CAS.
    pub content: BlobDescriptor,
}

pub struct PulledImage {
    pub reference: ImageReference,
    /// Present when the reference resolved through an OCI index/manifest list.
    pub index: Option<PulledBlob>,
    pub manifest: PulledBlob,
    pub config: PulledBlob,
    pub layers: Vec<PulledBlob>,
    pub manifest_document: ImageManifest,
    pub image_configuration: ImageConfiguration,
    lease: Lease,
}

impl PulledImage {
    /// The immutable digest to which the original tag or digest resolved.
    #[must_use]
    pub fn resolved_digest(&self) -> &Digest {
        self.index
            .as_ref()
            .map_or(&self.manifest.descriptor.digest, |index| {
                &index.descriptor.digest
            })
    }

    /// Keeps every returned blob rooted against process-local garbage collection.
    #[must_use]
    pub fn lease(&self) -> &Lease {
        &self.lease
    }

    /// Uncompressed layer digests in manifest order.
    ///
    /// Configuration validation guarantees these are canonical SHA-256
    /// digests and that their count matches [`Self::layers`].
    #[must_use]
    pub fn layer_diff_ids(&self) -> Vec<Digest> {
        self.image_configuration
            .rootfs
            .diff_ids
            .iter()
            .map(|value| {
                value
                    .parse()
                    .expect("validated image config contains canonical SHA-256 diff IDs")
            })
            .collect()
    }

    /// Separates the lease from image metadata when a caller needs to retain
    /// only the garbage-collection root.
    #[must_use]
    pub fn into_lease(self) -> Lease {
        self.lease
    }
}

impl fmt::Debug for PulledImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PulledImage")
            .field("reference", &self.reference)
            .field("resolved_digest", &self.resolved_digest())
            .field("has_index", &self.index.is_some())
            .field("manifest", &self.manifest.content)
            .field("config", &self.config.content)
            .field("layer_count", &self.layers.len())
            .field("os", &self.image_configuration.os)
            .field("architecture", &self.image_configuration.architecture)
            .field("lease", &"<active>")
            .finish()
    }
}

pub struct Puller<'a, T: ?Sized> {
    transport: &'a T,
    store: &'a ContentStore,
    policy: PullPolicy,
    platform: PlatformRequest,
}

impl<'a, T> Puller<'a, T>
where
    T: RegistryTransport + ?Sized,
{
    /// Creates a puller targeting Linux ARM64, preferring the OCI `v8`
    /// variant and then a variantless fallback.
    #[must_use]
    pub fn new(transport: &'a T, store: &'a ContentStore) -> Self {
        Self {
            transport,
            store,
            policy: PullPolicy::default(),
            platform: PlatformRequest::linux_arm64(None),
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: PullPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Adds a more specific ARM64 variant ahead of `v8`.
    #[must_use]
    pub fn with_preferred_arm64_variant(mut self, variant: Option<&str>) -> Self {
        self.platform = PlatformRequest::linux_arm64(variant);
        self
    }

    /// Selects one of the explicitly supported guest image platforms.
    ///
    /// Index matching is exact: ARM64 requires `v8`, while AMD64 requires a
    /// variantless `linux/amd64` descriptor.
    #[must_use]
    pub fn with_guest_platform(mut self, platform: GuestPlatform) -> Self {
        self.platform = platform.selection_request();
        self
    }

    #[must_use]
    pub fn policy(&self) -> PullPolicy {
        self.policy
    }

    #[must_use]
    pub fn platform(&self) -> &PlatformRequest {
        &self.platform
    }

    pub fn pull_str(&self, reference: &str) -> Result<PulledImage, PullError> {
        self.pull(&reference.parse()?)
    }

    pub fn pull(&self, reference: &ImageReference) -> Result<PulledImage, PullError> {
        ensure_reference_digest(reference)?;
        let lease = self.store.create_lease()?;
        let mut budget = Budget::new(self.policy.max_total_bytes);

        let root_payload = self.fetch_root(reference, &mut budget)?;
        let root_document = parse_document(&root_payload, self.policy.json_limits)?;

        let (index_payload, manifest_payload, manifest_document) = match root_document {
            ManifestDocument::Manifest(manifest) => {
                validate_document_media_type(
                    BlobKind::Manifest,
                    root_payload.descriptor.media_type.clone(),
                    manifest.media_type.as_ref(),
                )?;
                (None, root_payload, *manifest)
            }
            ManifestDocument::Index(index) => {
                validate_document_media_type(
                    BlobKind::Index,
                    root_payload.descriptor.media_type.clone(),
                    index.media_type.as_ref(),
                )?;
                let selected = select_platform(&index, &self.platform)?.clone();
                validate_descriptor_kind(&selected, BlobKind::Manifest)?;
                enforce_limit(
                    LimitKind::ManifestBytes,
                    self.manifest_limit(),
                    selected.size,
                )?;
                budget.charge(selected.size)?;

                let selected_payload =
                    self.fetch_small_descriptor(reference, &selected, self.manifest_limit())?;
                let selected_document = parse_document(&selected_payload, self.policy.json_limits)?;
                let ManifestDocument::Manifest(manifest) = selected_document else {
                    return Err(PullError::UnexpectedDocument {
                        expected: BlobKind::Manifest,
                        actual: BlobKind::Index,
                    });
                };
                validate_document_media_type(
                    BlobKind::Manifest,
                    selected.media_type.clone(),
                    manifest.media_type.as_ref(),
                )?;
                (Some(root_payload), selected_payload, *manifest)
            }
        };

        self.validate_manifest_graph(&manifest_document, &mut budget)?;

        let index = index_payload
            .map(|payload| store_payload(&lease, payload))
            .transpose()?;
        let manifest = store_payload(&lease, manifest_payload)?;

        let config_payload =
            self.fetch_small_descriptor(reference, &manifest_document.config, self.config_limit())?;
        let image_configuration =
            parse_image_configuration(&config_payload, self.policy.json_limits)?;
        self.validate_image_configuration(&image_configuration, &manifest_document)?;
        let config = store_payload(&lease, config_payload)?;

        let layers = manifest_document
            .layers
            .iter()
            .map(|descriptor| self.fetch_layer(reference, descriptor, &lease))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(PulledImage {
            reference: reference.clone(),
            index,
            manifest,
            config,
            layers,
            manifest_document,
            image_configuration,
            lease,
        })
    }

    fn fetch_root(
        &self,
        reference: &ImageReference,
        budget: &mut Budget,
    ) -> Result<VerifiedPayload, PullError> {
        let request =
            RegistryRequest::manifest(reference).with_response_body_limit(self.manifest_limit());
        let response = self.transport.execute(&request)?;
        if response.status != 200 {
            return Err(
                rish_registry::ResponseValidationError::UnexpectedStatus(response.status).into(),
            );
        }

        let size = response
            .content_length()?
            .ok_or(rish_registry::ResponseValidationError::MissingContentLength)?;
        enforce_limit(LimitKind::ManifestBytes, self.manifest_limit(), size)?;

        let media_type = required_content_type(&response.headers)?;
        let kind = document_kind(&media_type)?;
        let (digest, header_policy) = if let Some(expected) = reference.digest() {
            (expected.clone(), ValidationPolicy::descriptor_headers())
        } else {
            let registry_digest = required_registry_digest(&response.headers)?;
            ensure_sha256(&registry_digest, kind)?;
            (registry_digest, ValidationPolicy::strict_headers())
        };
        let descriptor = synthetic_descriptor(media_type, digest, size);
        response.validate_descriptor_metadata(&descriptor, header_policy)?;
        budget.charge(size)?;
        read_small_verified(response, descriptor, self.manifest_limit(), kind)
    }

    fn fetch_small_descriptor(
        &self,
        reference: &ImageReference,
        descriptor: &Descriptor,
        maximum: u64,
    ) -> Result<VerifiedPayload, PullError> {
        let kind = infer_kind(&descriptor.media_type);
        enforce_limit(limit_kind(kind), maximum, descriptor.size)?;
        let request = if descriptor.media_type.is_manifest() {
            manifest_request(reference, descriptor)
        } else {
            RegistryRequest::blob(reference, descriptor)
        }
        .with_response_body_limit(descriptor.size.min(maximum));
        let response = self.transport.execute(&request)?;
        response
            .validate_descriptor_metadata(descriptor, ValidationPolicy::descriptor_headers())?;
        read_small_verified(response, descriptor.clone(), maximum, kind)
    }

    fn fetch_layer(
        &self,
        reference: &ImageReference,
        descriptor: &Descriptor,
        lease: &Lease,
    ) -> Result<PulledBlob, PullError> {
        let request = RegistryRequest::blob(reference, descriptor);
        let response = self.transport.execute(&request)?;
        response
            .validate_descriptor_metadata(descriptor, ValidationPolicy::descriptor_headers())?;
        let expected = cas_descriptor(descriptor)?;
        let stored = lease.ingest_verified(response.body, expected)?;
        Ok(PulledBlob {
            descriptor: descriptor.clone(),
            content: stored,
        })
    }

    fn validate_manifest_graph(
        &self,
        manifest: &ImageManifest,
        budget: &mut Budget,
    ) -> Result<(), PullError> {
        manifest.validate()?;
        validate_descriptor_kind(&manifest.config, BlobKind::Config)?;
        enforce_limit(
            LimitKind::ConfigBytes,
            self.config_limit(),
            manifest.config.size,
        )?;
        let layer_count = u64::try_from(manifest.layers.len()).unwrap_or(u64::MAX);
        let max_layers = u64::try_from(self.policy.max_layers).unwrap_or(u64::MAX);
        enforce_limit(LimitKind::LayerCount, max_layers, layer_count)?;

        budget.charge(manifest.config.size)?;
        for layer in &manifest.layers {
            validate_descriptor_kind(layer, BlobKind::Layer)?;
            enforce_limit(
                LimitKind::LayerBytes,
                self.policy.max_layer_bytes,
                layer.size,
            )?;
            budget.charge(layer.size)?;
        }
        Ok(())
    }

    const fn manifest_limit(&self) -> u64 {
        if self.policy.max_manifest_bytes < DEFAULT_MAX_MANIFEST_BYTES {
            self.policy.max_manifest_bytes
        } else {
            DEFAULT_MAX_MANIFEST_BYTES
        }
    }

    const fn config_limit(&self) -> u64 {
        if self.policy.max_config_bytes < DEFAULT_MAX_CONFIG_BYTES {
            self.policy.max_config_bytes
        } else {
            DEFAULT_MAX_CONFIG_BYTES
        }
    }

    fn validate_image_configuration(
        &self,
        configuration: &ImageConfiguration,
        manifest: &ImageManifest,
    ) -> Result<(), PullError> {
        if configuration.os != self.platform.os()
            || configuration.architecture != self.platform.architecture()
        {
            return Err(PullError::ImagePlatformMismatch {
                expected_os: self.platform.os().to_owned(),
                expected_architecture: self.platform.architecture().to_owned(),
                actual_os: configuration.os.clone(),
                actual_architecture: configuration.architecture.clone(),
            });
        }
        configuration.validate_linux_guest_metadata()?;
        if configuration.rootfs.diff_ids.len() != manifest.layers.len() {
            return Err(PullError::LayerDiffIdCountMismatch {
                diff_ids: configuration.rootfs.diff_ids.len(),
                layers: manifest.layers.len(),
            });
        }
        Ok(())
    }
}

struct VerifiedPayload {
    descriptor: Descriptor,
    body: Vec<u8>,
}

struct Budget {
    maximum: u64,
    consumed: u64,
}

impl Budget {
    const fn new(maximum: u64) -> Self {
        Self {
            maximum,
            consumed: 0,
        }
    }

    fn charge(&mut self, bytes: u64) -> Result<(), PullError> {
        let total = self.consumed.saturating_add(bytes);
        enforce_limit(LimitKind::TotalBytes, self.maximum, total)?;
        self.consumed = total;
        Ok(())
    }
}

fn parse_document(
    payload: &VerifiedPayload,
    limits: JsonLimits,
) -> Result<ManifestDocument, PullError> {
    let kind = document_kind(&payload.descriptor.media_type).unwrap_or(BlobKind::Manifest);
    validate_json_shape(&payload.body, limits).map_err(|error| PullError::JsonPreflight {
        kind,
        reason: error.to_string(),
    })?;
    let document = serde_json::from_slice::<ManifestDocument>(&payload.body)
        .map_err(|source| PullError::Json { kind, source })?;
    document.validate()?;

    match (&document, &payload.descriptor.media_type) {
        (ManifestDocument::Manifest(_), media_type) if media_type.is_manifest() => Ok(document),
        (ManifestDocument::Index(_), media_type) if media_type.is_index() => Ok(document),
        (ManifestDocument::Manifest(_), _) => Err(PullError::UnexpectedDocument {
            expected: BlobKind::Index,
            actual: BlobKind::Manifest,
        }),
        (ManifestDocument::Index(_), _) => Err(PullError::UnexpectedDocument {
            expected: BlobKind::Manifest,
            actual: BlobKind::Index,
        }),
    }
}

fn parse_image_configuration(
    payload: &VerifiedPayload,
    limits: JsonLimits,
) -> Result<ImageConfiguration, PullError> {
    validate_json_shape(&payload.body, limits).map_err(|error| PullError::JsonPreflight {
        kind: BlobKind::Config,
        reason: error.to_string(),
    })?;
    serde_json::from_slice(&payload.body).map_err(|source| PullError::Json {
        kind: BlobKind::Config,
        source,
    })
}

fn validate_document_media_type(
    kind: BlobKind,
    descriptor: MediaType,
    document: Option<&MediaType>,
) -> Result<(), PullError> {
    if let Some(document) = document {
        if *document != descriptor {
            return Err(PullError::DocumentMediaTypeMismatch {
                kind,
                expected: descriptor,
                actual: document.clone(),
            });
        }
    }
    Ok(())
}

fn validate_descriptor_kind(descriptor: &Descriptor, kind: BlobKind) -> Result<(), PullError> {
    descriptor.validate()?;
    ensure_sha256(&descriptor.digest, kind)?;
    let supported = match kind {
        BlobKind::Index => descriptor.media_type.is_index(),
        BlobKind::Manifest => descriptor.media_type.is_manifest(),
        BlobKind::Config => matches!(
            descriptor.media_type,
            MediaType::OciImageConfig | MediaType::DockerImageConfig
        ),
        BlobKind::Layer => matches!(
            descriptor.media_type,
            MediaType::OciImageLayer | MediaType::OciImageLayerGzip | MediaType::DockerLayerGzip
        ),
    };
    if supported {
        Ok(())
    } else {
        Err(PullError::UnsupportedMediaType {
            kind,
            media_type: descriptor.media_type.clone(),
        })
    }
}

fn ensure_reference_digest(reference: &ImageReference) -> Result<(), PullError> {
    if let Some(digest) = reference.digest() {
        ensure_sha256(digest, BlobKind::Manifest)?;
    }
    Ok(())
}

fn ensure_sha256(digest: &Digest, kind: BlobKind) -> Result<(), PullError> {
    if digest.algorithm() == Sha256Digest::ALGORITHM {
        Ok(())
    } else {
        Err(PullError::UnsupportedDigest {
            kind,
            algorithm: digest.algorithm().to_owned(),
        })
    }
}

fn store_payload(lease: &Lease, payload: VerifiedPayload) -> Result<PulledBlob, PullError> {
    let content = cas_descriptor(&payload.descriptor)?;
    let stored = lease.ingest_verified(payload.body.as_slice(), content)?;
    Ok(PulledBlob {
        descriptor: payload.descriptor,
        content: stored,
    })
}

/// Reads a bounded config/manifest after the caller has validated its response
/// metadata, then independently verifies the descriptor size and digest.
fn read_small_verified<B: Read>(
    response: RegistryStreamResponse<B>,
    descriptor: Descriptor,
    maximum: u64,
    kind: BlobKind,
) -> Result<VerifiedPayload, PullError> {
    enforce_limit(limit_kind(kind), maximum, descriptor.size)?;
    let capacity = usize::try_from(descriptor.size).map_err(|_| PullError::LimitExceeded {
        kind: limit_kind(kind),
        limit: maximum,
        actual: descriptor.size,
    })?;
    let RegistryStreamResponse { body, .. } = response;
    let mut body = body.take(descriptor.size);
    let mut buffered = Vec::with_capacity(capacity);
    body.read_to_end(&mut buffered)?;
    let mut body = body.into_inner();
    let mut trailing = [0_u8; 1];
    if body.read(&mut trailing)? != 0 {
        return Err(
            rish_registry::ResponseValidationError::DescriptorSizeMismatch {
                expected: descriptor.size,
                actual: descriptor.size.saturating_add(1),
            }
            .into(),
        );
    }

    let actual = u64::try_from(buffered.len())
        .map_err(|_| rish_registry::ResponseValidationError::BodyTooLarge)?;
    if actual != descriptor.size {
        return Err(
            rish_registry::ResponseValidationError::DescriptorSizeMismatch {
                expected: descriptor.size,
                actual,
            }
            .into(),
        );
    }
    descriptor
        .digest
        .verify(&buffered)
        .map_err(rish_registry::ResponseValidationError::from)?;
    Ok(VerifiedPayload {
        descriptor,
        body: buffered,
    })
}

const fn limit_kind(kind: BlobKind) -> LimitKind {
    match kind {
        BlobKind::Index | BlobKind::Manifest => LimitKind::ManifestBytes,
        BlobKind::Config => LimitKind::ConfigBytes,
        BlobKind::Layer => LimitKind::LayerBytes,
    }
}

fn cas_descriptor(descriptor: &Descriptor) -> Result<BlobDescriptor, PullError> {
    ensure_sha256(&descriptor.digest, infer_kind(&descriptor.media_type))?;
    let digest = descriptor
        .digest
        .to_string()
        .parse::<Sha256Digest>()
        .map_err(|_| PullError::UnsupportedDigest {
            kind: infer_kind(&descriptor.media_type),
            algorithm: descriptor.digest.algorithm().to_owned(),
        })?;
    Ok(BlobDescriptor::new(digest, descriptor.size))
}

fn infer_kind(media_type: &MediaType) -> BlobKind {
    if media_type.is_index() {
        BlobKind::Index
    } else if media_type.is_manifest() {
        BlobKind::Manifest
    } else if matches!(
        media_type,
        MediaType::OciImageConfig | MediaType::DockerImageConfig
    ) {
        BlobKind::Config
    } else {
        BlobKind::Layer
    }
}

fn required_content_type(headers: &rish_registry::HeaderMap) -> Result<MediaType, PullError> {
    let value = headers
        .get_single("content-type")?
        .ok_or(rish_registry::ResponseValidationError::MissingContentType)?;
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .parse()
        .map_err(Into::into)
}

fn required_registry_digest(headers: &rish_registry::HeaderMap) -> Result<Digest, PullError> {
    headers
        .get_single("docker-content-digest")?
        .ok_or(rish_registry::ResponseValidationError::MissingRegistryDigest)?
        .trim()
        .parse()
        .map_err(Into::into)
}

fn document_kind(media_type: &MediaType) -> Result<BlobKind, PullError> {
    if media_type.is_index() {
        Ok(BlobKind::Index)
    } else if media_type.is_manifest() {
        Ok(BlobKind::Manifest)
    } else {
        Err(PullError::UnsupportedMediaType {
            kind: BlobKind::Manifest,
            media_type: media_type.clone(),
        })
    }
}

fn synthetic_descriptor(media_type: MediaType, digest: Digest, size: u64) -> Descriptor {
    Descriptor {
        media_type,
        digest,
        size,
        urls: Vec::new(),
        annotations: std::collections::BTreeMap::new(),
        data: None,
        platform: None,
        artifact_type: None,
    }
}

fn manifest_request(reference: &ImageReference, descriptor: &Descriptor) -> RegistryRequest {
    let mut request = RegistryRequest::manifest(reference);
    request.path_and_query = format!(
        "/v2/{}/manifests/{}",
        reference.repository(),
        descriptor.digest
    );
    request.with_response_body_limit(descriptor.size)
}

fn enforce_limit(kind: LimitKind, limit: u64, actual: u64) -> Result<(), PullError> {
    if actual <= limit {
        Ok(())
    } else {
        Err(PullError::LimitExceeded {
            kind,
            limit,
            actual,
        })
    }
}
