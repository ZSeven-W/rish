use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::sync::Mutex;

use rish_content::{ContentStore, StoreConfig};
use rish_registry::{
    Descriptor, Digest, ImageManifest, ImageReference, MediaType, RegistryRequest,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::*;

mod multiarch;

#[derive(Deserialize)]
struct DecodedFetchRequest {
    protocol_version: u32,
    request: RegistryRequest,
}

struct MockExchange {
    path: String,
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

struct MockHost {
    exchanges: Mutex<VecDeque<MockExchange>>,
    requested_paths: Mutex<Vec<String>>,
}

unsafe extern "C" fn mock_fetch(
    context: *mut c_void,
    request: *const u8,
    request_len: usize,
    body_fd: i32,
    metadata: *mut u8,
    metadata_capacity: usize,
    metadata_len: *mut usize,
) -> i32 {
    // SAFETY: The transport supplies valid pointers and the test keeps the
    // pointed-to host alive for the complete pull.
    let host = unsafe { &*(context.cast::<MockHost>()) };
    let request = unsafe { std::slice::from_raw_parts(request, request_len) };
    let decoded = match serde_json::from_slice::<DecodedFetchRequest>(request) {
        Ok(decoded) if decoded.protocol_version == PROTOCOL_VERSION => decoded,
        Ok(_) => {
            return unsafe {
                write_metadata(
                    metadata,
                    metadata_capacity,
                    metadata_len,
                    &failure_metadata("wrong request protocol"),
                )
            };
        }
        Err(error) => {
            return unsafe {
                write_metadata(
                    metadata,
                    metadata_capacity,
                    metadata_len,
                    &failure_metadata(&format!("invalid request: {error}")),
                )
            };
        }
    };
    let exchange = match host.exchanges.lock() {
        Ok(mut exchanges) => exchanges.pop_front(),
        Err(_) => None,
    };
    let Some(exchange) = exchange else {
        return unsafe {
            write_metadata(
                metadata,
                metadata_capacity,
                metadata_len,
                &failure_metadata("unexpected request"),
            )
        };
    };
    if exchange.path != decoded.request.path_and_query {
        return unsafe {
            write_metadata(
                metadata,
                metadata_capacity,
                metadata_len,
                &failure_metadata(&format!(
                    "expected {}, received {}",
                    exchange.path, decoded.request.path_and_query
                )),
            )
        };
    }
    if let Ok(mut paths) = host.requested_paths.lock() {
        paths.push(decoded.request.path_and_query);
    }

    // SAFETY: The fd is valid and borrowed. ManuallyDrop prevents this test
    // adapter from closing the descriptor owned by CallbackTransport.
    let mut output = ManuallyDrop::new(unsafe { File::from_raw_fd(body_fd) });
    for chunk in exchange.body.chunks(3) {
        if output.write_all(chunk).is_err() {
            return unsafe {
                write_metadata(
                    metadata,
                    metadata_capacity,
                    metadata_len,
                    &failure_metadata("body write failed"),
                )
            };
        }
    }
    unsafe {
        write_metadata(
            metadata,
            metadata_capacity,
            metadata_len,
            &json!({
                "protocol_version": PROTOCOL_VERSION,
                "ok": true,
                "status": exchange.status,
                "headers": exchange.headers,
                "error": null,
                "retryable": false
            }),
        )
    }
}

unsafe extern "C" fn metadata_overflow_fetch(
    _context: *mut c_void,
    _request: *const u8,
    _request_len: usize,
    _body_fd: i32,
    _metadata: *mut u8,
    metadata_capacity: usize,
    metadata_len: *mut usize,
) -> i32 {
    // SAFETY: The transport passes a valid out pointer.
    unsafe {
        *metadata_len = metadata_capacity.saturating_add(1);
    }
    0
}

unsafe extern "C" fn error_metadata_fetch(
    _context: *mut c_void,
    _request: *const u8,
    _request_len: usize,
    _body_fd: i32,
    metadata: *mut u8,
    metadata_capacity: usize,
    metadata_len: *mut usize,
) -> i32 {
    unsafe {
        write_metadata(
            metadata,
            metadata_capacity,
            metadata_len,
            &failure_metadata("network unavailable"),
        )
    }
}

unsafe extern "C" fn invalid_metadata_fetch(
    _context: *mut c_void,
    _request: *const u8,
    _request_len: usize,
    _body_fd: i32,
    metadata: *mut u8,
    metadata_capacity: usize,
    metadata_len: *mut usize,
) -> i32 {
    if metadata_capacity == 0 {
        return -1;
    }
    // SAFETY: The transport allocated at least one metadata byte.
    unsafe {
        *metadata = 0xff;
        *metadata_len = 1;
    }
    0
}

unsafe fn write_metadata(
    destination: *mut u8,
    capacity: usize,
    output_len: *mut usize,
    value: &Value,
) -> i32 {
    let encoded = match serde_json::to_vec(value) {
        Ok(encoded) => encoded,
        Err(_) => return -1,
    };
    // SAFETY: The transport always supplies a valid length out pointer.
    unsafe {
        *output_len = encoded.len();
    }
    if encoded.len() > capacity {
        return -2;
    }
    // SAFETY: `destination` names `capacity` writable bytes and the regions
    // cannot overlap with serde's owned output.
    unsafe {
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), destination, encoded.len());
    }
    0
}

