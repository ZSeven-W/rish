use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use flate2::read::MultiGzDecoder;
use rish_pull::VerifiedImageLayer;
use rish_registry::MediaType;
use sha2::{Digest as _, Sha256};

use crate::{ImportError, ImportLimits};

pub(crate) struct VerifiedLayerSpool {
    pub(crate) tar: File,
    pub(crate) compressed_bytes: u64,
    pub(crate) uncompressed_bytes: u64,
}

pub(crate) fn verify_and_expand<R: Read>(
    mut reader: R,
    layer: &VerifiedImageLayer,
    work_directory: &Path,
    limits: &ImportLimits,
) -> Result<VerifiedLayerSpool, ImportError> {
    preflight_layer(layer, limits)?;

    let mut compressed = tempfile::tempfile_in(work_directory)
        .map_err(|error| ImportError::filesystem(work_directory, error))?;
    let (compressed_bytes, actual_digest) =
        copy_descriptor(&mut reader, &mut compressed, layer.descriptor.size)?;
    let expected_digest = layer.descriptor.digest.to_string();
    if actual_digest != expected_digest {
        return Err(ImportError::DescriptorDigestMismatch {
            expected: expected_digest,
            actual: actual_digest,
        });
    }
    compressed.flush()?;
    compressed.seek(SeekFrom::Start(0))?;

    let mut tar = tempfile::tempfile_in(work_directory)
        .map_err(|error| ImportError::filesystem(work_directory, error))?;
    let (uncompressed_bytes, diff_id) = match layer.descriptor.media_type {
        MediaType::OciImageLayerGzip | MediaType::DockerLayerGzip => {
            let mut decoder = MultiGzDecoder::new(compressed);
            copy_uncompressed(&mut decoder, &mut tar, limits.max_uncompressed_layer_bytes)?
        }
        MediaType::OciImageLayer => copy_uncompressed(
            &mut compressed,
            &mut tar,
            limits.max_uncompressed_layer_bytes,
        )?,
        MediaType::DockerForeignLayerGzip => return Err(ImportError::ForeignLayerRejected),
        ref media_type => {
            return Err(ImportError::UnsupportedLayerMediaType(
                media_type.to_string(),
            ));
        }
    };
    let expected_diff_id = layer.diff_id.to_string();
    if diff_id != expected_diff_id {
        return Err(ImportError::DiffIdMismatch {
            expected: expected_diff_id,
            actual: diff_id,
        });
    }
    tar.flush()?;
    tar.seek(SeekFrom::Start(0))?;
    Ok(VerifiedLayerSpool {
        tar,
        compressed_bytes,
        uncompressed_bytes,
    })
}

pub(crate) fn preflight_layer(
    layer: &VerifiedImageLayer,
    limits: &ImportLimits,
) -> Result<(), ImportError> {
    let descriptor = &layer.descriptor;
    if descriptor.digest.algorithm() != "sha256" {
        return Err(ImportError::UnsupportedDigestAlgorithm(
            descriptor.digest.algorithm().to_owned(),
        ));
    }
    if descriptor.size > limits.max_compressed_layer_bytes {
        return Err(ImportError::CompressedLayerLimitExceeded {
            actual: descriptor.size,
            limit: limits.max_compressed_layer_bytes,
        });
    }
    match descriptor.media_type {
        MediaType::OciImageLayer | MediaType::OciImageLayerGzip | MediaType::DockerLayerGzip => {
            Ok(())
        }
        MediaType::DockerForeignLayerGzip => return Err(ImportError::ForeignLayerRejected),
        ref other => Err(ImportError::UnsupportedLayerMediaType(other.to_string())),
    }?;
    if layer.diff_id.algorithm() != "sha256" {
        return Err(ImportError::UnsupportedDiffIdAlgorithm(
            layer.diff_id.algorithm().to_owned(),
        ));
    }
    Ok(())
}

fn copy_descriptor(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected_size: u64,
) -> Result<(u64, String), ImportError> {
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(ImportError::DescriptorSizeMismatch {
                expected: expected_size,
                actual: u64::MAX,
            })?;
        if total > expected_size {
            return Err(ImportError::DescriptorSizeMismatch {
                expected: expected_size,
                actual: total,
            });
        }
        hasher.update(&buffer[..count]);
        writer.write_all(&buffer[..count])?;
    }
    if total != expected_size {
        return Err(ImportError::DescriptorSizeMismatch {
            expected: expected_size,
            actual: total,
        });
    }
    Ok((total, format!("sha256:{:x}", hasher.finalize())))
}

fn copy_uncompressed(
    reader: &mut impl Read,
    writer: &mut impl Write,
    limit: u64,
) -> Result<(u64, String), ImportError> {
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(ImportError::UncompressedLayerLimitExceeded { limit })?;
        if total > limit {
            return Err(ImportError::UncompressedLayerLimitExceeded { limit });
        }
        hasher.update(&buffer[..count]);
        writer.write_all(&buffer[..count])?;
    }
    Ok((total, format!("sha256:{:x}", hasher.finalize())))
}
