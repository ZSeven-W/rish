use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::archive::{
    EntryMetadata, LayerScan, Timestamp, device_numbers, entry_metadata, entry_path, is_special,
    link_target,
};
use crate::path::{Whiteout, normalize_entry_path, whiteout_for};
use crate::{ImportError, ImportOptions};

pub(crate) fn require_guest_root() -> Result<(), ImportError> {
    // SAFETY: geteuid has no preconditions and does not mutate memory.
    if unsafe { libc::geteuid() } != 0 {
        return Err(ImportError::GuestRootRequired);
    }
    Ok(())
}

pub(crate) fn resolve_destination(destination: &Path) -> Result<PathBuf, ImportError> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| ImportError::InvalidDestination(destination.to_owned()))?;
    let name = destination
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ImportError::InvalidDestination(destination.to_owned()))?;
    let metadata =
        fs::symlink_metadata(parent).map_err(|error| ImportError::filesystem(parent, error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(ImportError::UnsafeRootfsParent(parent.to_owned()));
    }
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| ImportError::filesystem(parent, error))?;
    let canonical_metadata = fs::symlink_metadata(&canonical_parent)
        .map_err(|error| ImportError::filesystem(&canonical_parent, error))?;
    if !canonical_metadata.is_dir()
        || canonical_metadata.file_type().is_symlink()
        || canonical_metadata.uid() != 0
        || canonical_metadata.mode() & 0o022 != 0
    {
        return Err(ImportError::UnsafeRootfsParent(canonical_parent));
    }
    let resolved = canonical_parent.join(name);
    match fs::symlink_metadata(&resolved) {
        Ok(_) => Err(ImportError::DestinationExists(resolved)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(ImportError::filesystem(resolved, error)),
    }
}

pub(crate) fn initialize_rootfs(root: &Path) -> Result<(), ImportError> {
    fs::create_dir(root).map_err(|error| ImportError::filesystem(root, error))?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o755))
        .map_err(|error| ImportError::filesystem(root, error))
}

pub(crate) fn apply_layer(
    file: &mut File,
    root: &Path,
    scan: &LayerScan,
    options: &ImportOptions,
) -> Result<(), ImportError> {
    apply_whiteouts(root, &scan.whiteouts)?;
    file.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(file);
    let mut directory_metadata = BTreeMap::<PathBuf, EntryMetadata>::new();
    let mut copied_bytes = 0_u64;
    let mut xattr_bytes = 0_u64;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry_path(&entry, &options.limits)?;
        if whiteout_for(&path)?.is_some() {
            continue;
        }
        let kind = entry.header().entry_type();
        let metadata = entry_metadata(&mut entry, &path, &options.limits, &mut xattr_bytes)?;
        if !kind.is_dir() {
            let replaced = root.join(&path);
            directory_metadata.retain(|directory, _| !directory.starts_with(&replaced));
        }
        if kind.is_dir() {
            let destination = ensure_directory(root, &path)?;
            directory_metadata.insert(destination, metadata);
        } else if kind.is_file() || kind.is_contiguous() {
            let destination = destination_leaf(root, &path)?;
            let expected_size = entry.size();
            let copied = write_regular(&mut entry, &destination, &path, expected_size, &metadata)?;
            copied_bytes = copied_bytes.checked_add(copied).ok_or(
                ImportError::RegularFileTotalLimitExceeded {
                    limit: options.limits.max_regular_file_bytes_total,
                },
            )?;
        } else if kind.is_symlink() {
            let target = link_target(&entry, &path, &options.limits)?;
            let destination = destination_leaf(root, &path)?;
            remove_no_follow(&destination)?;
            std::os::unix::fs::symlink(&target, &destination)
                .map_err(|error| ImportError::filesystem(&destination, error))?;
            apply_symlink_metadata(&destination, &metadata)?;
        } else if kind.is_hard_link() {
            let target = normalize_entry_path(&link_target(&entry, &path, &options.limits)?)?;
            apply_hardlink(root, &path, &target)?;
        } else if is_special(kind) {
            apply_special(root, &path, &entry, &metadata, options)?;
        }
    }
    if copied_bytes != scan.regular_bytes {
        return Err(ImportError::LayerByteCountMismatch {
            expected: scan.regular_bytes,
            actual: copied_bytes,
        });
    }
    let mut directories = directory_metadata.into_iter().collect::<Vec<_>>();
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, metadata) in directories {
        let actual =
            fs::symlink_metadata(&path).map_err(|error| ImportError::filesystem(&path, error))?;
        if !actual.is_dir() || actual.file_type().is_symlink() {
            return Err(ImportError::ParentNotDirectory(path));
        }
        apply_path_metadata(&path, &metadata, false)?;
    }
    Ok(())
}

