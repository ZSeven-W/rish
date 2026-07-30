use std::collections::{BTreeMap, VecDeque};
use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rish_content::{ContentStore, StoreConfig, StoreError};
use rish_pull::{PullError, Puller};
use rish_registry::{
    Descriptor, Digest, HeaderMap, ImageManifest, MediaType, RegistryRequest,
    RegistryStreamResponse, RegistryTransport, TransportError,
};
use sha2::{Digest as _, Sha256};

const LARGE_LAYER_SIZE: u64 = 16 * 1024 * 1024;
const NETWORK_CHUNK_SIZE: usize = 4 * 1024;
const CAS_READ_BUFFER_SIZE: usize = 64 * 1024;

enum BodySource {
    Buffered(Cursor<Vec<u8>>),
    ErrorAfter {
        byte: u8,
        remaining_before_error: u64,
    },
    Panic,
    Repeated {
        byte: u8,
        remaining: u64,
        largest_requested_read: Arc<AtomicUsize>,
    },
}

impl Read for BodySource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Buffered(cursor) => cursor.read(buffer),
            Self::ErrorAfter {
                byte,
                remaining_before_error,
            } => {
                if *remaining_before_error == 0 {
                    return Err(std::io::Error::other("injected stream failure"));
                }
                let count = usize::try_from(
                    (*remaining_before_error)
                        .min(u64::try_from(buffer.len().min(NETWORK_CHUNK_SIZE)).unwrap()),
                )
                .unwrap();
                buffer[..count].fill(*byte);
                *remaining_before_error -= u64::try_from(count).unwrap();
                Ok(count)
            }
            Self::Panic => panic!("response metadata failure must happen before the first read"),
            Self::Repeated {
                byte,
                remaining,
                largest_requested_read,
            } => {
                largest_requested_read.fetch_max(buffer.len(), Ordering::Relaxed);
                let count = usize::try_from(
                    (*remaining).min(u64::try_from(buffer.len().min(NETWORK_CHUNK_SIZE)).unwrap()),
                )
                .unwrap();
                buffer[..count].fill(*byte);
                *remaining -= u64::try_from(count).unwrap();
                Ok(count)
            }
        }
    }
}

struct Exchange {
    path: String,
    status: u16,
    headers: HeaderMap,
    body: BodySource,
}

struct ChunkedTransport {
    exchanges: Mutex<VecDeque<Exchange>>,
}

impl RegistryTransport for ChunkedTransport {
    type Body = BodySource;

    fn execute(
        &self,
        request: &RegistryRequest,
    ) -> Result<RegistryStreamResponse<Self::Body>, TransportError> {
        let exchange = self
            .exchanges
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| TransportError::new("unexpected registry request", false))?;
        if request.path_and_query != exchange.path {
            return Err(TransportError::new(
                format!(
                    "expected request {}, received {}",
                    exchange.path, request.path_and_query
                ),
                false,
            ));
        }
        let declared = exchange
            .headers
            .get("content-length")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        if declared > request.max_response_bytes {
            return Err(TransportError::new(
                "declared response exceeds request limit",
                false,
            ));
        }
        Ok(RegistryStreamResponse {
            status: exchange.status,
            headers: exchange.headers,
            body: exchange.body,
        })
    }
}

fn repeated_sha256(byte: u8, size: u64) -> Digest {
    let mut hasher = Sha256::new();
    let chunk = [byte; NETWORK_CHUNK_SIZE];
    let mut remaining = size;
    while remaining > 0 {
        let count = usize::try_from(remaining.min(NETWORK_CHUNK_SIZE as u64)).unwrap();
        hasher.update(&chunk[..count]);
        remaining -= u64::try_from(count).unwrap();
    }
    Digest::new("sha256", format!("{:x}", hasher.finalize())).unwrap()
}

fn descriptor(media_type: MediaType, digest: Digest, size: u64) -> Descriptor {
    Descriptor {
        media_type,
        digest,
        size,
        urls: Vec::new(),
        annotations: BTreeMap::new(),
        data: None,
        platform: None,
        artifact_type: None,
    }
}

fn headers(descriptor: &Descriptor) -> HeaderMap {
    let mut headers = HeaderMap::default();
    headers
        .insert("content-type", descriptor.media_type.to_string())
        .unwrap();
    headers
        .insert("content-length", descriptor.size.to_string())
        .unwrap();
    headers
        .insert("docker-content-digest", descriptor.digest.to_string())
        .unwrap();
    headers
}

fn buffered_exchange(path: String, descriptor: &Descriptor, body: Vec<u8>) -> Exchange {
    Exchange {
        path,
        status: 200,
        headers: headers(descriptor),
        body: BodySource::Buffered(Cursor::new(body)),
    }
}

fn assert_layer_and_temporary_file_absent(store: &ContentStore, layer: &Descriptor) {
    assert!(
        !store
            .contains(layer.digest.to_string().parse().unwrap())
            .unwrap()
    );
    assert!(
        std::fs::read_dir(store.root().join("tmp"))
            .unwrap()
            .next()
            .is_none(),
        "failed stream left a temporary CAS object behind"
    );
}

