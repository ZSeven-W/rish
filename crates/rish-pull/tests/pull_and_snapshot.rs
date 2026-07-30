use std::collections::{BTreeMap, VecDeque};
use std::io::{Cursor, Write as _};
use std::sync::Mutex;

use flate2::Compression;
use flate2::write::GzEncoder;
use rish_content::{ContentStore, StoreConfig};
use rish_pull::Puller;
use rish_registry::{
    Descriptor, Digest, HeaderMap, ImageManifest, MediaType, RegistryRequest, RegistryResponse,
    RegistryStreamResponse, RegistryTransport, TransportError,
};
use rish_snapshot::{SnapshotOptions, materialize_snapshot};
use tar::{Builder, Header};

struct Exchange {
    path: String,
    response: RegistryResponse,
}

struct MockTransport {
    exchanges: Mutex<VecDeque<Exchange>>,
}

impl RegistryTransport for MockTransport {
    type Body = Cursor<Vec<u8>>;

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
        if exchange.response.body.len() as u64 > request.max_response_bytes {
            return Err(TransportError::new(
                "response exceeded request limit",
                false,
            ));
        }
        Ok(exchange.response.into_stream())
    }
}

fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut builder = Builder::new(&mut bytes);
        for (path, contents) in entries {
            let mut header = Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(contents.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(contents))
                .unwrap();
        }
        builder.finish().unwrap();
    }
    bytes
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn descriptor(media_type: MediaType, body: &[u8]) -> Descriptor {
    Descriptor {
        media_type,
        digest: Digest::sha256(body),
        size: body.len() as u64,
        urls: Vec::new(),
        annotations: BTreeMap::new(),
        data: None,
        platform: None,
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

#[test]
fn verified_pull_materializes_an_atomic_multi_layer_rootfs() {
    let base_tar = tar(&[("etc/old", b"old"), ("etc/keep", b"keep")]);
    let upper_tar = tar(&[("etc/.wh.old", b""), ("etc/new", b"new")]);
    let base_blob = gzip(&base_tar);
    let upper_blob = gzip(&upper_tar);
    let base = descriptor(MediaType::OciImageLayerGzip, &base_blob);
    let upper = descriptor(MediaType::OciImageLayerGzip, &upper_blob);

    let config_body = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {
            "Entrypoint": ["/bin/demo"],
            "WorkingDir": "/"
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": [
                Digest::sha256(&base_tar).to_string(),
                Digest::sha256(&upper_tar).to_string()
            ]
        }
    }))
    .unwrap();
    let config = descriptor(MediaType::OciImageConfig, &config_body);
    let manifest_document = ImageManifest {
        schema_version: 2,
        media_type: Some(MediaType::OciImageManifest),
        config: config.clone(),
        layers: vec![base.clone(), upper.clone()],
        artifact_type: None,
        subject: None,
        annotations: BTreeMap::new(),
    };
    let manifest_body = serde_json::to_vec(&manifest_document).unwrap();
    let manifest = descriptor(MediaType::OciImageManifest, &manifest_body);

    let transport = MockTransport {
        exchanges: Mutex::new(VecDeque::from([
            Exchange {
                path: "/v2/team/demo/manifests/v1".to_owned(),
                response: response(&manifest, manifest_body),
            },
            Exchange {
                path: format!("/v2/team/demo/blobs/{}", config.digest),
                response: response(&config, config_body),
            },
            Exchange {
                path: format!("/v2/team/demo/blobs/{}", base.digest),
                response: response(&base, base_blob),
            },
            Exchange {
                path: format!("/v2/team/demo/blobs/{}", upper.digest),
                response: response(&upper, upper_blob),
            },
        ])),
    };
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();

    let image = Puller::new(&transport, &store)
        .pull_str("registry.example/team/demo:v1")
        .unwrap();
    let snapshot = materialize_snapshot(
        &store,
        &image.manifest_document.layers,
        &image.layer_diff_ids(),
        "demo-rootfs",
        &SnapshotOptions::default(),
    )
    .unwrap();

    assert_eq!(snapshot.layers.len(), 2);
    assert!(!snapshot.path.join("etc/old").exists());
    assert_eq!(
        std::fs::read(snapshot.path.join("etc/keep")).unwrap(),
        b"keep"
    );
    assert_eq!(
        std::fs::read(snapshot.path.join("etc/new")).unwrap(),
        b"new"
    );
    assert!(transport.exchanges.lock().unwrap().is_empty());
}
