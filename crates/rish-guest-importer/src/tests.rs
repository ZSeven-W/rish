use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use flate2::Compression;
use flate2::write::GzEncoder;
use rish_pull::{VerifiedDescriptor, VerifiedImageLayer};
use rish_registry::{Digest, MediaType};
use tar::{Builder, EntryType, Header};

use crate::archive::{entry_metadata, entry_path, is_special, scan_layer};
use crate::verify::verify_and_expand;
use crate::{ImportError, ImportLimits, PrivilegedGuestPolicy};

fn append_file(builder: &mut Builder<Vec<u8>>, path: &str, contents: &[u8]) {
    append_file_metadata(builder, path, contents, 0o644, 0, 0, 1_700_000_000);
}

fn append_file_metadata(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    contents: &[u8],
    mode: u32,
    uid: u64,
    gid: u64,
    mtime: u64,
) {
    let mut header = Header::new_gnu();
    header.set_mode(mode);
    header.set_uid(uid);
    header.set_gid(gid);
    header.set_mtime(mtime);
    header.set_size(contents.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(contents))
        .unwrap();
}

#[cfg(target_os = "linux")]
fn append_directory(builder: &mut Builder<Vec<u8>>, path: &str, mode: u32) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::dir());
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(1_700_000_000);
    header.set_size(0);
    header.set_cksum();
    builder.append_data(&mut header, path, io::empty()).unwrap();
}

fn append_link(
    builder: &mut Builder<Vec<u8>>,
    kind: EntryType,
    path: &str,
    target: &str,
    mode: u32,
) {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(0);
    header.set_cksum();
    builder.append_link(&mut header, path, target).unwrap();
}

fn append_special(builder: &mut Builder<Vec<u8>>, kind: EntryType, path: &str) {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(0o600);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(0);
    if kind.is_character_special() || kind.is_block_special() {
        header.set_device_major(1).unwrap();
        header.set_device_minor(3).unwrap();
    }
    header.set_cksum();
    builder.append_data(&mut header, path, io::empty()).unwrap();
}

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().unwrap()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn verified_layer(tar: &[u8], blob: &[u8], media_type: MediaType) -> VerifiedImageLayer {
    VerifiedImageLayer {
        descriptor: VerifiedDescriptor {
            media_type,
            digest: Digest::sha256(blob),
            size: blob.len() as u64,
        },
        diff_id: Digest::sha256(tar),
    }
}

fn spool(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file
}

fn raw_path_tar(path: &[u8], contents: &[u8]) -> Vec<u8> {
    assert!(path.len() <= 100);
    let mut header = Header::new_old();
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(contents.len() as u64);
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_cksum();
    let mut builder = Builder::new(Vec::new());
    builder.append(&header, Cursor::new(contents)).unwrap();
    finish(builder)
}

#[test]
fn verifies_compressed_digest_and_uncompressed_diff_id() {
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "etc/value", b"verified");
    let tar = finish(builder);
    let blob = gzip(&tar);
    let layer = verified_layer(&tar, &blob, MediaType::OciImageLayerGzip);
    let work = tempfile::tempdir().unwrap();

    let mut verified = verify_and_expand(
        Cursor::new(blob.clone()),
        &layer,
        work.path(),
        &ImportLimits::default(),
    )
    .unwrap();

    assert_eq!(verified.compressed_bytes, blob.len() as u64);
    assert_eq!(verified.uncompressed_bytes, tar.len() as u64);
    let mut materialized = Vec::new();
    verified.tar.read_to_end(&mut materialized).unwrap();
    assert_eq!(materialized, tar);
}

#[test]
fn rejects_descriptor_digest_before_decompression() {
    let tar = vec![0_u8; 1024];
    let blob = gzip(&tar);
    let mut layer = verified_layer(&tar, &blob, MediaType::OciImageLayerGzip);
    layer.descriptor.digest = Digest::sha256(b"substituted");
    let work = tempfile::tempdir().unwrap();

    let error = verify_and_expand(
        Cursor::new(blob),
        &layer,
        work.path(),
        &ImportLimits::default(),
    )
    .err()
    .unwrap();

    assert!(matches!(
        error,
        ImportError::DescriptorDigestMismatch { .. }
    ));
}

