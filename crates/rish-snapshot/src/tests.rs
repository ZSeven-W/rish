use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};

use flate2::Compression;
use flate2::write::GzEncoder;
use rish_content::{ContentStore, StoreConfig};
use rish_registry::{Descriptor, Digest, MediaType};
use tar::{Builder, Header};

use super::*;

fn store() -> (tempfile::TempDir, ContentStore) {
    let temporary = tempfile::tempdir().unwrap();
    let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();
    (temporary, store)
}

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
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
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn insert_layer(store: &ContentStore, tar: &[u8], media_type: MediaType) -> (Descriptor, Digest) {
    let blob = match media_type {
        MediaType::OciImageLayer | MediaType::Other(_) => tar.to_vec(),
        _ => gzip(tar),
    };
    let stored = store.ingest_bytes(&blob).unwrap();
    (
        Descriptor {
            media_type,
            digest: Digest::sha256(&blob),
            size: stored.size,
            urls: Vec::new(),
            annotations: BTreeMap::new(),
            data: None,
            platform: None,
            artifact_type: None,
        },
        Digest::sha256(tar),
    )
}

#[test]
fn applies_two_layers_and_whiteout_in_order() {
    let (_temporary, store) = store();
    let base = archive(&[
        ("etc/old", b"old"),
        ("etc/keep", b"keep"),
        ("var/base", b"base"),
    ]);
    let upper = archive(&[("etc/.wh.old", b""), ("etc/new", b"new")]);
    let (base_descriptor, base_diff_id) = insert_layer(&store, &base, MediaType::OciImageLayer);
    let (upper_descriptor, upper_diff_id) =
        insert_layer(&store, &upper, MediaType::OciImageLayerGzip);

    let result = materialize_snapshot(
        &store,
        &[base_descriptor, upper_descriptor],
        &[base_diff_id, upper_diff_id],
        "rootfs-01",
        &SnapshotOptions::default(),
    )
    .unwrap();

    assert_eq!(result.path, store.root().join("snapshots/rootfs-01"));
    assert_eq!(result.layers.len(), 2);
    assert_eq!(result.layers[1].whiteouts, 1);
    assert!(!result.path.join("etc/old").exists());
    assert_eq!(fs::read(result.path.join("etc/keep")).unwrap(), b"keep");
    assert_eq!(fs::read(result.path.join("etc/new")).unwrap(), b"new");
    assert_eq!(fs::read(result.path.join("var/base")).unwrap(), b"base");
}

#[test]
fn diff_id_mismatch_does_not_publish_or_leave_staging() {
    let (_temporary, store) = store();
    let layer = archive(&[("file", b"content")]);
    let (descriptor, _) = insert_layer(&store, &layer, MediaType::OciImageLayerGzip);
    let wrong_diff_id = Digest::sha256(b"not the uncompressed tar");

    let error = materialize_snapshot(
        &store,
        &[descriptor],
        &[wrong_diff_id],
        "must-not-exist",
        &SnapshotOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SnapshotError::Apply {
            index: 0,
            source: rish_layer::LayerError::DiffIdMismatch { .. }
        }
    ));
    let snapshots = store.root().join("snapshots");
    assert!(!snapshots.join("must-not-exist").exists());
    assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
}

#[test]
fn rejects_zstd_foreign_and_unknown_media_types() {
    let (_temporary, store) = store();
    let layer = archive(&[("file", b"content")]);

    for (index, media_type) in [
        MediaType::OciImageLayerZstd,
        MediaType::DockerForeignLayerGzip,
        MediaType::Other("application/vnd.example.layer+gzip".to_owned()),
        MediaType::OctetStream,
    ]
    .into_iter()
    .enumerate()
    {
        let (descriptor, diff_id) = insert_layer(&store, &layer, media_type.clone());
        let error = materialize_snapshot(
            &store,
            &[descriptor],
            &[diff_id],
            &format!("unsupported-{index}"),
            &SnapshotOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            SnapshotError::UnsupportedLayerMediaType {
                index: 0,
                media_type: actual
            } if actual == media_type
        ));
    }
    assert!(!store.root().join("snapshots").exists());
}

#[test]
fn rejects_unsafe_snapshot_ids() {
    let (_temporary, store) = store();
    let layer = archive(&[("file", b"content")]);
    let (descriptor, diff_id) = insert_layer(&store, &layer, MediaType::OciImageLayer);

    for id in ["", ".", "..", "../escape", "a/b", ".hidden", "含中文"] {
        let error = materialize_snapshot(
            &store,
            std::slice::from_ref(&descriptor),
            std::slice::from_ref(&diff_id),
            id,
            &SnapshotOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(error, SnapshotError::InvalidSnapshotId(_)), "{id}");
    }
    assert!(!store.root().join("snapshots").exists());
}

#[test]
fn existing_snapshot_is_never_overwritten() {
    let (_temporary, store) = store();
    let original = archive(&[("value", b"original")]);
    let replacement = archive(&[("value", b"replacement")]);
    let (original_descriptor, original_diff_id) =
        insert_layer(&store, &original, MediaType::OciImageLayer);
    let (replacement_descriptor, replacement_diff_id) =
        insert_layer(&store, &replacement, MediaType::OciImageLayer);

    let first = materialize_snapshot(
        &store,
        &[original_descriptor],
        &[original_diff_id],
        "stable",
        &SnapshotOptions::default(),
    )
    .unwrap();
    let error = materialize_snapshot(
        &store,
        &[replacement_descriptor],
        &[replacement_diff_id],
        "stable",
        &SnapshotOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, SnapshotError::SnapshotAlreadyExists(_)));
    assert_eq!(fs::read(first.path.join("value")).unwrap(), b"original");
}

#[cfg(unix)]
#[test]
fn rejects_symlink_store_root_and_snapshot_target() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let real_root = temporary.path().join("real");
    let real_store = ContentStore::open(StoreConfig::new(&real_root)).unwrap();
    let layer = archive(&[("file", b"content")]);
    let (descriptor, diff_id) = insert_layer(&real_store, &layer, MediaType::OciImageLayer);

    let linked_root = temporary.path().join("linked");
    symlink(&real_root, &linked_root).unwrap();
    assert!(ContentStore::open(StoreConfig::new(&linked_root)).is_err());

    let snapshots = real_store.root().join("snapshots");
    fs::create_dir(&snapshots).unwrap();
    let external = temporary.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("sentinel"), b"safe").unwrap();
    symlink(&external, snapshots.join("target-link")).unwrap();
    let target_error = materialize_snapshot(
        &real_store,
        &[descriptor],
        &[diff_id],
        "target-link",
        &SnapshotOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        target_error,
        SnapshotError::UnsafeSnapshotTarget(_)
    ));
    assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"safe");
}
