use std::fs;
use std::io::{self, Cursor, Write};
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tar::{Builder, EntryType, Header};

use super::*;

fn append_file(builder: &mut Builder<Vec<u8>>, path: &str, contents: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(contents.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(contents))
        .unwrap();
}

fn append_empty(builder: &mut Builder<Vec<u8>>, path: &str) {
    append_file(builder, path, &[]);
}

fn append_directory(builder: &mut Builder<Vec<u8>>, path: &str, mode: u32) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::dir());
    header.set_mode(mode);
    header.set_size(0);
    header.set_cksum();
    builder.append_data(&mut header, path, io::empty()).unwrap();
}

fn append_link(builder: &mut Builder<Vec<u8>>, kind: EntryType, path: &str, target: &str) {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(0o777);
    header.set_size(0);
    header.set_cksum();
    builder.append_link(&mut header, path, target).unwrap();
}

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().unwrap()
}

fn diff_id(tar: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(tar))
}

fn raw_path_tar(path: &[u8], contents: &[u8]) -> Vec<u8> {
    assert!(path.len() <= 100);
    let mut header = Header::new_old();
    header.set_mode(0o644);
    header.set_size(contents.len() as u64);
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_cksum();
    let mut builder = Builder::new(Vec::new());
    builder.append(&header, Cursor::new(contents)).unwrap();
    finish(builder)
}

#[test]
fn rejects_parent_traversal_before_writing() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let archive = raw_path_tar(b"../evil", b"owned");

    let error = apply_layer(Cursor::new(archive), &root, &ApplyOptions::default()).unwrap_err();

    assert!(matches!(error, LayerError::InvalidPath { .. }));
    assert!(!sandbox.path().join("evil").exists());
    assert!(
        !root.exists(),
        "preflight must happen before destination creation"
    );
}

#[test]
fn rejects_absolute_paths() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let archive = raw_path_tar(b"/tmp/evil", b"owned");

    let error = apply_layer(Cursor::new(archive), &root, &ApplyOptions::default()).unwrap_err();

    assert!(matches!(error, LayerError::InvalidPath { .. }));
    assert!(!root.exists());
}

#[test]
fn rejects_symlink_target_that_escapes_root() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_link(
        &mut builder,
        EntryType::symlink(),
        "inside/link",
        "../../outside",
    );

    let error = apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, LayerError::SymlinkEscape { .. }));
    assert!(!root.exists());
}

#[test]
fn refuses_to_extract_through_a_symlink_component() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_link(&mut builder, EntryType::symlink(), "redirect", "inside");
    append_file(&mut builder, "redirect/payload", b"owned");

    let error = apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, LayerError::SymlinkPathComponent { .. }));
    assert!(!root.join("inside/payload").exists());
}

#[test]
fn rejects_hardlink_traversal() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_link(&mut builder, EntryType::hard_link(), "link", "../outside");

    let error = apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, LayerError::InvalidPath { .. }));
    assert!(!root.exists());
}

#[test]
fn applies_removal_and_order_independent_opaque_whiteouts() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    fs::create_dir_all(root.join("regular")).unwrap();
    fs::write(root.join("regular/remove"), b"lower").unwrap();
    fs::write(root.join("regular/keep"), b"lower").unwrap();
    fs::create_dir_all(root.join("opaque/nested")).unwrap();
    fs::write(root.join("opaque/lower"), b"lower").unwrap();
    fs::write(root.join("opaque/nested/lower"), b"lower").unwrap();

    let mut builder = Builder::new(Vec::new());
    append_empty(&mut builder, "regular/.wh.remove");
    append_file(&mut builder, "opaque/new", b"upper");
    // Deliberately after the new entry: application is based on layer
    // semantics, not archive ordering.
    append_empty(&mut builder, "opaque/.wh..wh..opq");

    let report = apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap();

    assert!(!root.join("regular/remove").exists());
    assert_eq!(fs::read(root.join("regular/keep")).unwrap(), b"lower");
    assert!(!root.join("opaque/lower").exists());
    assert!(!root.join("opaque/nested").exists());
    assert_eq!(fs::read(root.join("opaque/new")).unwrap(), b"upper");
    assert_eq!(report.whiteouts, 2);
}

#[test]
fn per_file_limit_fails_during_preflight() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "large", b"12345");
    let mut options = ApplyOptions::default();
    options.limits.max_file_size = 4;

    let error = apply_layer(Cursor::new(finish(builder)), &root, &options).unwrap_err();

    assert!(matches!(
        error,
        LayerError::FileSizeLimitExceeded {
            size: 5,
            limit: 4,
            ..
        }
    ));
    assert!(!root.exists());
}

