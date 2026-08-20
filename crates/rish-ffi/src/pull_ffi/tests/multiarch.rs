use std::collections::BTreeSet;

use rish_pull::VerifiedImageRecordStore;
use rish_registry::{ImageIndex, Platform};

use super::*;

struct PlatformFixture {
    host: MockHost,
    index: Descriptor,
    selected: Descriptor,
}

fn platform_descriptor(
    architecture: &str,
    variant: Option<&str>,
    layer: &Descriptor,
) -> (Descriptor, Descriptor, Vec<u8>) {
    let config_body = serde_json::to_vec(&json!({
        "architecture": architecture,
        "os": "linux",
        "config": {"Cmd": ["/bin/sh"]},
        "rootfs": {
            "type": "layers",
            "diff_ids": [Digest::sha256(
                format!("uncompressed-{architecture}").as_bytes()
            ).to_string()]
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
    let manifest = Descriptor {
        platform: Some(Platform {
            architecture: architecture.to_owned(),
            os: "linux".to_owned(),
            variant: variant.map(str::to_owned),
            os_version: None,
            os_features: Vec::new(),
            features: Vec::new(),
        }),
        ..descriptor(MediaType::OciImageManifest, &manifest_body)
    };
    (manifest, config, manifest_body)
}

fn index_fixture(platform: GuestPlatform) -> PlatformFixture {
    let layer_body = b"shared-compressed-layer".to_vec();
    let layer = descriptor(MediaType::OciImageLayerGzip, &layer_body);
    let (arm64, arm64_config, arm64_body) = platform_descriptor("arm64", Some("v8"), &layer);
    let (amd64, amd64_config, amd64_body) = platform_descriptor("amd64", None, &layer);
    let index_body = serde_json::to_vec(&ImageIndex {
        schema_version: 2,
        media_type: Some(MediaType::OciImageIndex),
        manifests: vec![arm64.clone(), amd64.clone()],
        artifact_type: None,
        subject: None,
        annotations: BTreeMap::new(),
    })
    .unwrap();
    let index = descriptor(MediaType::OciImageIndex, &index_body);
    let (selected, config, manifest_body) = match platform {
        GuestPlatform::LinuxArm64V8 => (arm64, arm64_config, arm64_body),
        GuestPlatform::LinuxAmd64 => (amd64, amd64_config, amd64_body),
    };
    let config_body = serde_json::to_vec(&json!({
        "architecture": platform.architecture(),
        "os": "linux",
        "config": {"Cmd": ["/bin/sh"]},
        "rootfs": {
            "type": "layers",
            "diff_ids": [Digest::sha256(
                format!("uncompressed-{}", platform.architecture()).as_bytes()
            ).to_string()]
        }
    }))
    .unwrap();
    assert_eq!(config.digest, Digest::sha256(&config_body));
    let exchanges = VecDeque::from([
        MockExchange {
            path: "/v2/team/demo/manifests/v1".to_owned(),
            status: 200,
            headers: response_headers(&index),
            body: index_body,
        },
        MockExchange {
            path: format!("/v2/team/demo/manifests/{}", selected.digest),
            status: 200,
            headers: response_headers(&selected),
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
    PlatformFixture {
        host: MockHost {
            exchanges: Mutex::new(exchanges),
            requested_paths: Mutex::new(Vec::new()),
        },
        index,
        selected,
    }
}

fn request(store_root: &Path, platform: &str) -> String {
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "reference": "registry.test/team/demo:v1",
        "store_root": store_root,
        "platform": platform,
        "limits": {
            "max_layer_bytes": 1024,
            "max_total_bytes": 1024 * 1024
        }
    })
    .to_string()
}

#[test]
fn ffi_selects_and_persists_each_supported_guest_platform_exactly() {
    let temporary = tempfile::tempdir().unwrap();
    let store_root = temporary.path().join("content");
    let mut resolved_digest = None;

    for (platform, token, expected_variant) in [
        (GuestPlatform::LinuxArm64V8, "linux/arm64/v8", Some("v8")),
        (GuestPlatform::LinuxAmd64, "linux/amd64", None),
    ] {
        let fixture = index_fixture(platform);
        let response = unsafe {
            pull_image_json(
                &request(&store_root, token),
                Some(mock_fetch),
                (&fixture.host as *const MockHost).cast_mut().cast(),
            )
        };
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["receipt"]["architecture"], platform.architecture());
        assert_eq!(response["receipt"]["variant"].as_str(), expected_variant);
        assert_eq!(
            response["receipt"]["manifest_digest"],
            fixture.selected.digest.to_string()
        );
        assert_eq!(
            response["receipt"]["resolved_digest"],
            fixture.index.digest.to_string()
        );
        if let Some(previous) = &resolved_digest {
            assert_eq!(previous, &fixture.index.digest);
        } else {
            resolved_digest = Some(fixture.index.digest.clone());
        }
        assert!(fixture.host.exchanges.lock().unwrap().is_empty());
    }

    let resolved_digest = resolved_digest.unwrap();
    let store = ContentStore::open(StoreConfig::new(&store_root)).unwrap();
    let records = VerifiedImageRecordStore::open(&store).unwrap();
    for (platform, expected_variant) in [
        (GuestPlatform::LinuxArm64V8, Some("v8")),
        (GuestPlatform::LinuxAmd64, None),
    ] {
        let reopened = records
            .reopen_for_platform(&resolved_digest, platform)
            .unwrap();
        assert_eq!(
            reopened.record.platform.architecture,
            platform.architecture()
        );
        assert_eq!(
            reopened.record.platform.variant.as_deref(),
            expected_variant
        );
    }
    let graph_pins = store
        .pins()
        .unwrap()
        .into_iter()
        .filter(|pin| pin.name.starts_with("image-"))
        .map(|pin| pin.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(graph_pins.len(), 2);
}

#[test]
fn ffi_rejects_unknown_platforms_and_variants_before_network_or_store_io() {
    for platform in [
        "linux/riscv64".to_owned(),
        "linux/arm64/v9".to_owned(),
        "x".repeat(rish_registry::MAX_GUEST_PLATFORM_TOKEN_BYTES + 1),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let store_root = temporary.path().join("not-created");
        let response = unsafe {
            pull_image_json(
                &request(&store_root, &platform),
                Some(mock_fetch),
                std::ptr::null_mut(),
            )
        };
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["ok"], false);
        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .contains("invalid pull request JSON")
        );
        assert!(!store_root.exists());
    }
}

#[test]
fn direct_manifest_config_must_match_the_requested_guest_architecture() {
    let temporary = tempfile::tempdir().unwrap();
    let store_root = temporary.path().join("content");
    let (host, _, _, _) = pull_fixture();
    let response = unsafe {
        pull_image_json(
            &request(&store_root, "linux/amd64"),
            Some(mock_fetch),
            (&host as *const MockHost).cast_mut().cast(),
        )
    };
    let response: Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("requested linux/amd64")
    );
}
