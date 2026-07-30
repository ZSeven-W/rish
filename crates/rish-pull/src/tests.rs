use std::collections::{BTreeMap, VecDeque};
use std::io::Cursor;
use std::sync::Mutex;

use rish_content::{ContentStore, GcOptions, Sha256Digest, StoreConfig, StoreError};
use rish_oci::{ImageConfig, ImageConfiguration, RootFilesystem};
use rish_registry::{
    Descriptor, Digest, DigestValidationError, HeaderMap, ImageIndex, ImageManifest,
    ImageReference, MediaType, Platform, RegistryRequest, RegistryResponse, RegistryStreamResponse,
    RegistryTransport, ResponseValidationError, TransportError,
};
use tempfile::TempDir;

use crate::{LimitKind, PullError, PullPolicy, Puller};

#[derive(Clone)]
struct ExpectedExchange {
    path: String,
    response: RegistryResponse,
}

struct MockTransport {
    exchanges: Mutex<VecDeque<ExpectedExchange>>,
    requests: Mutex<Vec<(String, u64)>>,
}

impl MockTransport {
    fn new(exchanges: Vec<ExpectedExchange>) -> Self {
        Self {
            exchanges: Mutex::new(exchanges.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requested_paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn requested_limits(&self) -> Vec<u64> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, maximum)| *maximum)
            .collect()
    }

    fn assert_finished(&self) {
        assert!(
            self.exchanges.lock().unwrap().is_empty(),
            "mock still has unconsumed responses"
        );
    }
}

impl RegistryTransport for MockTransport {
    type Body = Cursor<Vec<u8>>;

    fn execute(
        &self,
        request: &RegistryRequest,
    ) -> Result<RegistryStreamResponse<Self::Body>, TransportError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.path_and_query.clone(), request.max_response_bytes));
        let exchange = self
            .exchanges
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| TransportError::new("unexpected registry request", false))?;
        if exchange.path != request.path_and_query {
            return Err(TransportError::new(
                format!(
                    "expected request path {}, got {}",
                    exchange.path, request.path_and_query
                ),
                false,
            ));
        }
        let response_size = u64::try_from(exchange.response.body.len()).unwrap_or(u64::MAX);
        if response_size > request.max_response_bytes {
            return Err(TransportError::new(
                format!(
                    "response body exceeded {} byte request limit",
                    request.max_response_bytes
                ),
                false,
            ));
        }
        Ok(exchange.response.into_stream())
    }
}

struct ManifestParts {
    body: Vec<u8>,
    descriptor: Descriptor,
    config_body: Vec<u8>,
    config: Descriptor,
    layer_bodies: Vec<Vec<u8>>,
    layers: Vec<Descriptor>,
}

struct Fixture {
    reference: ImageReference,
    root: Descriptor,
    manifest: Descriptor,
    config: Descriptor,
    layers: Vec<Descriptor>,
    exchanges: Vec<ExpectedExchange>,
}

fn manifest_parts(
    layer_media_types: &[MediaType],
    diff_id_count: usize,
    sha512_first_layer: bool,
) -> ManifestParts {
    let layer_bodies = layer_media_types
        .iter()
        .enumerate()
        .map(|(index, _)| format!("compressed-layer-{index}").into_bytes())
        .collect::<Vec<_>>();
    let layers = layer_media_types
        .iter()
        .zip(&layer_bodies)
        .enumerate()
        .map(|(index, (media_type, body))| {
            let digest = if index == 0 && sha512_first_layer {
                Digest::sha512(body)
            } else {
                Digest::sha256(body)
            };
            descriptor(media_type.clone(), digest, body.len(), None)
        })
        .collect::<Vec<_>>();

    let diff_ids = (0..diff_id_count)
        .map(|index| Digest::sha256(format!("uncompressed-layer-{index}").as_bytes()).to_string())
        .collect();
    let image_configuration = ImageConfiguration {
        architecture: "arm64".to_owned(),
        os: "linux".to_owned(),
        config: ImageConfig {
            env: vec!["PATH=/usr/bin:/bin".to_owned()],
            cmd: vec!["/bin/sh".to_owned()],
            working_dir: "/".to_owned(),
            ..ImageConfig::default()
        },
        rootfs: RootFilesystem {
            kind: "layers".to_owned(),
            diff_ids,
        },
    };
    let config_body = serde_json::to_vec(&image_configuration).unwrap();
    let config = descriptor(
        MediaType::OciImageConfig,
        Digest::sha256(&config_body),
        config_body.len(),
        None,
    );
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MediaType::OciImageManifest),
        config: config.clone(),
        layers: layers.clone(),
        artifact_type: None,
        subject: None,
        annotations: BTreeMap::new(),
    };
    let body = serde_json::to_vec(&manifest).unwrap();
    let manifest_descriptor = descriptor(
        MediaType::OciImageManifest,
        Digest::sha256(&body),
        body.len(),
        None,
    );

    ManifestParts {
        body,
        descriptor: manifest_descriptor,
        config_body,
        config,
        layer_bodies,
        layers,
    }
}