fn failure_metadata(message: &str) -> Value {
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "ok": false,
        "status": null,
        "headers": {},
        "error": message,
        "retryable": true
    })
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

fn response_headers(descriptor: &Descriptor) -> HeaderMap {
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

fn pull_fixture() -> (MockHost, Descriptor, Descriptor, Descriptor) {
    let layer_body = b"small-compressed-layer".to_vec();
    let layer = descriptor(MediaType::OciImageLayerGzip, &layer_body);
    let config_body = serde_json::to_vec(&json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {"Cmd": ["/bin/sh"]},
        "rootfs": {
            "type": "layers",
            "diff_ids": [Digest::sha256(b"small-uncompressed-layer").to_string()]
        }
    }))
    .unwrap();
    let config = descriptor(MediaType::OciImageConfig, &config_body);
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
    let manifest = descriptor(MediaType::OciImageManifest, &manifest_body);
    let exchanges = VecDeque::from([
        MockExchange {
            path: "/v2/team/demo/manifests/v1".to_owned(),
            status: 200,
            headers: response_headers(&manifest),
            body: manifest_body,
        },
        MockExchange {
            path: format!("/v2/team/demo/blobs/{}", config.digest),
            status: 200,
            headers: response_headers(&config),
            body: config_body,
        },
        MockExchange {
            path: format!("/v2/team/demo/blobs/{}", layer.digest),
            status: 200,
            headers: response_headers(&layer),
            body: layer_body,
        },
    ]);
    (
        MockHost {
            exchanges: Mutex::new(exchanges),
            requested_paths: Mutex::new(Vec::new()),
        },
        manifest,
        config,
        layer,
    )
}

fn pull_request(store_root: &Path) -> String {
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "reference": "registry.test/team/demo:v1",
        "store_root": store_root,
        "limits": {
            "max_layer_bytes": 1024,
            "max_total_bytes": 1024 * 1024
        }
    })
    .to_string()
}

