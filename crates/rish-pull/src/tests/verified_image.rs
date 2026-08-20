use std::fs;

use rish_content::StoreError;

use super::*;
use crate::{VerifiedImageRecordError, VerifiedImageRecordStore};

fn persist_index_fixture() -> (
    TempDir,
    ContentStore,
    Digest,
    Sha256Digest,
    crate::VerifiedImageHandle,
) {
    let fixture = index_fixture();
    let resolved = fixture.root.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (temporary, store) = content_store();
    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();
    let layer_digest = image.layers[0].content.digest;
    let handle = VerifiedImageRecordStore::open(&store)
        .unwrap()
        .persist(&image)
        .unwrap();
    drop(image);
    (temporary, store, resolved, layer_digest, handle)
}

fn empty_command_fixture() -> Fixture {
    let parts = manifest_parts_with_config(
        &[MediaType::OciImageLayerGzip],
        1,
        false,
        "arm64",
        ImageConfig {
            env: vec!["PATH=/bin".to_owned()],
            working_dir: "/".to_owned(),
            ..ImageConfig::default()
        },
    );
    let reference: ImageReference =
        format!("registry.test/team/override@{}", parts.descriptor.digest)
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

#[test]
fn verified_record_round_trips_after_content_store_restart() {
    let (temporary, store, resolved, _layer, persisted) = persist_index_fixture();
    assert_eq!(persisted.record.schema_version, 1);
    assert_eq!(persisted.record.platform.os, "linux");
    assert_eq!(persisted.record.platform.architecture, "arm64");
    assert_eq!(persisted.record.platform.variant.as_deref(), Some("v8"));
    assert!(persisted.record.index_descriptor.is_some());
    assert_eq!(persisted.record.layers.len(), 2);
    assert_eq!(
        persisted.record.process.env,
        ["PATH=/usr/bin:/bin".to_owned()]
    );
    assert_eq!(persisted.record.process.cmd, ["/bin/sh".to_owned()]);
    let debug = format!("{persisted:?}");
    assert!(!debug.contains("PATH="));
    assert!(!debug.contains("/usr/bin:/bin"));

    let root = store.root().to_owned();
    drop(store);
    let reopened_store = ContentStore::open(StoreConfig::new(&root)).unwrap();
    let reopened = VerifiedImageRecordStore::open(&reopened_store)
        .unwrap()
        .reopen(&resolved)
        .unwrap();

    assert_eq!(reopened.record, persisted.record);
    assert_eq!(reopened.record_digest, persisted.record_digest);
    assert_eq!(reopened.graph_pin, persisted.graph_pin);
    assert!(temporary.path().exists());
}

#[test]
fn tampered_record_cas_object_fails_digest_verification() {
    let (_temporary, store, resolved, _layer, persisted) = persist_index_fixture();
    let path = store.blob_path(persisted.record_digest);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 1;
    fs::write(path, bytes).unwrap();

    let error = VerifiedImageRecordStore::open(&store)
        .unwrap()
        .reopen(&resolved)
        .unwrap_err();

    assert!(matches!(
        error,
        VerifiedImageRecordError::Store(StoreError::DigestMismatch { .. })
    ));
    let rendered = error.to_string();
    assert!(!rendered.contains("PATH="));
    assert!(!rendered.contains("/usr/bin:/bin"));
}

#[test]
fn missing_reachable_blob_fails_closed_on_reopen() {
    let (_temporary, store, resolved, layer, _persisted) = persist_index_fixture();
    fs::remove_file(store.blob_path(layer)).unwrap();

    let error = VerifiedImageRecordStore::open(&store)
        .unwrap()
        .reopen(&resolved)
        .unwrap_err();

    assert!(matches!(
        error,
        VerifiedImageRecordError::Store(StoreError::BlobNotFound(missing)) if missing == layer
    ));
}

#[test]
fn missing_graph_pin_fails_before_reopen_returns_metadata() {
    let (_temporary, store, resolved, layer, persisted) = persist_index_fixture();
    assert!(store.unpin(&persisted.graph_pin, layer).unwrap());

    let error = VerifiedImageRecordStore::open(&store)
        .unwrap()
        .reopen(&resolved)
        .unwrap_err();

    assert!(matches!(error, VerifiedImageRecordError::GraphPinMismatch));
}

#[test]
fn missing_record_pin_fails_closed() {
    let (_temporary, store, resolved, _layer, persisted) = persist_index_fixture();
    let record_pin = store
        .pins()
        .unwrap()
        .into_iter()
        .find(|pin| pin.digest == persisted.record_digest && pin.name.starts_with("record-"))
        .unwrap();
    assert!(
        store
            .unpin(&record_pin.name, persisted.record_digest)
            .unwrap()
    );

    let error = VerifiedImageRecordStore::open(&store)
        .unwrap()
        .reopen(&resolved)
        .unwrap_err();

    assert!(matches!(error, VerifiedImageRecordError::MissingRecordPin));
}

#[test]
fn empty_image_command_round_trips_for_a_future_runtime_override() {
    let fixture = empty_command_fixture();
    let resolved = fixture.root.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (temporary, store) = content_store();
    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();
    assert!(image.image_configuration.config.entrypoint.is_empty());
    assert!(image.image_configuration.config.cmd.is_empty());

    let persisted = VerifiedImageRecordStore::open(&store)
        .unwrap()
        .persist(&image)
        .unwrap();
    assert!(persisted.record.process.entrypoint.is_empty());
    assert!(persisted.record.process.cmd.is_empty());
    drop(image);
    let root = store.root().to_owned();
    drop(store);

    let reopened_store = ContentStore::open(StoreConfig::new(root)).unwrap();
    let reopened = VerifiedImageRecordStore::open(&reopened_store)
        .unwrap()
        .reopen(&resolved)
        .unwrap();
    assert!(reopened.record.process.entrypoint.is_empty());
    assert!(reopened.record.process.cmd.is_empty());
    assert!(temporary.path().exists());
}

#[test]
fn replacing_same_digest_from_an_alias_prunes_superseded_record_pin() {
    let tagged = tagged_manifest_fixture();
    let resolved = tagged.root.digest.clone();
    let tagged_transport = MockTransport::new(tagged.exchanges);
    let (_temporary, store) = content_store();
    let tagged_image = Puller::new(&tagged_transport, &store)
        .pull(&tagged.reference)
        .unwrap();
    let records = VerifiedImageRecordStore::open(&store).unwrap();
    let first = records.persist(&tagged_image).unwrap();
    drop(tagged_image);

    let direct = direct_fixture();
    assert_eq!(direct.root.digest, resolved);
    let direct_transport = MockTransport::new(direct.exchanges);
    let direct_image = Puller::new(&direct_transport, &store)
        .pull(&direct.reference)
        .unwrap();
    let second = records.persist(&direct_image).unwrap();

    assert_ne!(first.record_digest, second.record_digest);
    let record_pin = format!("record-{}-linux-arm64-v8", resolved.encoded());
    let pinned_records = store
        .pins()
        .unwrap()
        .into_iter()
        .filter(|pin| pin.name == record_pin)
        .collect::<Vec<_>>();
    assert_eq!(pinned_records.len(), 1);
    assert_eq!(pinned_records[0].digest, second.record_digest);
    let reopened = records.reopen(&resolved).unwrap();
    assert_eq!(
        reopened.record.normalized_reference,
        direct.reference.to_string()
    );
}