fn direct_fixture_with(
    layer_media_types: &[MediaType],
    diff_id_count: usize,
    sha512_first_layer: bool,
) -> Fixture {
    let parts = manifest_parts(layer_media_types, diff_id_count, sha512_first_layer);
    let reference: ImageReference =
        format!("registry.test/team/runtime@{}", parts.descriptor.digest)
            .parse()
            .unwrap();
    let mut exchanges = vec![ExpectedExchange {
        path: reference.manifest_path(),
        response: response(&parts.descriptor, parts.body),
    }];
    exchanges.push(ExpectedExchange {
        path: reference.blob_path(&parts.config.digest),
        response: response(&parts.config, parts.config_body),
    });
    exchanges.extend(
        parts
            .layers
            .iter()
            .zip(parts.layer_bodies)
            .map(|(descriptor, body)| ExpectedExchange {
                path: reference.blob_path(&descriptor.digest),
                response: response(descriptor, body),
            }),
    );

    Fixture {
        reference,
        root: parts.descriptor.clone(),
        manifest: parts.descriptor,
        config: parts.config,
        layers: parts.layers,
        exchanges,
    }
}

fn direct_fixture() -> Fixture {
    direct_fixture_with(
        &[MediaType::OciImageLayerGzip, MediaType::OciImageLayer],
        2,
        false,
    )
}

fn tagged_manifest_fixture() -> Fixture {
    let mut fixture = direct_fixture();
    let reference: ImageReference = "registry.test/team/runtime:edge".parse().unwrap();
    fixture.exchanges[0].path = reference.manifest_path();
    fixture.reference = reference;
    fixture
}

fn index_fixture() -> Fixture {
    let parts = manifest_parts(
        &[MediaType::OciImageLayerGzip, MediaType::OciImageLayer],
        2,
        false,
    );
    let amd64 = descriptor(
        MediaType::OciImageManifest,
        Digest::sha256(b"not-selected"),
        12,
        Some(Platform {
            architecture: "amd64".to_owned(),
            os: "linux".to_owned(),
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: Vec::new(),
        }),
    );
    let selected = Descriptor {
        platform: Some(Platform {
            architecture: "arm64".to_owned(),
            os: "linux".to_owned(),
            variant: Some("v8".to_owned()),
            os_version: None,
            os_features: Vec::new(),
            features: Vec::new(),
        }),
        ..parts.descriptor.clone()
    };
    let index = ImageIndex {
        schema_version: 2,
        media_type: Some(MediaType::OciImageIndex),
        manifests: vec![amd64, selected.clone()],
        artifact_type: None,
        subject: None,
        annotations: BTreeMap::new(),
    };
    let index_body = serde_json::to_vec(&index).unwrap();
    let root = descriptor(
        MediaType::OciImageIndex,
        Digest::sha256(&index_body),
        index_body.len(),
        None,
    );
    let reference: ImageReference = "registry.test/team/runtime:v1".parse().unwrap();
    let mut exchanges = vec![
        ExpectedExchange {
            path: reference.manifest_path(),
            response: response(&root, index_body),
        },
        ExpectedExchange {
            path: format!(
                "/v2/{}/manifests/{}",
                reference.repository(),
                selected.digest
            ),
            response: response(&selected, parts.body),
        },
        ExpectedExchange {
            path: reference.blob_path(&parts.config.digest),
            response: response(&parts.config, parts.config_body),
        },
    ];
    exchanges.extend(
        parts
            .layers
            .iter()
            .zip(parts.layer_bodies)
            .map(|(descriptor, body)| ExpectedExchange {
                path: reference.blob_path(&descriptor.digest),
                response: response(descriptor, body),
            }),
    );

    Fixture {
        reference,
        root,
        manifest: selected,
        config: parts.config,
        layers: parts.layers,
        exchanges,
    }
}

fn descriptor(
    media_type: MediaType,
    digest: Digest,
    size: usize,
    platform: Option<Platform>,
) -> Descriptor {
    Descriptor {
        media_type,
        digest,
        size: u64::try_from(size).unwrap(),
        urls: Vec::new(),
        annotations: BTreeMap::new(),
        data: None,
        platform,
        artifact_type: None,
    }
}

fn response(descriptor: &Descriptor, body: Vec<u8>) -> RegistryResponse {
    let mut headers = HeaderMap::default();
    headers
        .insert("content-type", descriptor.media_type.to_string())
        .unwrap();
    headers
        .insert("content-length", body.len().to_string())
        .unwrap();
    headers
        .insert("docker-content-digest", descriptor.digest.to_string())
        .unwrap();
    RegistryResponse {
        status: 200,
        headers,
        body,
    }
}