fn apply_whiteouts(root: &Path, whiteouts: &[Whiteout]) -> Result<(), ImportError> {
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
                let name = path
                    .file_name()
                    .ok_or_else(|| ImportError::MalformedWhiteout(path.clone()))?;
                remove_no_follow(&parent.join(name))?;
            }
            Whiteout::Opaque(path) => {
                let Some(directory) = resolve_existing_directory(root, path)? else {
                    continue;
                };
                for child in fs::read_dir(&directory)
                    .map_err(|error| ImportError::filesystem(&directory, error))?
                {
                    let child =
                        child.map_err(|error| ImportError::filesystem(&directory, error))?;
                    remove_no_follow(&child.path())?;
                }
            }
        }
    }
    Ok(())
}

fn ensure_directory(root: &Path, path: &Path) -> Result<PathBuf, ImportError> {
    if path.as_os_str().is_empty() {
        return Ok(root.to_owned());
    }
    let parent = ensure_parents(root, path.parent().unwrap_or_else(|| Path::new("")))?;
    let leaf = path.file_name().ok_or_else(|| ImportError::InvalidPath {
        path: path.to_owned(),
        reason: "directory has no basename",
    })?;
    let destination = parent.join(leaf);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            remove_no_follow(&destination)?;
            create_implicit_directory(&destination)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_implicit_directory(&destination)?;
        }
        Err(error) => return Err(ImportError::filesystem(&destination, error)),
    }
    Ok(destination)
}

fn ensure_parents(root: &Path, path: &Path) -> Result<PathBuf, ImportError> {
    let mut current = root.to_owned();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ImportError::SymlinkPathComponent {
                    path: path.to_owned(),
                    component: current,
                });
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(ImportError::ParentNotDirectory(path.to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_implicit_directory(&current)?;
            }
            Err(error) => return Err(ImportError::filesystem(&current, error)),
        }
    }
    Ok(current)
}

fn create_implicit_directory(path: &Path) -> Result<(), ImportError> {
    fs::create_dir(path).map_err(|error| ImportError::filesystem(path, error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| ImportError::filesystem(path, error))
}

fn destination_leaf(root: &Path, path: &Path) -> Result<PathBuf, ImportError> {
    let parent = ensure_parents(root, path.parent().unwrap_or_else(|| Path::new("")))?;
    let name = path.file_name().ok_or_else(|| ImportError::InvalidPath {
        path: path.to_owned(),
        reason: "entry has no basename",
    })?;
    Ok(parent.join(name))
}

fn write_regular(
    entry: &mut impl Read,
    destination: &Path,
    archive_path: &Path,
    expected_size: u64,
    metadata: &EntryMetadata,
) -> Result<u64, ImportError> {
    let parent = destination
        .parent()
        .ok_or_else(|| ImportError::InvalidPath {
            path: archive_path.to_owned(),
            reason: "regular file has no parent",
        })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| ImportError::filesystem(parent, error))?;
    let copied = io::copy(entry, temporary.as_file_mut())?;
    if copied != expected_size {
        return Err(ImportError::TruncatedFile {
            path: archive_path.to_owned(),
            expected: expected_size,
            actual: copied,
        });
    }
    apply_file_metadata(temporary.as_file(), metadata)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| ImportError::filesystem(temporary.path(), error))?;
    remove_no_follow(destination)?;
    temporary
        .persist(destination)
        .map_err(|error| ImportError::filesystem(destination, error.error))?;
    Ok(copied)
}