#[test]
fn rejects_wrong_diff_id_after_bounded_decompression() {
    let tar = vec![0_u8; 1024];
    let blob = gzip(&tar);
    let mut layer = verified_layer(&tar, &blob, MediaType::OciImageLayerGzip);
    layer.diff_id = Digest::sha256(b"different tar");
    let work = tempfile::tempdir().unwrap();

    let error = verify_and_expand(
        Cursor::new(blob),
        &layer,
        work.path(),
        &ImportLimits::default(),
    )
    .err()
    .unwrap();

    assert!(matches!(error, ImportError::DiffIdMismatch { .. }));
}

#[test]
fn compressed_bomb_hits_uncompressed_limit() {
    let tar = vec![0_u8; 128 * 1024];
    let blob = gzip(&tar);
    let layer = verified_layer(&tar, &blob, MediaType::OciImageLayerGzip);
    let work = tempfile::tempdir().unwrap();
    let limits = ImportLimits {
        max_uncompressed_layer_bytes: 4 * 1024,
        ..ImportLimits::default()
    };

    let error = verify_and_expand(Cursor::new(blob), &layer, work.path(), &limits)
        .err()
        .unwrap();

    assert!(matches!(
        error,
        ImportError::UncompressedLayerLimitExceeded { .. }
    ));
}

#[test]
fn rejects_path_and_hardlink_traversal_during_preflight() {
    let limits = ImportLimits::default();
    let mut traversal = spool(&raw_path_tar(b"../../escape", b"owned"));
    let path_error =
        scan_layer(&mut traversal, &limits, PrivilegedGuestPolicy::default()).unwrap_err();
    assert!(matches!(path_error, ImportError::InvalidPath { .. }));

    let mut builder = Builder::new(Vec::new());
    append_link(
        &mut builder,
        EntryType::hard_link(),
        "inside/link",
        "../../escape",
        0o644,
    );
    let mut hardlink = spool(&finish(builder));
    let link_error =
        scan_layer(&mut hardlink, &limits, PrivilegedGuestPolicy::default()).unwrap_err();
    assert!(matches!(link_error, ImportError::InvalidPath { .. }));
}

#[test]
fn rejects_ambiguous_or_oversized_xattrs() {
    let mut supported_builder = Builder::new(Vec::new());
    supported_builder
        .append_pax_extensions([("SCHILY.xattr.user.rish", b"kept".as_slice())])
        .unwrap();
    append_file(&mut supported_builder, "value", b"x");
    let mut supported = tar::Archive::new(Cursor::new(finish(supported_builder)));
    let mut supported_entry = supported.entries().unwrap().next().unwrap().unwrap();
    let supported_path = entry_path(&supported_entry, &ImportLimits::default()).unwrap();
    let mut xattr_bytes = 0;
    let metadata = entry_metadata(
        &mut supported_entry,
        &supported_path,
        &ImportLimits::default(),
        &mut xattr_bytes,
    )
    .unwrap();
    assert_eq!(metadata.uid, 0);
    assert_eq!(metadata.gid, 0);
    assert_eq!(metadata.mtime.seconds, 1_700_000_000);
    assert_eq!(metadata.mtime.nanoseconds, 0);
    assert_eq!(metadata.xattrs[0].name, b"user.rish");
    assert_eq!(metadata.xattrs[0].value, b"kept");

    let mut oversized_builder = Builder::new(Vec::new());
    oversized_builder
        .append_pax_extensions([("SCHILY.xattr.user.rish", b"12345".as_slice())])
        .unwrap();
    append_file(&mut oversized_builder, "value", b"x");
    let mut oversized = spool(&finish(oversized_builder));
    let limits = ImportLimits {
        max_xattr_value_bytes: 4,
        ..ImportLimits::default()
    };
    let oversized_error =
        scan_layer(&mut oversized, &limits, PrivilegedGuestPolicy::default()).unwrap_err();
    assert!(matches!(
        oversized_error,
        ImportError::XattrValueLimitExceeded { .. }
    ));

    let mut unsupported_builder = Builder::new(Vec::new());
    unsupported_builder
        .append_pax_extensions([("LIBARCHIVE.xattr.dXNlci5yaXNo", b"eA==".as_slice())])
        .unwrap();
    append_file(&mut unsupported_builder, "value", b"x");
    let mut unsupported = spool(&finish(unsupported_builder));
    let unsupported_error = scan_layer(
        &mut unsupported,
        &ImportLimits::default(),
        PrivilegedGuestPolicy::default(),
    )
    .unwrap_err();
    assert!(matches!(
        unsupported_error,
        ImportError::UnsupportedMetadataEncoding { .. }
    ));
}