fn content_store() -> (TempDir, ContentStore) {
    let temporary = TempDir::new().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();
    (temporary, store)
}

fn cas_digest(digest: &Digest) -> Sha256Digest {
    digest.to_string().parse().unwrap()
}

#[test]
fn pulls_index_manifest_config_and_layers_into_one_lease() {
    let fixture = index_fixture();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();

    assert_eq!(image.resolved_digest(), &fixture.root.digest);
    assert_eq!(
        image.index.as_ref().unwrap().descriptor.digest,
        fixture.root.digest
    );
    assert_eq!(image.manifest.descriptor.digest, fixture.manifest.digest);
    assert_eq!(image.config.descriptor.digest, fixture.config.digest);
    assert_eq!(image.layers.len(), 2);
    assert_eq!(image.image_configuration.os, "linux");
    assert_eq!(image.image_configuration.architecture, "arm64");
    assert_eq!(image.layer_diff_ids().len(), image.layers.len());
    assert!(
        image
            .layer_diff_ids()
            .iter()
            .all(|digest| digest.algorithm() == "sha256")
    );
    let debug = format!("{image:?}");
    assert!(!debug.contains("/usr/bin:/bin"));
    assert!(!debug.contains("PATH="));
    assert_eq!(
        transport.requested_limits(),
        vec![
            crate::DEFAULT_MAX_MANIFEST_BYTES,
            fixture.manifest.size,
            fixture.config.size,
            fixture.layers[0].size,
            fixture.layers[1].size,
        ]
    );
    transport.assert_finished();

    let live_gc = store.garbage_collect(GcOptions { dry_run: false }).unwrap();
    assert_eq!(live_gc.kept_blobs, 5);
    assert_eq!(live_gc.removed_blobs, 0);

    drop(image);
    let released_gc = store.garbage_collect(GcOptions { dry_run: false }).unwrap();
    assert_eq!(released_gc.removed_blobs, 5);
}

#[test]
fn pulls_a_digest_addressed_top_level_manifest() {
    let fixture = direct_fixture();
    let expected_path = fixture.reference.manifest_path();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();

    assert!(image.index.is_none());
    assert_eq!(image.resolved_digest(), &fixture.manifest.digest);
    assert_eq!(transport.requested_paths()[0], expected_path);
    transport.assert_finished();
}

#[test]
fn resolves_a_tag_directly_to_an_immutable_manifest_digest() {
    let fixture = tagged_manifest_fixture();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();

    assert!(image.index.is_none());
    assert_eq!(image.resolved_digest(), &fixture.root.digest);
    transport.assert_finished();
}

#[test]
fn rejects_top_level_digest_mismatch_before_cas_admission() {
    let mut fixture = direct_fixture();
    fixture.exchanges[0].response.body[0] ^= 1;
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::DigestValidation(
            DigestValidationError::Mismatch { .. }
        ))
    ));
    assert!(!store.contains(cas_digest(&fixture.root.digest)).unwrap());
}

#[test]
fn rejects_layer_digest_mismatch_without_storing_corrupt_bytes() {
    let mut fixture = direct_fixture();
    let last = fixture.exchanges.last_mut().unwrap();
    last.response.body[0] ^= 1;
    let corrupt_digest = fixture.layers.last().unwrap().digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Store(StoreError::DigestMismatch { .. })
    ));
    assert!(!store.contains(cas_digest(&corrupt_digest)).unwrap());
}

#[test]
fn layer_count_limit_fails_before_any_blob_request_or_cas_write() {
    let fixture = direct_fixture();
    let root_digest = fixture.root.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();
    let policy = PullPolicy {
        max_layers: 1,
        ..PullPolicy::default()
    };

    let error = Puller::new(&transport, &store)
        .with_policy(policy)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::LimitExceeded {
            kind: LimitKind::LayerCount,
            limit: 1,
            actual: 2
        }
    ));
    assert_eq!(transport.requested_paths().len(), 1);
    assert!(!store.contains(cas_digest(&root_digest)).unwrap());
}

#[test]
fn config_size_limit_is_checked_before_the_config_request() {
    let fixture = direct_fixture();
    let limit = fixture.config.size - 1;
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();
    let policy = PullPolicy {
        max_config_bytes: limit,
        ..PullPolicy::default()
    };

    let error = Puller::new(&transport, &store)
        .with_policy(policy)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::LimitExceeded {
            kind: LimitKind::ConfigBytes,
            limit: observed_limit,
            actual,
        } if observed_limit == limit && actual == fixture.config.size
    ));
    assert_eq!(transport.requested_paths().len(), 1);
}

