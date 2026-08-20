use std::path::Path;

use rish_pull::VerifiedImageRecord;

use crate::{DescriptorSource, ImportError, ImportOptions, ImportReport};

#[cfg(target_os = "linux")]
pub fn import_verified_image<S: DescriptorSource>(
    record: &VerifiedImageRecord,
    source: &mut S,
    destination: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<ImportReport, ImportError> {
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    use rish_pull::VERIFIED_IMAGE_RECORD_SCHEMA_VERSION;

    use crate::archive::scan_layer;
    use crate::linux::{
        apply_layer, initialize_rootfs, publish_noreplace, require_guest_root, resolve_destination,
        sync_tree,
    };
    use crate::verify::{preflight_layer, verify_and_expand};

    if record.schema_version != VERIFIED_IMAGE_RECORD_SCHEMA_VERSION {
        return Err(ImportError::UnsupportedRecordSchema(record.schema_version));
    }
    if record.platform.os != "linux" {
        return Err(ImportError::NonLinuxImage(record.platform.os.clone()));
    }
    let layer_count = u64::try_from(record.layers.len()).unwrap_or(u64::MAX);
    if layer_count > options.limits.max_layers {
        return Err(ImportError::LayerLimitExceeded {
            actual: layer_count,
            limit: options.limits.max_layers,
        });
    }
    let mut declared_compressed = 0_u64;
    for layer in &record.layers {
        preflight_layer(layer, &options.limits)?;
        declared_compressed = declared_compressed
            .checked_add(layer.descriptor.size)
            .ok_or(ImportError::CompressedTotalLimitExceeded {
                limit: options.limits.max_compressed_total_bytes,
            })?;
        if declared_compressed > options.limits.max_compressed_total_bytes {
            return Err(ImportError::CompressedTotalLimitExceeded {
                limit: options.limits.max_compressed_total_bytes,
            });
        }
    }

    require_guest_root()?;
    let destination = resolve_destination(destination.as_ref())?;
    let parent = destination
        .parent()
        .ok_or_else(|| ImportError::InvalidDestination(destination.clone()))?;
    let staging = tempfile::Builder::new()
        .prefix(".rish-import-")
        .tempdir_in(parent)
        .map_err(|error| ImportError::filesystem(parent, error))?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))
        .map_err(|error| ImportError::filesystem(staging.path(), error))?;
    let work = staging.path().join("work");
    fs::create_dir(&work).map_err(|error| ImportError::filesystem(&work, error))?;
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700))
        .map_err(|error| ImportError::filesystem(&work, error))?;
    let rootfs = staging.path().join("rootfs");
    initialize_rootfs(&rootfs)?;

    let mut report = ImportReport {
        rootfs: destination.clone(),
        layers: 0,
        compressed_bytes: 0,
        uncompressed_bytes: 0,
        entries: 0,
        regular_file_bytes: 0,
        whiteouts: 0,
        device_nodes: 0,
        fifos: 0,
    };
    for layer in &record.layers {
        let reader = source.open(&layer.descriptor)?;
        let mut verified = verify_and_expand(reader, layer, &work, &options.limits)?;
        report.compressed_bytes = checked_total(
            report.compressed_bytes,
            verified.compressed_bytes,
            options.limits.max_compressed_total_bytes,
            ImportError::CompressedTotalLimitExceeded {
                limit: options.limits.max_compressed_total_bytes,
            },
        )?;
        report.uncompressed_bytes = checked_total(
            report.uncompressed_bytes,
            verified.uncompressed_bytes,
            options.limits.max_uncompressed_total_bytes,
            ImportError::UncompressedTotalLimitExceeded {
                limit: options.limits.max_uncompressed_total_bytes,
            },
        )?;
        let scan = scan_layer(&mut verified.tar, &options.limits, options.privileged_guest)?;
        report.entries = checked_total(
            report.entries,
            scan.entries,
            options.limits.max_entries_total,
            ImportError::TotalEntryLimitExceeded {
                limit: options.limits.max_entries_total,
            },
        )?;
        report.regular_file_bytes = checked_total(
            report.regular_file_bytes,
            scan.regular_bytes,
            options.limits.max_regular_file_bytes_total,
            ImportError::RegularFileTotalLimitExceeded {
                limit: options.limits.max_regular_file_bytes_total,
            },
        )?;
        apply_layer(&mut verified.tar, &rootfs, &scan, options)?;
        report.layers += 1;
        report.whiteouts += u64::try_from(scan.whiteouts.len()).unwrap_or(u64::MAX);
        report.device_nodes += scan.device_nodes;
        report.fifos += scan.fifos;
    }

    sync_tree(&rootfs)?;
    let staging_directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(staging.path())
        .map_err(|error| ImportError::filesystem(staging.path(), error))?;
    staging_directory
        .sync_all()
        .map_err(|error| ImportError::filesystem(staging.path(), error))?;
    publish_noreplace(&rootfs, &destination)?;
    Ok(report)
}

#[cfg(target_os = "linux")]
fn checked_total(
    current: u64,
    added: u64,
    limit: u64,
    error: ImportError,
) -> Result<u64, ImportError> {
    let Some(total) = current.checked_add(added) else {
        return Err(error);
    };
    if total > limit {
        return Err(error);
    }
    Ok(total)
}

#[cfg(not(target_os = "linux"))]
pub fn import_verified_image<S: DescriptorSource>(
    _record: &VerifiedImageRecord,
    _source: &mut S,
    _destination: impl AsRef<Path>,
    _options: &ImportOptions,
) -> Result<ImportReport, ImportError> {
    Err(ImportError::UnsupportedPlatform)
}
