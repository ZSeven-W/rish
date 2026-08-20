use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tar::{Entry, EntryType};

use crate::path::{Whiteout, encoded_len, normalize_entry_path, path_from_bytes, whiteout_for};
use crate::{ImportError, ImportLimits, PrivilegedGuestPolicy};

const SCHILY_XATTR: &[u8] = b"SCHILY.xattr.";
#[derive(Clone, Debug)]
pub(crate) struct ExtendedAttribute {
    pub(crate) name: Vec<u8>,
    pub(crate) value: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct EntryMetadata {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) mode: u32,
    pub(crate) mtime: Timestamp,
    pub(crate) xattrs: Vec<ExtendedAttribute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Timestamp {
    pub(crate) seconds: i64,
    pub(crate) nanoseconds: i64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LayerScan {
    pub(crate) entries: u64,
    pub(crate) regular_bytes: u64,
    pub(crate) whiteouts: Vec<Whiteout>,
    pub(crate) device_nodes: u64,
    pub(crate) fifos: u64,
}

pub(crate) fn scan_layer(
    file: &mut File,
    limits: &ImportLimits,
    privilege: PrivilegedGuestPolicy,
) -> Result<LayerScan, ImportError> {
    validate_raw_headers(file, limits)?;
    file.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(file);
    let mut scan = LayerScan::default();
    let mut xattr_bytes = 0_u64;
    let mut metadata_bytes = 0_u64;

    for entry in archive.entries()? {
        let mut entry = entry?;
        scan.entries = scan
            .entries
            .checked_add(1)
            .ok_or(ImportError::LayerEntryLimitExceeded {
                limit: limits.max_entries_per_layer,
            })?;
        if scan.entries > limits.max_entries_per_layer {
            return Err(ImportError::LayerEntryLimitExceeded {
                limit: limits.max_entries_per_layer,
            });
        }

        let path = entry_path(&entry, limits)?;
        add_metadata_bytes(&mut metadata_bytes, encoded_len(&path), limits)?;
        let entry_type = entry.header().entry_type();
        let metadata = entry_metadata(&mut entry, &path, limits, &mut xattr_bytes)?;
        for xattr in &metadata.xattrs {
            add_metadata_bytes(
                &mut metadata_bytes,
                xattr.name.len().saturating_add(xattr.value.len()),
                limits,
            )?;
        }
        validate_capability_target(entry_type, &metadata, &path)?;
        if let Some(whiteout) = whiteout_for(&path)? {
            validate_whiteout(&entry, &path, &metadata)?;
            scan.whiteouts.push(whiteout);
            continue;
        }
        if path.as_os_str().is_empty() && !entry_type.is_dir() {
            return Err(ImportError::InvalidPath {
                path,
                reason: "empty non-directory path",
            });
        }

        if entry_type.is_file() || entry_type.is_contiguous() {
            let size = entry.size();
            if size > limits.max_file_bytes {
                return Err(ImportError::FileLimitExceeded {
                    path,
                    actual: size,
                    limit: limits.max_file_bytes,
                });
            }
            scan.regular_bytes = scan.regular_bytes.checked_add(size).ok_or(
                ImportError::RegularFileTotalLimitExceeded {
                    limit: limits.max_regular_file_bytes_total,
                },
            )?;
            if scan.regular_bytes > limits.max_regular_file_bytes_total {
                return Err(ImportError::RegularFileTotalLimitExceeded {
                    limit: limits.max_regular_file_bytes_total,
                });
            }
        } else if entry_type.is_dir() {
            if entry.size() != 0 {
                return Err(ImportError::UnsupportedEntryType {
                    path,
                    entry_type: entry_type.as_byte(),
                });
            }
        } else if entry_type.is_symlink() {
            validate_zero_sized_link(&entry, &path)?;
            let target = link_target(&entry, &path, limits)?;
            add_metadata_bytes(&mut metadata_bytes, encoded_len(&target), limits)?;
            if target.as_os_str().is_empty() {
                return Err(ImportError::InvalidPath {
                    path,
                    reason: "empty symbolic-link target",
                });
            }
            if metadata.mode != 0o777 {
                return Err(ImportError::UnsupportedSymlinkMode {
                    path,
                    mode: metadata.mode,
                });
            }
        } else if entry_type.is_hard_link() {
            validate_zero_sized_link(&entry, &path)?;
            let target = normalize_entry_path(&link_target(&entry, &path, limits)?)?;
            add_metadata_bytes(&mut metadata_bytes, encoded_len(&target), limits)?;
            if target.components().count() > limits.max_path_components {
                return Err(ImportError::PathDepthLimitExceeded {
                    limit: limits.max_path_components,
                });
            }
            if target.as_os_str().is_empty() {
                return Err(ImportError::InvalidHardlinkTarget { path, target });
            }
            if !metadata.xattrs.is_empty() {
                return Err(ImportError::UnsupportedHardlinkMetadata(path));
            }
        } else if entry_type.is_gnu_sparse() {
            return Err(ImportError::SparseEntryRejected(path));
        } else if entry_type.is_character_special() || entry_type.is_block_special() {
            if !privilege.allow_device_nodes {
                return Err(ImportError::DevicePolicyRequired(path));
            }
            validate_special_size(&entry, &path)?;
            device_numbers(&entry, &path)?;
            scan.device_nodes += 1;
        } else if entry_type.is_fifo() {
            if !privilege.allow_fifos {
                return Err(ImportError::FifoPolicyRequired(path));
            }
            validate_special_size(&entry, &path)?;
            scan.fifos += 1;
        } else {
            return Err(ImportError::UnsupportedEntryType {
                path,
                entry_type: entry_type.as_byte(),
            });
        }
    }
    Ok(scan)
}

fn add_metadata_bytes(
    total: &mut u64,
    added: usize,
    limits: &ImportLimits,
) -> Result<(), ImportError> {
    *total = (*total)
        .checked_add(u64::try_from(added).unwrap_or(u64::MAX))
        .ok_or(ImportError::MetadataLayerLimitExceeded {
            limit: limits.max_metadata_bytes_per_layer,
        })?;
    if *total > limits.max_metadata_bytes_per_layer {
        return Err(ImportError::MetadataLayerLimitExceeded {
            limit: limits.max_metadata_bytes_per_layer,
        });
    }
    Ok(())
}

fn validate_raw_headers(file: &mut File, limits: &ImportLimits) -> Result<(), ImportError> {
    file.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(file);
    let mut extensions = 0_u64;
    for entry in archive.entries()?.raw(true) {
        let entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            return Err(ImportError::GlobalPaxRejected);
        }
        if kind.is_pax_local_extensions() {
            extensions = count_extension(extensions, limits)?;
            if entry.size() > limits.max_pax_header_bytes {
                return Err(ImportError::PaxHeaderLimitExceeded {
                    actual: entry.size(),
                    limit: limits.max_pax_header_bytes,
                });
            }
        } else if kind.is_gnu_longname() {
            extensions = count_extension(extensions, limits)?;
            let maximum = u64::try_from(limits.max_path_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            if entry.size() > maximum {
                return Err(ImportError::PathLimitExceeded {
                    limit: limits.max_path_bytes,
                });
            }
        } else if kind.is_gnu_longlink() {
            extensions = count_extension(extensions, limits)?;
            let maximum = u64::try_from(limits.max_link_target_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            if entry.size() > maximum {
                return Err(ImportError::LinkTargetLimitExceeded {
                    path: PathBuf::from("<gnu-longlink>"),
                    limit: limits.max_link_target_bytes,
                });
            }
        } else if kind.is_gnu_sparse() {
            return Err(ImportError::SparseEntryRejected(entry_path(
                &entry, limits,
            )?));
        }
    }
    Ok(())
}

fn count_extension(current: u64, limits: &ImportLimits) -> Result<u64, ImportError> {
    let count = current
        .checked_add(1)
        .ok_or(ImportError::ExtensionHeaderLimitExceeded {
            limit: limits.max_extension_headers_per_layer,
        })?;
    if count > limits.max_extension_headers_per_layer {
        return Err(ImportError::ExtensionHeaderLimitExceeded {
            limit: limits.max_extension_headers_per_layer,
        });
    }
    Ok(count)
}

pub(crate) fn entry_path<R: Read>(
    entry: &Entry<'_, R>,
    limits: &ImportLimits,
) -> Result<PathBuf, ImportError> {
    let raw = path_from_bytes(entry.path_bytes().as_ref())?;
    if encoded_len(&raw) > limits.max_path_bytes {
        return Err(ImportError::PathLimitExceeded {
            limit: limits.max_path_bytes,
        });
    }
    let normalized = normalize_entry_path(&raw)?;
    if normalized.components().count() > limits.max_path_components {
        return Err(ImportError::PathDepthLimitExceeded {
            limit: limits.max_path_components,
        });
    }
    Ok(normalized)
}

pub(crate) fn link_target<R: Read>(
    entry: &Entry<'_, R>,
    path: &Path,
    limits: &ImportLimits,
) -> Result<PathBuf, ImportError> {
    let target = entry
        .link_name_bytes()
        .ok_or_else(|| ImportError::MissingLinkTarget(path.to_owned()))?;
    if target.len() > limits.max_link_target_bytes {
        return Err(ImportError::LinkTargetLimitExceeded {
            path: path.to_owned(),
            limit: limits.max_link_target_bytes,
        });
    }
    path_from_bytes(target.as_ref())
}

pub(crate) fn entry_metadata<R: Read>(
    entry: &mut Entry<'_, R>,
    path: &Path,
    limits: &ImportLimits,
    layer_xattr_bytes: &mut u64,
) -> Result<EntryMetadata, ImportError> {
    let uid = u32::try_from(entry.header().uid()?)
        .map_err(|_| ImportError::IdOutOfRange(path.to_owned()))?;
    let gid = u32::try_from(entry.header().gid()?)
        .map_err(|_| ImportError::IdOutOfRange(path.to_owned()))?;
    let mode = entry.header().mode()? & 0o7777;
    let header_mtime = i64::try_from(entry.header().mtime()?)
        .map_err(|_| ImportError::TimestampOutOfRange(path.to_owned()))?;
    let pax = read_pax_metadata(entry, path, limits, layer_xattr_bytes)?;
    Ok(EntryMetadata {
        uid,
        gid,
        mode,
        mtime: pax.mtime.unwrap_or(Timestamp {
            seconds: header_mtime,
            nanoseconds: 0,
        }),
        xattrs: pax.xattrs,
    })
}

pub(crate) fn device_numbers<R: Read>(
    entry: &Entry<'_, R>,
    path: &Path,
) -> Result<(u32, u32), ImportError> {
    let major = entry
        .header()
        .device_major()?
        .ok_or_else(|| ImportError::MissingDeviceNumber(path.to_owned()))?;
    let minor = entry
        .header()
        .device_minor()?
        .ok_or_else(|| ImportError::MissingDeviceNumber(path.to_owned()))?;
    Ok((major, minor))
}

fn validate_whiteout<R: Read>(
    entry: &Entry<'_, R>,
    path: &Path,
    metadata: &EntryMetadata,
) -> Result<(), ImportError> {
    let kind = entry.header().entry_type();
    let regular = kind.is_file() || kind.is_contiguous();
    let overlay_device = if kind.is_character_special() {
        matches!(device_numbers(entry, path), Ok((0, 0)))
    } else {
        false
    };
    if entry.size() != 0 || (!regular && !overlay_device) || !metadata.xattrs.is_empty() {
        return Err(ImportError::InvalidWhiteout(path.to_owned()));
    }
    Ok(())
}

fn validate_zero_sized_link<R: Read>(entry: &Entry<'_, R>, path: &Path) -> Result<(), ImportError> {
    if entry.size() != 0 {
        return Err(ImportError::UnsupportedEntryType {
            path: path.to_owned(),
            entry_type: entry.header().entry_type().as_byte(),
        });
    }
    Ok(())
}

fn validate_special_size<R: Read>(entry: &Entry<'_, R>, path: &Path) -> Result<(), ImportError> {
    if entry.size() != 0 {
        return Err(ImportError::UnsupportedEntryType {
            path: path.to_owned(),
            entry_type: entry.header().entry_type().as_byte(),
        });
    }
    Ok(())
}

struct PaxMetadata {
    xattrs: Vec<ExtendedAttribute>,
    mtime: Option<Timestamp>,
}

fn read_pax_metadata<R: Read>(
    entry: &mut Entry<'_, R>,
    path: &Path,
    limits: &ImportLimits,
    layer_bytes: &mut u64,
) -> Result<PaxMetadata, ImportError> {
    let mut result = PaxMetadata {
        xattrs: Vec::new(),
        mtime: None,
    };
    let mut names = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let Some(extensions) = entry.pax_extensions()? else {
        return Ok(result);
    };
    for extension in extensions {
        let extension = extension?;
        let key = extension.key_bytes();
        if !keys.insert(key.to_vec()) {
            return Err(ImportError::DuplicatePaxKey {
                path: path.to_owned(),
                key: key.to_vec(),
            });
        }
        if let Some(name) = key.strip_prefix(SCHILY_XATTR) {
            if name.starts_with(b"trusted.overlay.") || name.starts_with(b"user.overlay.") {
                return Err(ImportError::UnsupportedMetadataEncoding {
                    path: path.to_owned(),
                    key: key.to_vec(),
                });
            }
            if name.is_empty() || name.contains(&0) || name.contains(&b'/') || !name.contains(&b'.')
            {
                return Err(ImportError::InvalidXattrName(path.to_owned()));
            }
            if name.len() > limits.max_xattr_name_bytes {
                return Err(ImportError::XattrNameLimitExceeded {
                    path: path.to_owned(),
                    limit: limits.max_xattr_name_bytes,
                });
            }
            let value = extension.value_bytes();
            if value.len() > limits.max_xattr_value_bytes {
                return Err(ImportError::XattrValueLimitExceeded {
                    path: path.to_owned(),
                    limit: limits.max_xattr_value_bytes,
                });
            }
            if !names.insert(name.to_vec()) {
                return Err(ImportError::DuplicateXattr {
                    path: path.to_owned(),
                    name: name.to_vec(),
                });
            }
            if name == b"security.capability" {
                validate_file_capability(value, path)?;
            }
            if u64::try_from(result.xattrs.len()).unwrap_or(u64::MAX) >= limits.max_xattrs_per_entry
            {
                return Err(ImportError::XattrCountLimitExceeded {
                    path: path.to_owned(),
                    limit: limits.max_xattrs_per_entry,
                });
            }
            let added = u64::try_from(name.len().saturating_add(value.len())).unwrap_or(u64::MAX);
            *layer_bytes =
                layer_bytes
                    .checked_add(added)
                    .ok_or(ImportError::XattrLayerLimitExceeded {
                        limit: limits.max_xattr_bytes_per_layer,
                    })?;
            if *layer_bytes > limits.max_xattr_bytes_per_layer {
                return Err(ImportError::XattrLayerLimitExceeded {
                    limit: limits.max_xattr_bytes_per_layer,
                });
            }
            result.xattrs.push(ExtendedAttribute {
                name: name.to_vec(),
                value: value.to_vec(),
            });
        } else if key == b"mtime" {
            result.mtime = Some(parse_timestamp(extension.value_bytes(), path, key)?);
        } else if matches!(key, b"atime" | b"ctime") {
            return Err(ImportError::UnsupportedMetadataEncoding {
                path: path.to_owned(),
                key: key.to_vec(),
            });
        } else if matches!(key, b"uid" | b"gid" | b"size")
            && !valid_pax_u64(extension.value_bytes())
        {
            return Err(ImportError::InvalidPaxNumeric {
                path: path.to_owned(),
                key: key.to_vec(),
            });
        } else if unsupported_metadata_key(key) {
            return Err(ImportError::UnsupportedMetadataEncoding {
                path: path.to_owned(),
                key: key.to_vec(),
            });
        }
    }
    Ok(result)
}

fn parse_timestamp(value: &[u8], path: &Path, key: &[u8]) -> Result<Timestamp, ImportError> {
    let invalid = || ImportError::InvalidTimestamp {
        path: path.to_owned(),
        key: key.to_vec(),
    };
    let value = std::str::from_utf8(value).map_err(|_| invalid())?;
    let (negative, unsigned) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
        Some(_) => return Err(invalid()),
        None => (unsigned, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    let whole = whole.parse::<u128>().map_err(|_| invalid())?;
    let (kept_fraction, remainder) = if fraction.len() > 9 {
        fraction.split_at(9)
    } else {
        (fraction, "")
    };
    if remainder.bytes().any(|byte| byte != b'0') {
        return Err(invalid());
    }
    let fraction_value = if kept_fraction.is_empty() {
        0
    } else {
        kept_fraction.parse::<u32>().map_err(|_| invalid())?
    };
    let nanoseconds = fraction_value
        .checked_mul(10_u32.pow((9 - kept_fraction.len()) as u32))
        .ok_or_else(invalid)?;
    let whole = i128::try_from(whole).map_err(|_| invalid())?;
    let (seconds, nanoseconds) = if negative && nanoseconds != 0 {
        (
            whole
                .checked_neg()
                .and_then(|value| value.checked_sub(1))
                .ok_or_else(invalid)?,
            1_000_000_000_i64 - i64::from(nanoseconds),
        )
    } else if negative {
        (whole.checked_neg().ok_or_else(invalid)?, 0)
    } else {
        (whole, i64::from(nanoseconds))
    };
    Ok(Timestamp {
        seconds: i64::try_from(seconds).map_err(|_| invalid())?,
        nanoseconds,
    })
}

fn validate_capability_target(
    entry_type: EntryType,
    metadata: &EntryMetadata,
    path: &Path,
) -> Result<(), ImportError> {
    let has_capability = metadata
        .xattrs
        .iter()
        .any(|xattr| xattr.name == b"security.capability");
    if has_capability && !(entry_type.is_file() || entry_type.is_contiguous()) {
        return Err(ImportError::UnsupportedCapabilityTarget {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn validate_file_capability(value: &[u8], path: &Path) -> Result<(), ImportError> {
    const VFS_CAP_REVISION_2: u32 = 0x0200_0000;
    const VFS_CAP_REVISION_MASK: u32 = 0xff00_0000;
    const VFS_CAP_FLAGS_EFFECTIVE: u32 = 0x0000_0001;
    if value.len() != 20 {
        return Err(ImportError::InvalidFileCapability {
            path: path.to_owned(),
            reason: "only the 20-byte Linux V2 wire format is supported",
        });
    }
    let magic = u32::from_le_bytes(value[..4].try_into().expect("four-byte slice"));
    if magic & VFS_CAP_REVISION_MASK != VFS_CAP_REVISION_2 {
        return Err(ImportError::InvalidFileCapability {
            path: path.to_owned(),
            reason: "unknown or unmapped capability revision",
        });
    }
    if magic & !(VFS_CAP_REVISION_MASK | VFS_CAP_FLAGS_EFFECTIVE) != 0 {
        return Err(ImportError::InvalidFileCapability {
            path: path.to_owned(),
            reason: "reserved capability flags are set",
        });
    }
    Ok(())
}

fn unsupported_metadata_key(key: &[u8]) -> bool {
    !matches!(
        key,
        b"path"
            | b"linkpath"
            | b"size"
            | b"uid"
            | b"gid"
            | b"uname"
            | b"gname"
            | b"charset"
            | b"comment"
            | b"hdrcharset"
    )
}

fn valid_pax_u64(value: &[u8]) -> bool {
    !value.is_empty()
        && value.iter().all(u8::is_ascii_digit)
        && std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .is_some()
}

pub(crate) fn is_special(entry_type: EntryType) -> bool {
    entry_type.is_character_special() || entry_type.is_block_special() || entry_type.is_fifo()
}
