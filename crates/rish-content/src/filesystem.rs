use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, StoreError};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn ensure_managed_directory(base: &Path, path: &Path) -> Result<()> {
    let relative = managed_relative_path(base, path)?;
    let mut current = base.to_owned();
    assert_real_directory(&current)?;
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(StoreError::UnsafeFilesystemEntry {
                    path: current,
                    reason: "managed path is not a real directory".to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
        assert_real_directory(&current)?;
        set_private_directory_permissions(&current)?;
    }
    Ok(())
}

pub(crate) fn ensure_optional_managed_directory(base: &Path, path: &Path) -> Result<()> {
    let relative = managed_relative_path(base, path)?;
    let mut current = base.to_owned();
    assert_real_directory(&current)?;
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                set_private_directory_permissions(&current)?;
            }
            Ok(_) => {
                return Err(StoreError::UnsafeFilesystemEntry {
                    path: current,
                    reason: "managed path is not a real directory".to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn ensure_regular_marker(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(StoreError::UnsafeFilesystemEntry {
            path: path.to_owned(),
            reason: "pin marker is not a regular file".to_owned(),
        })
    }
}

pub(crate) fn remove_directory_if_empty(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn create_temporary_file(directory: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..128 {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(".ingest-{}-{timestamp}-{sequence}", std::process::id());
        let path = directory.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                set_private_file_permissions(&file)?;
                return Ok((path, file));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique content-store temporary file",
    )
    .into())
}

pub(crate) fn prepare_store_root(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(StoreError::UnsafeFilesystemEntry {
                path: root.to_owned(),
                reason: "content-store root is not a real directory".to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(root)?,
        Err(error) => return Err(error.into()),
    }
    assert_real_directory(root)?;
    set_private_directory_permissions(root)
}

fn managed_relative_path<'a>(base: &'a Path, path: &'a Path) -> Result<&'a Path> {
    path.strip_prefix(base)
        .map_err(|_| StoreError::UnsafeFilesystemEntry {
            path: path.to_owned(),
            reason: "managed directory escapes its base".to_owned(),
        })
}

fn assert_real_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(StoreError::UnsafeFilesystemEntry {
            path: path.to_owned(),
            reason: "managed path is not a real directory".to_owned(),
        })
    }
}

#[cfg(unix)]
pub(crate) fn open_regular_file_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn open_regular_file_no_follow(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_private_file_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn set_private_file_permissions(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