fn apply_hardlink(root: &Path, path: &Path, target: &Path) -> Result<(), ImportError> {
    if path == target {
        return Err(ImportError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        });
    }
    let Some(target_parent) =
        resolve_existing_directory(root, target.parent().unwrap_or_else(|| Path::new("")))?
    else {
        return Err(ImportError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        });
    };
    let source = target_parent.join(target.file_name().ok_or_else(|| {
        ImportError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        }
    })?);
    let source_metadata =
        fs::symlink_metadata(&source).map_err(|_| ImportError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        })?;
    if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
        return Err(ImportError::InvalidHardlinkTarget {
            path: path.to_owned(),
            target: target.to_owned(),
        });
    }
    let destination = destination_leaf(root, path)?;
    remove_no_follow(&destination)?;
    fs::hard_link(&source, &destination)
        .map_err(|error| ImportError::filesystem(&destination, error))
}

fn apply_special<R: Read>(
    root: &Path,
    path: &Path,
    entry: &tar::Entry<'_, R>,
    metadata: &EntryMetadata,
    options: &ImportOptions,
) -> Result<(), ImportError> {
    let kind = entry.header().entry_type();
    let destination = destination_leaf(root, path)?;
    remove_no_follow(&destination)?;
    if kind.is_fifo() {
        if !options.privileged_guest.allow_fifos {
            return Err(ImportError::FifoPolicyRequired(path.to_owned()));
        }
        let cpath = c_string(destination.as_os_str())?;
        // SAFETY: cpath is NUL-terminated and points to valid memory.
        let result = unsafe { libc::mkfifo(cpath.as_ptr(), metadata.mode as libc::mode_t) };
        cvt_path(result, &destination)?;
    } else {
        if !options.privileged_guest.allow_device_nodes {
            return Err(ImportError::DevicePolicyRequired(path.to_owned()));
        }
        let (major, minor) = device_numbers(entry, path)?;
        let file_type = if kind.is_character_special() {
            libc::S_IFCHR
        } else {
            libc::S_IFBLK
        };
        let cpath = c_string(destination.as_os_str())?;
        let device = libc::makedev(major, minor);
        // SAFETY: cpath is NUL-terminated and mode/device are validated values.
        let result = unsafe {
            libc::mknod(
                cpath.as_ptr(),
                file_type | metadata.mode as libc::mode_t,
                device,
            )
        };
        cvt_path(result, &destination)?;
    }
    apply_path_metadata(&destination, metadata, false)
}

fn resolve_existing_directory(root: &Path, path: &Path) -> Result<Option<PathBuf>, ImportError> {
    let mut current = root.to_owned();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ImportError::filesystem(&current, error)),
        };
        if metadata.file_type().is_symlink() {
            return Err(ImportError::SymlinkPathComponent {
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

fn remove_no_follow(path: &Path) -> Result<(), ImportError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|error| ImportError::filesystem(path, error))
        }
        Ok(_) => fs::remove_file(path).map_err(|error| ImportError::filesystem(path, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ImportError::filesystem(path, error)),
    }
}

fn apply_file_metadata(file: &File, metadata: &EntryMetadata) -> Result<(), ImportError> {
    let fd = file.as_raw_fd();
    // SAFETY: fd is live and uid/gid were range-checked.
    cvt(unsafe { libc::fchown(fd, metadata.uid, metadata.gid) })?;
    // SAFETY: fd is live and mode contains only permission/special bits.
    cvt(unsafe { libc::fchmod(fd, metadata.mode as libc::mode_t) })?;
    for xattr in &metadata.xattrs {
        let name = CString::new(xattr.name.as_slice())
            .map_err(|_| ImportError::InvalidXattrName(PathBuf::from("<open-file>")))?;
        // SAFETY: fd is live; name/value buffers remain valid for the call.
        cvt(unsafe {
            libc::fsetxattr(
                fd,
                name.as_ptr(),
                xattr.value.as_ptr().cast(),
                xattr.value.len(),
                0,
            )
        })?;
    }
    set_file_mtime(fd, metadata.mtime)
}

