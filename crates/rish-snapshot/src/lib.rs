//! Transactional userspace materialization of OCI root filesystems.
//!
//! Every input layer is first validated against the content-addressed store.
//! Layers are then applied in order to a private directory beneath
//! `snapshots/`. The completed tree becomes visible only through an atomic,
//! no-replace rename. Failed builds leave no published snapshot.

mod error;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use error::SnapshotError;
use rish_content::{BlobDescriptor, ContentStore, Sha256Digest};
use rish_layer::{
    ApplyOptions, ApplyReport, LayerFormat, LayerLimits, SpecialFilePolicy, apply_layer_with_format,
};
use rish_registry::{Descriptor, Digest, MediaType};

const MAX_SNAPSHOT_ID_BYTES: usize = 128;

/// Resource and special-file policy applied independently to every layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotOptions {
    pub layer_limits: LayerLimits,
    pub special_files: SpecialFilePolicy,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            layer_limits: LayerLimits::default(),
            special_files: SpecialFilePolicy::Reject,
        }
    }
}

/// A rootfs tree that was successfully published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedSnapshot {
    pub path: PathBuf,
    pub layers: Vec<ApplyReport>,
}

struct PreparedLayer {
    digest: Sha256Digest,
    size: u64,
    diff_id: String,
    format: LayerFormat,
}

/// Materialize ordered OCI layers into `CONTENT_STORE/snapshots/SNAPSHOT_ID`.
///
/// Only explicit OCI tar, OCI gzip, and Docker gzip layer media types are
/// accepted. In particular, zstd, foreign/nondistributable, octet-stream, and
/// extension media types fail closed.
pub fn materialize_snapshot(
    content_store: &ContentStore,
    layer_descriptors: &[Descriptor],
    diff_ids: &[Digest],
    snapshot_id: &str,
    options: &SnapshotOptions,
) -> Result<MaterializedSnapshot, SnapshotError> {
    validate_snapshot_id(snapshot_id)?;
    if layer_descriptors.len() != diff_ids.len() {
        return Err(SnapshotError::LayerCountMismatch {
            descriptors: layer_descriptors.len(),
            diff_ids: diff_ids.len(),
        });
    }

    let prepared = prepare_layers(layer_descriptors, diff_ids)?;
    let snapshots = prepare_snapshots_directory(content_store.root())?;
    let target = snapshots.join(snapshot_id);
    reject_existing_target(&target)?;

    // Keep every blob reachable while verify/open/apply runs. This protects
    // against GC through clones of this ContentStore.
    let lease = content_store
        .create_lease()
        .map_err(|source| SnapshotError::Content { index: 0, source })?;
    for (index, layer) in prepared.iter().enumerate() {
        lease
            .add(layer.digest)
            .map_err(|source| SnapshotError::Content { index, source })?;
        content_store
            .verify(BlobDescriptor::new(layer.digest, layer.size))
            .map_err(|source| SnapshotError::Content { index, source })?;
    }

    let staging = tempfile::Builder::new()
        .prefix(".rish-stage-")
        .tempdir_in(&snapshots)?;
    make_private(staging.path())?;
    ensure_real_directory(staging.path(), SnapshotError::UnsafeSnapshotsDirectory)?;

    let mut reports = Vec::with_capacity(prepared.len());
    for (index, layer) in prepared.iter().enumerate() {
        let blob = content_store
            .open_blob(layer.digest)
            .map_err(|source| SnapshotError::Content { index, source })?;
        let apply_options = ApplyOptions {
            limits: options.layer_limits.clone(),
            special_files: options.special_files,
            expected_diff_id: Some(layer.diff_id.clone()),
            spool_directory: Some(snapshots.clone()),
        };
        let report = apply_layer_with_format(blob, staging.path(), layer.format, &apply_options)
            .map_err(|source| SnapshotError::Apply { index, source })?;
        reports.push(report);
    }

    reject_existing_target(&target)?;
    publish_no_replace(staging.path(), &target)?;
    let _former_staging_path = staging.keep();

    Ok(MaterializedSnapshot {
        path: target,
        layers: reports,
    })
}