#[test]
fn parses_fractional_pax_times_and_rejects_unrepresentable_metadata() {
    let mut precise_builder = Builder::new(Vec::new());
    precise_builder
        .append_pax_extensions([("mtime", b"1700000000.123456789".as_slice())])
        .unwrap();
    append_file(&mut precise_builder, "value", b"x");
    let mut precise = tar::Archive::new(Cursor::new(finish(precise_builder)));
    let mut entry = precise.entries().unwrap().next().unwrap().unwrap();
    let path = entry_path(&entry, &ImportLimits::default()).unwrap();
    let mut xattr_bytes = 0;
    let metadata = entry_metadata(
        &mut entry,
        &path,
        &ImportLimits::default(),
        &mut xattr_bytes,
    )
    .unwrap();
    assert_eq!(metadata.mtime.seconds, 1_700_000_000);
    assert_eq!(metadata.mtime.nanoseconds, 123_456_789);

    for (key, value) in [
        ("mtime", b"1.0000000001".as_slice()),
        ("atime", b"-1.5".as_slice()),
        ("ctime", b"1".as_slice()),
    ] {
        let mut builder = Builder::new(Vec::new());
        builder.append_pax_extensions([(key, value)]).unwrap();
        append_file(&mut builder, "value", b"x");
        let mut archive = spool(&finish(builder));
        assert!(
            scan_layer(
                &mut archive,
                &ImportLimits::default(),
                PrivilegedGuestPolicy::default(),
            )
            .is_err()
        );
    }
}

#[test]
fn validates_file_capability_wire_format_and_target() {
    let valid_capability = [
        0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut valid_builder = Builder::new(Vec::new());
    valid_builder
        .append_pax_extensions([(
            "SCHILY.xattr.security.capability",
            valid_capability.as_slice(),
        )])
        .unwrap();
    append_file(&mut valid_builder, "bin/tool", b"x");
    let mut valid = spool(&finish(valid_builder));
    scan_layer(
        &mut valid,
        &ImportLimits::default(),
        PrivilegedGuestPolicy::default(),
    )
    .unwrap();

    let mut v3 = [0_u8; 24];
    v3[..4].copy_from_slice(&0x0300_0001_u32.to_le_bytes());
    let mut v3_builder = Builder::new(Vec::new());
    v3_builder
        .append_pax_extensions([("SCHILY.xattr.security.capability", v3.as_slice())])
        .unwrap();
    append_file(&mut v3_builder, "bin/tool", b"x");
    let mut v3_archive = spool(&finish(v3_builder));
    let error = scan_layer(
        &mut v3_archive,
        &ImportLimits::default(),
        PrivilegedGuestPolicy::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ImportError::InvalidFileCapability { .. }));

    let mut link_builder = Builder::new(Vec::new());
    link_builder
        .append_pax_extensions([(
            "SCHILY.xattr.security.capability",
            valid_capability.as_slice(),
        )])
        .unwrap();
    append_link(
        &mut link_builder,
        EntryType::hard_link(),
        "bin/copy",
        "bin/tool",
        0o755,
    );
    let mut link = spool(&finish(link_builder));
    let error = scan_layer(
        &mut link,
        &ImportLimits::default(),
        PrivilegedGuestPolicy::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ImportError::UnsupportedCapabilityTarget { .. }
    ));
}

