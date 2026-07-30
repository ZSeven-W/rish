use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;
use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};

use crate::LayerError;
use crate::path::{
    Whiteout, encoded_len, normalize_entry_path, path_from_tar_bytes, validate_symlink_target,
    whiteout_target,
};

/// Compression format of an incoming layer blob.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LayerFormat {
    /// Detect gzip from its magic bytes; otherwise treat the input as tar.
    #[default]
    Auto,
    Tar,
    Gzip,
}

/// Handling for device nodes and FIFOs.
///
/// Skipping is explicit because creating host devices from image metadata is
/// never safe or portable. A privileged VM backend may materialize them later.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpecialFilePolicy {
    #[default]
    Reject,
    Skip,
}

/// Resource limits enforced before the destination is changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerLimits {
    pub max_entries: u64,
    pub max_file_size: u64,
    pub max_total_size: u64,
    pub max_archive_size: u64,
    pub max_path_bytes: usize,
    pub max_link_target_bytes: usize,
}

impl Default for LayerLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_file_size: 512 * 1024 * 1024,
            max_total_size: 2 * 1024 * 1024 * 1024,
            max_archive_size: 3 * 1024 * 1024 * 1024,
            max_path_bytes: 4_096,
            max_link_target_bytes: 4_096,
        }
    }
}

/// Configuration for applying one layer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyOptions {
    pub limits: LayerLimits,
    pub special_files: SpecialFilePolicy,
    /// Optional OCI `rootfs.diff_ids` value expected for this uncompressed tar.
    pub expected_diff_id: Option<String>,
    /// App-owned directory used for the bounded decompressed spool.
    ///
    /// Mobile callers should always set this to an application cache/staging
    /// directory because the process-global temporary directory may not be
    /// writable inside an iOS, Android, or Harmony sandbox.
    pub spool_directory: Option<PathBuf>,
}

/// Auditable totals for a successfully applied layer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyReport {
    /// Canonical digest of the uncompressed layer tar (`sha256:<lower-hex>`).
    pub diff_id: String,
    pub entries: u64,
    pub expanded_bytes: u64,
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub hardlinks: u64,
    pub whiteouts: u64,
    pub skipped_special_files: u64,
}

/// Apply a plain-tar or gzip-compressed OCI layer, selected by magic bytes.
pub fn apply_layer<R: Read>(
    reader: R,
    destination: impl AsRef<Path>,
    options: &ApplyOptions,
) -> Result<ApplyReport, LayerError> {
    apply_layer_with_format(reader, destination, LayerFormat::Auto, options)
}

/// Apply an OCI layer using an explicitly selected input format.
pub fn apply_layer_with_format<R: Read>(
    reader: R,
    destination: impl AsRef<Path>,
    format: LayerFormat,
    options: &ApplyOptions,
) -> Result<ApplyReport, LayerError> {
    let (mut spool, diff_id) = spool_archive(
        reader,
        format,
        options.limits.max_archive_size,
        options.spool_directory.as_deref(),
    )?;
    if let Some(expected) = options.expected_diff_id.as_deref() {
        let expected = normalize_diff_id(expected)?;
        if expected != diff_id {
            return Err(LayerError::DiffIdMismatch {
                expected,
                actual: diff_id,
            });
        }
    }

    let mut scan = scan_archive(&mut spool, options)?;
    scan.report.diff_id = diff_id;
    let root = prepare_destination(destination.as_ref())?;

    apply_whiteouts(&root, &scan.whiteouts)?;
    spool.seek(SeekFrom::Start(0))?;
    apply_entries(&mut spool, &root, options, &scan.report)?;
    Ok(scan.report)
}

struct Scan {
    report: ApplyReport,
    whiteouts: Vec<Whiteout>,
}

fn spool_archive<R: Read>(
    reader: R,
    format: LayerFormat,
    limit: u64,
    spool_directory: Option<&Path>,
) -> Result<(File, String), LayerError> {
    let mut reader = BufReader::new(reader);
    let gzip = match format {
        LayerFormat::Auto => reader.fill_buf()?.starts_with(&[0x1f, 0x8b]),
        LayerFormat::Tar => false,
        LayerFormat::Gzip => true,
    };
    let mut spool = if let Some(directory) = spool_directory {
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(LayerError::InvalidSpoolDirectory(directory.to_owned()));
        }
        tempfile::tempfile_in(directory)?
    } else {
        tempfile::tempfile()?
    };
    let digest = if gzip {
        let mut decoder = MultiGzDecoder::new(reader);
        copy_bounded(&mut decoder, &mut spool, limit)?
    } else {
        copy_bounded(&mut reader, &mut spool, limit)?
    };
    spool.flush()?;
    spool.seek(SeekFrom::Start(0))?;
    Ok((spool, format!("sha256:{digest:x}")))
}