#[test]
fn individual_layer_size_limit_is_checked_before_blob_requests() {
    let fixture = direct_fixture();
    let limit = fixture.layers[0].size - 1;
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();
    let policy = PullPolicy {
        max_layer_bytes: limit,
        ..PullPolicy::default()
    };

    let error = Puller::new(&transport, &store)
        .with_policy(policy)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::LimitExceeded {
            kind: LimitKind::LayerBytes,
            limit: observed_limit,
            actual,
        } if observed_limit == limit && actual == fixture.layers[0].size
    ));
    assert_eq!(transport.requested_paths().len(), 1);
}

#[test]
fn aggregate_limit_is_preflighted_from_verified_descriptors() {
    let fixture = direct_fixture();
    let allowed = fixture
        .root
        .size
        .checked_add(fixture.config.size)
        .unwrap()
        .checked_add(fixture.layers[0].size)
        .unwrap();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();
    let policy = PullPolicy {
        max_total_bytes: allowed,
        ..PullPolicy::default()
    };

    let error = Puller::new(&transport, &store)
        .with_policy(policy)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::LimitExceeded {
            kind: LimitKind::TotalBytes,
            ..
        }
    ));
    assert_eq!(transport.requested_paths().len(), 1);
}

#[test]
fn manifest_size_limit_blocks_parsing_and_cas_admission() {
    let fixture = direct_fixture();
    let limit = fixture.root.size - 1;
    let root_digest = fixture.root.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();
    let policy = PullPolicy {
        max_manifest_bytes: limit,
        ..PullPolicy::default()
    };

    let error = Puller::new(&transport, &store)
        .with_policy(policy)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(error, PullError::Transport(_)));
    assert_eq!(transport.requested_limits(), vec![limit]);
    assert!(!store.contains(cas_digest(&root_digest)).unwrap());
}

#[test]
fn rejects_sha512_descriptors_before_blob_fetch() {
    let fixture = direct_fixture_with(&[MediaType::OciImageLayerGzip], 1, true);
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::UnsupportedDigest {
            algorithm,
            ..
        } if algorithm == "sha512"
    ));
    assert_eq!(transport.requested_paths().len(), 1);
}

#[test]
fn rejects_sha512_top_level_references_before_transport() {
    let digest = Digest::sha512(b"manifest");
    let reference: ImageReference = format!("registry.test/team/runtime@{digest}")
        .parse()
        .unwrap();
    let transport = MockTransport::new(Vec::new());
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::UnsupportedDigest {
            algorithm,
            ..
        } if algorithm == "sha512"
    ));
    assert!(transport.requested_paths().is_empty());
}

#[test]
fn rejects_zstd_layers_until_materialization_support_exists() {
    let fixture = direct_fixture_with(&[MediaType::OciImageLayerZstd], 1, false);
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::UnsupportedMediaType {
            media_type: MediaType::OciImageLayerZstd,
            ..
        }
    ));
    assert_eq!(transport.requested_paths().len(), 1);
}

#[test]
fn validates_diff_id_count_before_config_enters_cas() {
    let fixture = direct_fixture_with(&[MediaType::OciImageLayerGzip], 0, false);
    let config_digest = fixture.config.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::LayerDiffIdCountMismatch {
            diff_ids: 0,
            layers: 1
        }
    ));
    assert_eq!(transport.requested_paths().len(), 2);
    assert!(!store.contains(cas_digest(&config_digest)).unwrap());
}

#[test]
fn response_media_type_mismatch_is_rejected_before_layer_admission() {
    let mut fixture = direct_fixture_with(&[MediaType::OciImageLayerGzip], 1, false);
    let layer_digest = fixture.layers[0].digest.clone();
    fixture.exchanges[2]
        .response
        .headers
        .insert("content-type", MediaType::OCI_IMAGE_CONFIG)
        .unwrap();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::ContentTypeMismatch { .. })
    ));
    assert!(!store.contains(cas_digest(&layer_digest)).unwrap());
}

#[test]
fn descriptor_size_mismatch_is_rejected_before_config_admission() {
    let mut fixture = direct_fixture_with(&[MediaType::OciImageLayerGzip], 1, false);
    let config_digest = fixture.config.digest.clone();
    fixture.exchanges[1].response.body.pop();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::DescriptorSizeMismatch { .. })
    ));
    assert!(!store.contains(cas_digest(&config_digest)).unwrap());
}

#[test]
fn non_success_status_is_rejected_before_response_metadata_is_trusted() {
    let mut fixture = tagged_manifest_fixture();
    fixture.exchanges[0].response.status = 404;
    let root_digest = fixture.root.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::UnexpectedStatus(404))
    ));
    assert!(!store.contains(cas_digest(&root_digest)).unwrap());
}