#[test]
fn binary_pax_value_with_line_feed_fails_closed() {
    // tar 0.4.x splits PAX records on LF, so it cannot faithfully recover
    // arbitrary SCHILY binary values containing 0x0a. Until the importer owns
    // a length-driven raw PAX parser, this encoding must be rejected.
    let capability_with_lf = [
        0x01, 0x00, 0x00, 0x02, 0x0a, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut builder = Builder::new(Vec::new());
    builder
        .append_pax_extensions([(
            "SCHILY.xattr.security.capability",
            capability_with_lf.as_slice(),
        )])
        .unwrap();
    append_file(&mut builder, "bin/tool", b"x");
    let mut archive = spool(&finish(builder));

    assert!(
        scan_layer(
            &mut archive,
            &ImportLimits::default(),
            PrivilegedGuestPolicy::default(),
        )
        .is_err()
    );
}

#[test]
fn pax_header_is_bounded_before_tar_allocates_it() {
    let mut builder = Builder::new(Vec::new());
    builder
        .append_pax_extensions([("SCHILY.xattr.user.rish", [b'x'; 64].as_slice())])
        .unwrap();
    append_file(&mut builder, "value", b"x");
    let mut archive = spool(&finish(builder));
    let limits = ImportLimits {
        max_pax_header_bytes: 32,
        ..ImportLimits::default()
    };

    let error = scan_layer(&mut archive, &limits, PrivilegedGuestPolicy::default()).unwrap_err();

    assert!(matches!(error, ImportError::PaxHeaderLimitExceeded { .. }));
}

#[test]
fn rejects_special_files_without_explicit_guest_policy() {
    assert!(is_special(EntryType::block_special()));
    for (kind, expected_device) in [
        (EntryType::character_special(), true),
        (EntryType::fifo(), false),
    ] {
        let mut builder = Builder::new(Vec::new());
        append_special(&mut builder, kind, "dev/special");
        let mut archive = spool(&finish(builder));
        let error = scan_layer(
            &mut archive,
            &ImportLimits::default(),
            PrivilegedGuestPolicy::default(),
        )
        .unwrap_err();
        if expected_device {
            assert!(matches!(error, ImportError::DevicePolicyRequired(_)));
        } else {
            assert!(matches!(error, ImportError::FifoPolicyRequired(_)));
        }
    }
}

#[test]
fn rejects_whiteout_payload_and_unrepresentable_symlink_mode() {
    let mut whiteout_builder = Builder::new(Vec::new());
    append_file(&mut whiteout_builder, ".wh.secret", b"not empty");
    let mut whiteout = spool(&finish(whiteout_builder));
    let error = scan_layer(
        &mut whiteout,
        &ImportLimits::default(),
        PrivilegedGuestPolicy::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ImportError::InvalidWhiteout(_)));

    let mut symlink_builder = Builder::new(Vec::new());
    append_link(
        &mut symlink_builder,
        EntryType::symlink(),
        "link",
        "/guest/absolute",
        0o755,
    );
    let mut symlink = spool(&finish(symlink_builder));
    let error = scan_layer(
        &mut symlink,
        &ImportLimits::default(),
        PrivilegedGuestPolicy::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ImportError::UnsupportedSymlinkMode { .. }));
}