#[test]
fn total_size_limit_fails_during_preflight() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "one", b"123");
    append_file(&mut builder, "two", b"456");
    let mut options = ApplyOptions::default();
    options.limits.max_total_size = 5;

    let error = apply_layer(Cursor::new(finish(builder)), &root, &options).unwrap_err();

    assert!(matches!(
        error,
        LayerError::TotalSizeLimitExceeded { limit: 5 }
    ));
    assert!(!root.exists());
}

#[test]
fn entry_count_limit_fails_during_preflight() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_empty(&mut builder, "one");
    append_empty(&mut builder, "two");
    let mut options = ApplyOptions::default();
    options.limits.max_entries = 1;

    let error = apply_layer(Cursor::new(finish(builder)), &root, &options).unwrap_err();

    assert!(matches!(error, LayerError::EntryLimitExceeded { limit: 1 }));
    assert!(!root.exists());
}

#[test]
fn creates_in_root_hardlinks_to_regular_files() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "original", b"same inode");
    append_link(&mut builder, EntryType::hard_link(), "copy", "original");

    let report = apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap();

    assert_eq!(fs::read(root.join("copy")).unwrap(), b"same inode");
    assert_eq!(report.hardlinks, 1);
}

#[test]
fn gzip_layers_are_detected_and_applied() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "hello", b"mobile");
    let tar = finish(builder);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar).unwrap();
    let gzip = encoder.finish().unwrap();
    let expected = diff_id(&tar);
    let options = ApplyOptions {
        expected_diff_id: Some(expected.to_ascii_uppercase()),
        ..ApplyOptions::default()
    };

    let report = apply_layer(Cursor::new(gzip), &root, &options).unwrap();

    assert_eq!(fs::read(root.join("hello")).unwrap(), b"mobile");
    assert_eq!(report.expanded_bytes, 6);
    assert_eq!(report.diff_id, expected);
}

#[test]
fn gzip_diff_id_mismatch_precedes_destination_mutation() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "must-not-exist", b"mobile");
    let tar = finish(builder);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar).unwrap();
    let gzip = encoder.finish().unwrap();
    let options = ApplyOptions {
        expected_diff_id: Some(format!("sha256:{}", "00".repeat(32))),
        ..ApplyOptions::default()
    };

    let error = apply_layer(Cursor::new(gzip), &root, &options).unwrap_err();

    assert!(matches!(error, LayerError::DiffIdMismatch { .. }));
    assert!(!root.exists());
}

#[test]
fn rejects_device_nodes_by_default() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::character_special());
    header.set_device_major(1).unwrap();
    header.set_device_minor(3).unwrap();
    header.set_size(0);
    header.set_cksum();
    let mut builder = Builder::new(Vec::new());
    builder
        .append_data(&mut header, Path::new("dev/null"), io::empty())
        .unwrap();

    let error = apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(error, LayerError::SpecialFileRejected { .. }));
}

#[cfg(unix)]
#[test]
fn preserves_sticky_directory_mode_but_strips_setid_bits() {
    use std::os::unix::fs::PermissionsExt as _;

    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::dir());
    header.set_mode(0o7777);
    header.set_size(0);
    header.set_cksum();
    let mut builder = Builder::new(Vec::new());
    builder
        .append_data(&mut header, Path::new("tmp"), io::empty())
        .unwrap();

    apply_layer(
        Cursor::new(finish(builder)),
        &root,
        &ApplyOptions::default(),
    )
    .unwrap();

    assert_eq!(
        fs::metadata(root.join("tmp")).unwrap().permissions().mode() & 0o7777,
        0o1777
    );
}

#[cfg(unix)]
#[test]
fn keeps_directories_owner_writable_across_layers() {
    use std::os::unix::fs::PermissionsExt as _;

    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let mut base = Builder::new(Vec::new());
    append_directory(&mut base, "locked", 0);
    apply_layer(Cursor::new(finish(base)), &root, &ApplyOptions::default()).unwrap();

    let mut upper = Builder::new(Vec::new());
    append_file(&mut upper, "locked/next", b"works");
    apply_layer(Cursor::new(finish(upper)), &root, &ApplyOptions::default()).unwrap();

    assert_eq!(fs::read(root.join("locked/next")).unwrap(), b"works");
    assert_eq!(
        fs::metadata(root.join("locked"))
            .unwrap()
            .permissions()
            .mode()
            & 0o700,
        0o700
    );
}

#[test]
fn uses_an_explicit_app_owned_spool_directory() {
    let sandbox = tempfile::tempdir().unwrap();
    let spool = sandbox.path().join("cache");
    let root = sandbox.path().join("root");
    fs::create_dir(&spool).unwrap();
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "hello", b"mobile");
    let options = ApplyOptions {
        spool_directory: Some(spool),
        ..ApplyOptions::default()
    };

    apply_layer(Cursor::new(finish(builder)), &root, &options).unwrap();

    assert_eq!(fs::read(root.join("hello")).unwrap(), b"mobile");
}
