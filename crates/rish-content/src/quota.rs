use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::digest::Sha256Digest;
use crate::error::{Result, StoreError};

pub(crate) fn scan_committed_blobs(root: &Path) -> Result<(BTreeMap<Sha256Digest, u64>, u64)> {
    let mut committed_blobs = BTreeMap::new();
    let mut committed_bytes = 0_u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            return Err(StoreError::UnsafeFilesystemEntry {
                path,
                reason: "unexpected non-file in blob directory".to_owned(),
            });
        }
        let encoded = entry
            .file_name()
            .to_str()
            .ok_or_else(|| StoreError::UnsafeFilesystemEntry {
                path: path.clone(),
                reason: "blob filename is not UTF-8".to_owned(),
            })?
            .to_owned();
        let digest = Sha256Digest::from_encoded(&encoded).map_err(|error| {
            StoreError::UnsafeFilesystemEntry {
                path: path.clone(),
                reason: format!("invalid blob filename: {error}"),
            }
        })?;
        committed_bytes = committed_bytes.checked_add(metadata.len()).ok_or_else(|| {
            StoreError::UnsafeFilesystemEntry {
                path: root.to_owned(),
                reason: "committed blob sizes exceed the u64 accounting range".to_owned(),
            }
        })?;
        committed_blobs.insert(digest, metadata.len());
    }
    Ok((committed_blobs, committed_bytes))
}