#[test]
fn entry_limit_fails_closed() {
    let mut builder = Builder::new(Vec::new());
    append_file(&mut builder, "one", b"1");
    append_file(&mut builder, "two", b"2");
    let mut archive = spool(&finish(builder));
    let limits = ImportLimits {
        max_entries_per_layer: 1,
        ..ImportLimits::default()
    };

    let error = scan_layer(&mut archive, &limits, PrivilegedGuestPolicy::default()).unwrap_err();

    assert!(matches!(error, ImportError::LayerEntryLimitExceeded { .. }));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn public_import_api_fails_closed_off_linux() {
    use rish_pull::{
        VERIFIED_IMAGE_RECORD_SCHEMA_VERSION, VerifiedImageRecord, VerifiedPlatform,
        VerifiedProcessConfig,
    };

    struct UnusedSource;
    impl crate::DescriptorSource for UnusedSource {
        type Reader = Cursor<Vec<u8>>;

        fn open(&mut self, _descriptor: &VerifiedDescriptor) -> Result<Self::Reader, ImportError> {
            panic!("off-Linux importer must not open a descriptor")
        }
    }

    let digest = Digest::sha256(b"record");
    let descriptor = VerifiedDescriptor {
        media_type: MediaType::OciImageManifest,
        digest: digest.clone(),
        size: 0,
    };
    let record = VerifiedImageRecord {
        schema_version: VERIFIED_IMAGE_RECORD_SCHEMA_VERSION,
        normalized_reference: "example.invalid/image@sha256:00".to_owned(),
        resolved_digest: digest.clone(),
        index_descriptor: None,
        manifest_descriptor: descriptor.clone(),
        config_descriptor: VerifiedDescriptor {
            media_type: MediaType::OciImageConfig,
            digest,
            size: 0,
        },
        platform: VerifiedPlatform {
            os: "linux".to_owned(),
            architecture: "amd64".to_owned(),
            variant: None,
        },
        layers: Vec::new(),
        process: VerifiedProcessConfig {
            entrypoint: Vec::new(),
            cmd: Vec::new(),
            env: Vec::new(),
            working_dir: "/".to_owned(),
            user: "0".to_owned(),
        },
    };
    let root = tempfile::tempdir().unwrap();
    let mut source = UnusedSource;
    let error = crate::import_verified_image(
        &record,
        &mut source,
        root.path().join("rootfs"),
        &crate::ImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, ImportError::UnsupportedPlatform));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_guest_import_preserves_metadata_and_publishes_atomically() {
    use std::collections::VecDeque;
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    use rish_pull::{
        VERIFIED_IMAGE_RECORD_SCHEMA_VERSION, VerifiedImageRecord, VerifiedPlatform,
        VerifiedProcessConfig,
    };

    if unsafe { libc::geteuid() } != 0 {
        return;
    }

    struct QueueSource {
        blobs: VecDeque<Vec<u8>>,
        opens: u64,
    }
    impl crate::DescriptorSource for QueueSource {
        type Reader = Cursor<Vec<u8>>;

        fn open(&mut self, descriptor: &VerifiedDescriptor) -> Result<Self::Reader, ImportError> {
            let blob = self
                .blobs
                .pop_front()
                .ok_or_else(|| ImportError::BlobSource("missing test blob".to_owned()))?;
            if Digest::sha256(&blob) != descriptor.digest {
                return Err(ImportError::BlobSource(
                    "test descriptor order mismatch".to_owned(),
                ));
            }
            self.opens += 1;
            Ok(Cursor::new(blob))
        }
    }

    let file_capability = [
        0x01, 0x00, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut lower_builder = Builder::new(Vec::new());
    append_directory(&mut lower_builder, "bin", 0o755);
    append_directory(&mut lower_builder, "etc", 0o750);
    append_directory(&mut lower_builder, "replace-me", 0o700);
    append_file(&mut lower_builder, "replace-me", b"final file");
    append_file(&mut lower_builder, "etc/old", b"lower");
    lower_builder
        .append_pax_extensions([
            ("SCHILY.xattr.user.rish", b"preserved".as_slice()),
            (
                "SCHILY.xattr.security.capability",
                file_capability.as_slice(),
            ),
        ])
        .unwrap();
    append_file_metadata(
        &mut lower_builder,
        "bin/tool",
        b"#!/bin/sh\n",
        0o4755,
        0,
        0,
        1_650_000_000,
    );
    append_link(
        &mut lower_builder,
        EntryType::hard_link(),
        "bin/tool-copy",
        "bin/tool",
        0o4755,
    );
    append_link(
        &mut lower_builder,
        EntryType::symlink(),
        "bin/absolute-link",
        "/bin/tool",
        0o777,
    );
    let lower_tar = finish(lower_builder);
    let lower_blob = gzip(&lower_tar);

    let mut upper_builder = Builder::new(Vec::new());
    append_file(&mut upper_builder, "etc/.wh.old", b"");
    append_file(&mut upper_builder, "etc/new", b"upper");
    append_special(&mut upper_builder, EntryType::fifo(), "run/control");
    let upper_tar = finish(upper_builder);
    let upper_blob = gzip(&upper_tar);
    let layers = vec![
        verified_layer(&lower_tar, &lower_blob, MediaType::OciImageLayerGzip),
        verified_layer(&upper_tar, &upper_blob, MediaType::OciImageLayerGzip),
    ];
    let graph_digest = Digest::sha256(b"verified graph");
    let record = VerifiedImageRecord {
        schema_version: VERIFIED_IMAGE_RECORD_SCHEMA_VERSION,
        normalized_reference: "example.invalid/library/test:latest".to_owned(),
        resolved_digest: graph_digest.clone(),
        index_descriptor: None,
        manifest_descriptor: VerifiedDescriptor {
            media_type: MediaType::OciImageManifest,
            digest: graph_digest.clone(),
            size: 0,
        },
        config_descriptor: VerifiedDescriptor {
            media_type: MediaType::OciImageConfig,
            digest: Digest::sha256(b"config"),
            size: 0,
        },
        platform: VerifiedPlatform {
            os: "linux".to_owned(),
            architecture: "amd64".to_owned(),
            variant: None,
        },
        layers,
        process: VerifiedProcessConfig {
            entrypoint: vec!["/bin/tool".to_owned()],
            cmd: Vec::new(),
            env: Vec::new(),
            working_dir: "/".to_owned(),
            user: "0".to_owned(),
        },
    };
    let sandbox = tempfile::tempdir().unwrap();
    let destination = sandbox.path().join("rootfs");
    let mut source = QueueSource {
        blobs: VecDeque::from([lower_blob, upper_blob]),
        opens: 0,
    };
    let options = crate::ImportOptions {
        privileged_guest: PrivilegedGuestPolicy::after_capability_negotiation(false, true),
        ..crate::ImportOptions::default()
    };

    let report =
        crate::import_verified_image(&record, &mut source, &destination, &options).unwrap();

    assert_eq!(report.layers, 2);
    assert_eq!(source.opens, 2);
    assert!(!destination.join("etc/old").exists());
    assert_eq!(fs::read(destination.join("etc/new")).unwrap(), b"upper");
    assert_eq!(
        fs::read(destination.join("replace-me")).unwrap(),
        b"final file"
    );
    assert_eq!(
        fs::read_link(destination.join("bin/absolute-link")).unwrap(),
        std::path::PathBuf::from("/bin/tool")
    );
    let tool = fs::metadata(destination.join("bin/tool")).unwrap();
    let copy = fs::metadata(destination.join("bin/tool-copy")).unwrap();
    assert_eq!(tool.ino(), copy.ino());
    assert_eq!(tool.mode() & 0o7777, 0o4755);
    assert_eq!(tool.uid(), 0);
    assert_eq!(tool.gid(), 0);
    assert_eq!(tool.mtime(), 1_650_000_000);
    assert_eq!(
        fs::symlink_metadata(destination.join("run/control"))
            .unwrap()
            .mode()
            & libc::S_IFMT,
        libc::S_IFIFO
    );

    let tool_path = CString::new(destination.join("bin/tool").as_os_str().as_bytes()).unwrap();
    let name = c"user.rish";
    let mut value = [0_u8; 32];
    let count = unsafe {
        libc::getxattr(
            tool_path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    assert_eq!(count, 9);
    assert_eq!(&value[..count as usize], b"preserved");
    let capability_name = c"security.capability";
    let mut capability = [0_u8; 24];
    let capability_count = unsafe {
        libc::getxattr(
            tool_path.as_ptr(),
            capability_name.as_ptr(),
            capability.as_mut_ptr().cast(),
            capability.len(),
        )
    };
    assert_eq!(capability_count, 20);
    assert_eq!(&capability[..20], &file_capability);

    let mut unused = QueueSource {
        blobs: VecDeque::new(),
        opens: 0,
    };
    let error =
        crate::import_verified_image(&record, &mut unused, &destination, &options).unwrap_err();
    assert!(matches!(error, ImportError::DestinationExists(_)));
    assert_eq!(unused.opens, 0);
    assert_eq!(fs::read(destination.join("etc/new")).unwrap(), b"upper");
}