fn apply_path_metadata(
    path: &Path,
    metadata: &EntryMetadata,
    symlink: bool,
) -> Result<(), ImportError> {
    let cpath = c_string(path.as_os_str())?;
    // SAFETY: cpath is NUL-terminated and uid/gid were range-checked.
    cvt_path(
        unsafe { libc::lchown(cpath.as_ptr(), metadata.uid, metadata.gid) },
        path,
    )?;
    if !symlink {
        // SAFETY: the private staging tree was checked not to contain a symlink here.
        cvt_path(
            unsafe { libc::chmod(cpath.as_ptr(), metadata.mode as libc::mode_t) },
            path,
        )?;
    }
    for xattr in &metadata.xattrs {
        let name = CString::new(xattr.name.as_slice())
            .map_err(|_| ImportError::InvalidXattrName(path.to_owned()))?;
        let result = if symlink {
            // SAFETY: buffers remain valid and lsetxattr does not follow the link.
            unsafe {
                libc::lsetxattr(
                    cpath.as_ptr(),
                    name.as_ptr(),
                    xattr.value.as_ptr().cast(),
                    xattr.value.len(),
                    0,
                )
            }
        } else {
            // SAFETY: the private staging tree was checked not to contain a symlink here.
            unsafe {
                libc::setxattr(
                    cpath.as_ptr(),
                    name.as_ptr(),
                    xattr.value.as_ptr().cast(),
                    xattr.value.len(),
                    0,
                )
            }
        };
        cvt_path(result, path)?;
    }
    set_path_mtime(&cpath, path, metadata.mtime)
}

fn apply_symlink_metadata(path: &Path, metadata: &EntryMetadata) -> Result<(), ImportError> {
    apply_path_metadata(path, metadata, true)
}

fn set_file_mtime(fd: libc::c_int, mtime: Timestamp) -> Result<(), ImportError> {
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        timestamp_to_timespec(mtime),
    ];
    // SAFETY: fd is live and times points to two initialized timespec values.
    cvt(unsafe { libc::futimens(fd, times.as_ptr()) })
}

fn set_path_mtime(cpath: &CString, path: &Path, mtime: Timestamp) -> Result<(), ImportError> {
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        timestamp_to_timespec(mtime),
    ];
    // SAFETY: cpath and times remain valid; AT_SYMLINK_NOFOLLOW avoids link traversal.
    cvt_path(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                cpath.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        },
        path,
    )
}

const fn timestamp_to_timespec(timestamp: Timestamp) -> libc::timespec {
    libc::timespec {
        tv_sec: timestamp.seconds,
        tv_nsec: timestamp.nanoseconds,
    }
}

pub(crate) fn sync_tree(root: &Path) -> Result<(), ImportError> {
    let metadata =
        fs::symlink_metadata(root).map_err(|error| ImportError::filesystem(root, error))?;
    if metadata.file_type().is_symlink() {
        // Symlink contents live in the directory entry; the containing
        // directory fsync below makes that entry durable. Never open/follow it.
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(root).map_err(|error| ImportError::filesystem(root, error))? {
            let entry = entry.map_err(|error| ImportError::filesystem(root, error))?;
            sync_tree(&entry.path())?;
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(|error| ImportError::filesystem(root, error))?;
        directory
            .sync_all()
            .map_err(|error| ImportError::filesystem(root, error))?;
    } else if metadata.is_file() {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root)
            .map_err(|error| ImportError::filesystem(root, error))?;
        file.sync_all()
            .map_err(|error| ImportError::filesystem(root, error))?;
    }
    Ok(())
}

pub(crate) fn publish_noreplace(source: &Path, destination: &Path) -> Result<(), ImportError> {
    let source_c = c_string(source.as_os_str())?;
    let destination_c = c_string(destination.as_os_str())?;
    // SAFETY: both paths are NUL-terminated. renameat2 owns no passed memory.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        let parent = destination
            .parent()
            .ok_or_else(|| ImportError::InvalidDestination(destination.to_owned()))?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)
            .map_err(|error| ImportError::filesystem(parent, error))?;
        directory
            .sync_all()
            .map_err(|error| ImportError::filesystem(parent, error))?;
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EEXIST) => Err(ImportError::DestinationExists(destination.to_owned())),
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => {
            Err(ImportError::AtomicPublishUnsupported)
        }
        _ => Err(ImportError::filesystem(destination, error)),
    }
}

fn c_string(value: &OsStr) -> Result<CString, ImportError> {
    CString::new(value.as_bytes()).map_err(|_| ImportError::InvalidPath {
        path: PathBuf::from(value),
        reason: "NUL byte",
    })
}

fn cvt(result: libc::c_int) -> Result<(), ImportError> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().into())
    }
}

fn cvt_path(result: libc::c_int, path: &Path) -> Result<(), ImportError> {
    if result == 0 {
        Ok(())
    } else {
        Err(ImportError::filesystem(path, io::Error::last_os_error()))
    }
}