fn copy_bounded(
    reader: &mut impl Read,
    writer: &mut impl Write,
    limit: u64,
) -> Result<sha2::digest::Output<Sha256>, LayerError> {
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finalize());
        }
        total = total
            .checked_add(read as u64)
            .ok_or(LayerError::ArchiveSizeLimitExceeded { limit })?;
        if total > limit {
            return Err(LayerError::ArchiveSizeLimitExceeded { limit });
        }
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
    }
}

fn normalize_diff_id(value: &str) -> Result<String, LayerError> {
    let Some((algorithm, encoded)) = value.split_once(':') else {
        return Err(LayerError::InvalidDiffId(value.to_owned()));
    };
    if !algorithm.eq_ignore_ascii_case("sha256")
        || encoded.len() != 64
        || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(LayerError::InvalidDiffId(value.to_owned()));
    }
    Ok(format!("sha256:{}", encoded.to_ascii_lowercase()))
}

fn scan_archive(file: &mut File, options: &ApplyOptions) -> Result<Scan, LayerError> {
    file.seek(SeekFrom::Start(0))?;
    let mut archive = Archive::new(file);
    let mut report = ApplyReport::default();
    let mut whiteouts = Vec::new();

    for entry in archive.entries()? {
        let entry = entry?;
        report.entries = report
            .entries
            .checked_add(1)
            .ok_or(LayerError::EntryLimitExceeded {
                limit: options.limits.max_entries,
            })?;
        if report.entries > options.limits.max_entries {
            return Err(LayerError::EntryLimitExceeded {
                limit: options.limits.max_entries,
            });
        }

        let raw_path = path_from_tar_bytes(entry.path_bytes().as_ref())?;
        if encoded_len(&raw_path) > options.limits.max_path_bytes {
            return Err(LayerError::PathTooLong {
                limit: options.limits.max_path_bytes,
            });
        }
        let path = normalize_entry_path(&raw_path)?;
        let entry_type = entry.header().entry_type();

        if let Some(whiteout) = whiteout_target(&path)? {
            if entry.size() != 0 {
                return Err(LayerError::WhiteoutHasData(path));
            }
            report.whiteouts += 1;
            whiteouts.push(whiteout);
            continue;
        }
        if path.as_os_str().is_empty() && !entry_type.is_dir() {
            return Err(LayerError::InvalidPath {
                path: raw_path,
                reason: "empty non-directory path",
            });
        }

        if entry_type.is_file() || entry_type.is_contiguous() {
            validate_regular(&path, entry.size(), &mut report, &options.limits)?;
            entry.header().mode()?;
        } else if entry_type.is_dir() {
            report.directories += 1;
            entry.header().mode()?;
        } else if entry_type.is_symlink() {
            let target = read_link_target(&entry, &path, options)?;
            validate_symlink_target(&path, &target)?;
            report.symlinks += 1;
        } else if entry_type.is_hard_link() {
            let target = read_link_target(&entry, &path, options)?;
            let target = normalize_entry_path(&target)?;
            if target.as_os_str().is_empty() {
                return Err(LayerError::InvalidHardlinkTarget { path, target });
            }
            report.hardlinks += 1;
        } else if is_special(entry_type) {
            match options.special_files {
                SpecialFilePolicy::Reject => {
                    return Err(LayerError::SpecialFileRejected {
                        path,
                        entry_type: entry_type.as_byte(),
                    });
                }
                SpecialFilePolicy::Skip => report.skipped_special_files += 1,
            }
        } else {
            return Err(LayerError::UnsupportedEntryType {
                path,
                entry_type: entry_type.as_byte(),
            });
        }
    }
    Ok(Scan { report, whiteouts })
}