#[test]
fn callback_pull_streams_to_cas_and_pins_the_verified_graph() {
    let temporary = tempfile::tempdir().unwrap();
    let store_root = temporary.path().join("content");
    let (host, manifest, config, layer) = pull_fixture();
    let response = unsafe {
        pull_image_json(
            &pull_request(&store_root),
            Some(mock_fetch),
            (&host as *const MockHost).cast_mut().cast(),
        )
    };
    let response: Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["ok"], true);
    assert_eq!(
        response["receipt"]["normalized_reference"],
        "registry.test/team/demo:v1"
    );
    assert_eq!(
        response["receipt"]["resolved_digest"],
        manifest.digest.to_string()
    );
    assert_eq!(
        response["receipt"]["manifest_digest"],
        manifest.digest.to_string()
    );
    assert_eq!(
        response["receipt"]["config_digest"],
        config.digest.to_string()
    );
    assert_eq!(
        response["receipt"]["layers"][0]["digest"],
        layer.digest.to_string()
    );
    assert_eq!(response["receipt"]["layers"][0]["size"], layer.size);
    assert_eq!(response["receipt"]["os"], "linux");
    assert_eq!(response["receipt"]["architecture"], "arm64");
    assert_eq!(response["receipt"]["content_store"], "app_private_cas");
    assert!(host.exchanges.lock().unwrap().is_empty());
    assert_eq!(host.requested_paths.lock().unwrap().len(), 3);

    let store = ContentStore::open(StoreConfig::new(&store_root)).unwrap();
    let pin = response["receipt"]["pin"].as_str().unwrap();
    let pinned = store
        .pins()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.name == pin)
        .collect::<Vec<_>>();
    assert_eq!(pinned.len(), 3);
    assert!(
        std::fs::read_dir(store.root().join("tmp"))
            .unwrap()
            .next()
            .is_none(),
        "transport or CAS temporary files survived a successful pull"
    );
}

#[test]
fn null_callback_fails_without_opening_the_store() {
    let temporary = tempfile::tempdir().unwrap();
    let store_root = temporary.path().join("not-created");
    let response =
        unsafe { pull_image_json(&pull_request(&store_root), None, std::ptr::null_mut()) };
    let response: Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("callback is null")
    );
    assert!(!store_root.exists());
}

#[test]
fn oversized_or_invalid_fetch_metadata_fails_closed() {
    for (callback, expected) in [
        (
            metadata_overflow_fetch as RishRegistryFetchCallback,
            "metadata exceeds bridge limit",
        ),
        (
            invalid_metadata_fetch as RishRegistryFetchCallback,
            "metadata is not UTF-8",
        ),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let response = unsafe {
            pull_image_json(
                &pull_request(&temporary.path().join("content")),
                Some(callback),
                std::ptr::null_mut(),
            )
        };
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["ok"], false);
        assert!(
            response["error"].as_str().unwrap().contains(expected),
            "{response}"
        );
    }
}

#[test]
fn host_error_metadata_is_preserved_as_transport_failure() {
    let temporary = tempfile::tempdir().unwrap();
    let response = unsafe {
        pull_image_json(
            &pull_request(&temporary.path().join("content")),
            Some(error_metadata_fetch),
            std::ptr::null_mut(),
        )
    };
    let response: Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("network unavailable")
    );
}

#[test]
fn transport_rejects_a_body_larger_than_the_request_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let temporary_directory = temporary.path().join("tmp");
    std::fs::create_dir(&temporary_directory).unwrap();
    let image: ImageReference = "registry.test/team/demo:v1".parse().unwrap();
    let body = b"12345".to_vec();
    let blob = descriptor(MediaType::OciImageLayer, &body);
    let host = MockHost {
        exchanges: Mutex::new(VecDeque::from([MockExchange {
            path: image.blob_path(&blob.digest),
            status: 200,
            headers: response_headers(&blob),
            body,
        }])),
        requested_paths: Mutex::new(Vec::new()),
    };
    let transport = CallbackTransport {
        callback: mock_fetch,
        context: (&host as *const MockHost).cast_mut().cast(),
        temporary_directory,
    };
    let request = RegistryRequest::blob(&image, &blob).with_response_body_limit(4);

    let error = match transport.execute(&request) {
        Ok(_) => panic!("oversized body was accepted"),
        Err(error) => error,
    };
    assert!(error.message.contains("exceeds 4 byte request limit"));
}