fn fixture(actual_layer_size: u64) -> (ChunkedTransport, Descriptor, Arc<AtomicUsize>) {
    let layer_byte = 0x5a;
    let layer = descriptor(
        MediaType::OciImageLayer,
        repeated_sha256(layer_byte, LARGE_LAYER_SIZE),
        LARGE_LAYER_SIZE,
    );
    let config_body = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {
            "Cmd": ["/bin/sh"]
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": [Digest::sha256(b"uncompressed").to_string()]
        }
    }))
    .unwrap();
    let config = descriptor(
        MediaType::OciImageConfig,
        Digest::sha256(&config_body),
        config_body.len() as u64,
    );
    let manifest_body = serde_json::to_vec(&ImageManifest {
        schema_version: 2,
        media_type: Some(MediaType::OciImageManifest),
        config: config.clone(),
        layers: vec![layer.clone()],
        artifact_type: None,
        subject: None,
        annotations: BTreeMap::new(),
    })
    .unwrap();
    let manifest = descriptor(
        MediaType::OciImageManifest,
        Digest::sha256(&manifest_body),
        manifest_body.len() as u64,
    );
    let largest_requested_read = Arc::new(AtomicUsize::new(0));
    let transport = ChunkedTransport {
        exchanges: Mutex::new(VecDeque::from([
            buffered_exchange(
                "/v2/team/stream/manifests/v1".to_owned(),
                &manifest,
                manifest_body,
            ),
            buffered_exchange(
                format!("/v2/team/stream/blobs/{}", config.digest),
                &config,
                config_body,
            ),
            Exchange {
                path: format!("/v2/team/stream/blobs/{}", layer.digest),
                status: 200,
                headers: headers(&layer),
                body: BodySource::Repeated {
                    byte: layer_byte,
                    remaining: actual_layer_size,
                    largest_requested_read: Arc::clone(&largest_requested_read),
                },
            },
        ])),
    };
    (transport, layer, largest_requested_read)
}

#[test]
fn large_layer_streams_to_cas_without_a_whole_body_read() {
    let (transport, _layer, largest_requested_read) = fixture(LARGE_LAYER_SIZE);
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let image = Puller::new(&transport, &store)
        .pull_str("registry.example/team/stream:v1")
        .unwrap();

    assert_eq!(image.layers[0].content.size, LARGE_LAYER_SIZE);
    assert_eq!(
        std::fs::metadata(store.blob_path(image.layers[0].content.digest))
            .unwrap()
            .len(),
        LARGE_LAYER_SIZE
    );
    assert!(
        largest_requested_read.load(Ordering::Relaxed) <= CAS_READ_BUFFER_SIZE,
        "the puller attempted to buffer the layer instead of feeding CAS incrementally"
    );
    assert!(transport.exchanges.lock().unwrap().is_empty());
}

#[test]
fn layer_stream_larger_than_its_descriptor_is_rejected_by_cas() {
    let (transport, layer, _largest_requested_read) = fixture(LARGE_LAYER_SIZE + 1);
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let error = Puller::new(&transport, &store)
        .pull_str("registry.example/team/stream:v1")
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Store(StoreError::SizeMismatch {
            expected: LARGE_LAYER_SIZE,
            actual,
        }) if actual == LARGE_LAYER_SIZE + 1
    ));
    assert_layer_and_temporary_file_absent(&store, &layer);
}

#[test]
fn layer_stream_shorter_than_content_length_is_rejected_by_cas() {
    let (transport, layer, _largest_requested_read) = fixture(LARGE_LAYER_SIZE - 1);
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let error = Puller::new(&transport, &store)
        .pull_str("registry.example/team/stream:v1")
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Store(StoreError::SizeMismatch {
            expected: LARGE_LAYER_SIZE,
            actual,
        }) if actual == LARGE_LAYER_SIZE - 1
    ));
    assert_layer_and_temporary_file_absent(&store, &layer);
}

#[test]
fn layer_read_error_removes_temporary_file_and_skips_cas() {
    let (transport, layer, _largest_requested_read) = fixture(LARGE_LAYER_SIZE);
    {
        let mut exchanges = transport.exchanges.lock().unwrap();
        exchanges.back_mut().unwrap().body = BodySource::ErrorAfter {
            byte: 0x5a,
            remaining_before_error: (NETWORK_CHUNK_SIZE * 2) as u64,
        };
    }
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let error = Puller::new(&transport, &store)
        .pull_str("registry.example/team/stream:v1")
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Store(StoreError::Io(error))
            if error.kind() == std::io::ErrorKind::Other
    ));
    assert_layer_and_temporary_file_absent(&store, &layer);
}

#[test]
fn overlong_small_body_is_rejected_by_puller_not_transport() {
    let (transport, _layer, _largest_requested_read) = fixture(LARGE_LAYER_SIZE);
    let config_digest = {
        let mut exchanges = transport.exchanges.lock().unwrap();
        let config = exchanges.get_mut(1).unwrap();
        let BodySource::Buffered(body) = &mut config.body else {
            panic!("config fixture is buffered")
        };
        body.get_mut().push(0);
        config
            .headers
            .get("docker-content-digest")
            .unwrap()
            .parse()
            .unwrap()
    };
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let error = Puller::new(&transport, &store)
        .pull_str("registry.example/team/stream:v1")
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(rish_registry::ResponseValidationError::DescriptorSizeMismatch { .. })
    ));
    assert!(!store.contains(config_digest).unwrap());
}

#[test]
fn invalid_layer_headers_are_rejected_before_body_read() {
    let (transport, layer, _largest_requested_read) = fixture(LARGE_LAYER_SIZE);
    {
        let mut exchanges = transport.exchanges.lock().unwrap();
        let layer_exchange = exchanges.back_mut().unwrap();
        layer_exchange
            .headers
            .insert("content-type", MediaType::OCI_IMAGE_CONFIG)
            .unwrap();
        layer_exchange.body = BodySource::Panic;
    }
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let error = Puller::new(&transport, &store)
        .pull_str("registry.example/team/stream:v1")
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(rish_registry::ResponseValidationError::ContentTypeMismatch { .. })
    ));
    assert_layer_and_temporary_file_absent(&store, &layer);
}