fn validate_regular(
    path: &Path,
    size: u64,
    report: &mut ApplyReport,
    limits: &LayerLimits,
) -> Result<(), LayerError> {
    if size > limits.max_file_size {
        return Err(LayerError::FileSizeLimitExceeded {
            path: path.to_owned(),
            size,
            limit: limits.max_file_size,
        });
    }
    report.expanded_bytes =
        report
            .expanded_bytes
            .checked_add(size)
            .ok_or(LayerError::TotalSizeLimitExceeded {
                limit: limits.max_total_size,
            })?;
    if report.expanded_bytes > limits.max_total_size {
        return Err(LayerError::TotalSizeLimitExceeded {
            limit: limits.max_total_size,
        });
    }
    report.files += 1;
    Ok(())
}

fn read_link_target<R: Read>(
    entry: &tar::Entry<'_, R>,
    path: &Path,
    options: &ApplyOptions,
) -> Result<PathBuf, LayerError> {
    let bytes = entry
        .link_name_bytes()
        .ok_or_else(|| LayerError::MissingLinkTarget(path.to_owned()))?;
    if bytes.len() > options.limits.max_link_target_bytes {
        return Err(LayerError::LinkTargetTooLong {
            path: path.to_owned(),
            limit: options.limits.max_link_target_bytes,
        });
    }
    path_from_tar_bytes(bytes.as_ref())
}

fn is_special(entry_type: EntryType) -> bool {
    entry_type.is_character_special() || entry_type.is_block_special() || entry_type.is_fifo()
}

fn prepare_destination(destination: &Path) -> Result<PathBuf, LayerError> {
    fs::create_dir_all(destination)?;
    let metadata = fs::symlink_metadata(destination)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(LayerError::InvalidDestination(destination.to_owned()));
    }
    Ok(fs::canonicalize(destination)?)
}

fn apply_whiteouts(root: &Path, whiteouts: &[Whiteout]) -> Result<(), LayerError> {
    for whiteout in whiteouts {
        match whiteout {
            Whiteout::Remove(path) => {
                let Some(parent) = resolve_existing_directory(
                    root,
                    path.parent().unwrap_or_else(|| Path::new("")),
                )?
                else {
                    continue;
                };
                let Some(name) = path.file_name() else {
                    return Err(LayerError::MalformedWhiteout(path.clone()));
                };
                remove_path_no_follow(&parent.join(name))?;
            }
            Whiteout::Opaque(path) => {
                let Some(directory) = resolve_existing_directory(root, path)? else {
                    continue;
                };
                for child in fs::read_dir(directory)? {
                    remove_path_no_follow(&child?.path())?;
                }
            }
        }
    }
    Ok(())
}

fn apply_entries(
    file: &mut File,
    root: &Path,
    options: &ApplyOptions,
    expected: &ApplyReport,
) -> Result<(), LayerError> {
    let mut archive = Archive::new(file);
    let mut directory_modes = Vec::new();
    let mut copied_bytes = 0_u64;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = path_from_tar_bytes(entry.path_bytes().as_ref())?;
        let path = normalize_entry_path(&raw_path)?;
        if whiteout_target(&path)?.is_some() {
            continue;
        }
        let entry_type = entry.header().entry_type();
        if is_special(entry_type) && options.special_files == SpecialFilePolicy::Skip {
            continue;
        }

        if entry_type.is_dir() {
            let destination = if path.as_os_str().is_empty() {
                root.to_owned()
            } else {
                let parent =
                    ensure_directories(root, path.parent().unwrap_or_else(|| Path::new("")))?;
                let destination =
                    parent.join(path.file_name().ok_or_else(|| LayerError::InvalidPath {
                        path: path.clone(),
                        reason: "directory has no basename",
                    })?);
                ensure_directory_leaf(&destination)?;
                destination
            };
            directory_modes.push((
                destination,
                sanitized_directory_mode(entry.header().mode()?),
            ));
        } else if entry_type.is_file() || entry_type.is_contiguous() {
            let parent = ensure_directories(root, path.parent().unwrap_or_else(|| Path::new("")))?;
            let destination =
                parent.join(path.file_name().ok_or_else(|| LayerError::InvalidPath {
                    path: path.clone(),
                    reason: "file has no basename",
                })?);
            let mode = sanitized_mode(entry.header().mode()?);
            let copied = write_regular_file(&mut entry, &destination, &path, mode)?;
            copied_bytes =
                copied_bytes
                    .checked_add(copied)
                    .ok_or(LayerError::TotalSizeLimitExceeded {
                        limit: options.limits.max_total_size,
                    })?;
        } else if entry_type.is_symlink() {
            let target = read_link_target(&entry, &path, options)?;
            let parent = ensure_directories(root, path.parent().unwrap_or_else(|| Path::new("")))?;
            let destination =
                parent.join(path.file_name().ok_or_else(|| LayerError::InvalidPath {
                    path: path.clone(),
                    reason: "symbolic link has no basename",
                })?);
            replace_with_symlink(&destination, &target)?;
        } else if entry_type.is_hard_link() {
            let target = normalize_entry_path(&read_link_target(&entry, &path, options)?)?;
            apply_hardlink(root, &path, &target)?;
        }
    }

    if copied_bytes != expected.expanded_bytes {
        return Err(LayerError::TruncatedEntry {
            path: PathBuf::from("<layer-total>"),
            expected: expected.expanded_bytes,
            actual: copied_bytes,
        });
    }
    directory_modes.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, mode) in directory_modes {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            set_mode(&path, mode)?;
        }
    }
    Ok(())
}