fn prepare_layers(
    descriptors: &[Descriptor],
    diff_ids: &[Digest],
) -> Result<Vec<PreparedLayer>, SnapshotError> {
    descriptors
        .iter()
        .zip(diff_ids)
        .enumerate()
        .map(|(index, (descriptor, diff_id))| {
            descriptor
                .validate()
                .map_err(|source| SnapshotError::InvalidDescriptor { index, source })?;
            let format = layer_format(index, &descriptor.media_type)?;
            if descriptor.digest.algorithm() != Sha256Digest::ALGORITHM {
                return Err(SnapshotError::UnsupportedContentDigest {
                    index,
                    algorithm: descriptor.digest.algorithm().to_owned(),
                });
            }
            if diff_id.algorithm() != Sha256Digest::ALGORITHM {
                return Err(SnapshotError::UnsupportedDiffId {
                    index,
                    algorithm: diff_id.algorithm().to_owned(),
                });
            }

            let digest = Sha256Digest::from_encoded(descriptor.digest.encoded())
                .expect("registry SHA-256 digests have already been validated");
            Ok(PreparedLayer {
                digest,
                size: descriptor.size,
                diff_id: diff_id.to_string(),
                format,
            })
        })
        .collect()
}

fn layer_format(index: usize, media_type: &MediaType) -> Result<LayerFormat, SnapshotError> {
    match media_type {
        MediaType::OciImageLayer => Ok(LayerFormat::Tar),
        MediaType::OciImageLayerGzip | MediaType::DockerLayerGzip => Ok(LayerFormat::Gzip),
        _ => Err(SnapshotError::UnsupportedLayerMediaType {
            index,
            media_type: media_type.clone(),
        }),
    }
}

fn validate_snapshot_id(snapshot_id: &str) -> Result<(), SnapshotError> {
    let bytes = snapshot_id.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_SNAPSHOT_ID_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(SnapshotError::InvalidSnapshotId(snapshot_id.to_owned()))
    }
}

fn prepare_snapshots_directory(root: &Path) -> Result<PathBuf, SnapshotError> {
    ensure_real_directory(root, SnapshotError::UnsafeStoreRoot)?;
    let snapshots = root.join("snapshots");
    match fs::create_dir(&snapshots) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    ensure_real_directory(&snapshots, SnapshotError::UnsafeSnapshotsDirectory)?;
    Ok(snapshots)
}

fn ensure_real_directory(
    path: &Path,
    error: impl FnOnce(PathBuf) -> SnapshotError,
) -> Result<(), SnapshotError> {
    let metadata = fs::symlink_metadata(path).map_err(SnapshotError::Io)?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(error(path.to_owned()))
    }
}

fn reject_existing_target(target: &Path) -> Result<(), SnapshotError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(SnapshotError::UnsafeSnapshotTarget(target.to_owned()))
        }
        Ok(_) => Err(SnapshotError::SnapshotAlreadyExists(target.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn make_private(path: &Path) -> Result<(), SnapshotError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> Result<(), SnapshotError> {
    Ok(())
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn publish_no_replace(source: &Path, target: &Path) -> Result<(), SnapshotError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    match renameat_with(CWD, source, CWD, target, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(error) => {
            let error = io::Error::from(error);
            if error.kind() == io::ErrorKind::AlreadyExists {
                reject_existing_target(target)
            } else if error.kind() == io::ErrorKind::Unsupported {
                Err(SnapshotError::AtomicPublishUnavailable(error))
            } else {
                Err(SnapshotError::Publish(error))
            }
        }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn publish_no_replace(_source: &Path, _target: &Path) -> Result<(), SnapshotError> {
    Err(SnapshotError::AtomicPublishUnavailable(io::Error::new(
        io::ErrorKind::Unsupported,
        "the host has no supported atomic no-replace directory rename",
    )))
}

#[cfg(test)]
mod tests;