fn resolve_existing_directory(root: &Path, path: &Path) -> Result<Option<PathBuf>, LayerError> {
    let mut current = root.to_owned();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(LayerError::SymlinkPathComponent {
                path: path.to_owned(),
                component: current,
            });
        }
        if !metadata.is_dir() {
            return Ok(None);
        }
    }
    Ok(Some(current))
}

fn ensure_directories(root: &Path, path: &Path) -> Result<PathBuf, LayerError> {
    let mut current = root.to_owned();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LayerError::SymlinkPathComponent {
                    path: path.to_owned(),
                    component: current,
                });
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(LayerError::ParentNotDirectory(path.to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

fn ensure_directory_leaf(path: &Path) -> Result<(), LayerError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => {
            remove_path_no_follow(path)?;
            fs::create_dir(path)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn write_regular_file(
    entry: &mut impl Read,
    destination: &Path,
    archive_path: &Path,
    mode: u32,
) -> Result<u64, LayerError> {
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(destination)?;
        }
    }

    let parent = destination
        .parent()
        .ok_or_else(|| LayerError::InvalidPath {
            path: archive_path.to_owned(),
            reason: "file has no parent",
        })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let copied = io::copy(entry, temporary.as_file_mut())?;
    set_file_mode(temporary.as_file(), mode)?;
    temporary.flush()?;
    temporary
        .persist(destination)
        .map_err(|error| LayerError::Io(error.error))?;
    Ok(copied)
}

fn apply_hardlink(root: &Path, path: &Path, target: &Path) -> Result<(), LayerError> {
    let target_parent =
        resolve_existing_directory(root, target.parent().unwrap_or_else(|| Path::new("")))?;
    let Some(target_parent) = target_parent else {
        return Err(LayerError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        });
    };
    let source = target_parent.join(target.file_name().ok_or_else(|| {
        LayerError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        }
    })?);
    let metadata =
        fs::symlink_metadata(&source).map_err(|_| LayerError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(LayerError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        });
    }

    let parent = ensure_directories(root, path.parent().unwrap_or_else(|| Path::new("")))?;
    let destination =
        parent.join(
            path.file_name()
                .ok_or_else(|| LayerError::InvalidHardlinkTarget {
                    path: path.to_owned(),
                    target: target.to_owned(),
                })?,
        );
    remove_path_no_follow(&destination)?;
    fs::hard_link(source, destination)?;
    Ok(())
}

fn remove_path_no_follow(path: &Path) -> Result<(), LayerError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn replace_with_symlink(destination: &Path, target: &Path) -> Result<(), LayerError> {
    remove_path_no_follow(destination)?;
    create_symlink(target, destination)
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path) -> Result<(), LayerError> {
    std::os::unix::fs::symlink(target, destination)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _destination: &Path) -> Result<(), LayerError> {
    Err(LayerError::SymlinksUnsupported)
}

fn sanitized_mode(mode: u32) -> u32 {
    // Sticky is required for directories such as /tmp. setuid/setgid are
    // deliberately stripped on portable host snapshots; VM root filesystems
    // must be unpacked inside the Linux guest to retain privileged metadata.
    mode & 0o1777
}

fn sanitized_directory_mode(mode: u32) -> u32 {
    // Layer application and failed-staging cleanup run as the mobile app user,
    // not Linux root. Keep every directory traversable/writable by that owner
    // across later layers. Full Linux ownership/mode fidelity belongs to the
    // in-guest unpacker.
    sanitized_mode(mode) | 0o700
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), LayerError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), LayerError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: u32) -> Result<(), LayerError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_file: &File, _mode: u32) -> Result<(), LayerError> {
    Ok(())
}
