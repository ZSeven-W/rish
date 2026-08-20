use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::VerifiedImageRecordError;

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn ensure_private_directory(
    base: &Path,
    path: &Path,
) -> Result<(), VerifiedImageRecordError> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| invalid_entry(path, "managed path escapes content-store root"))?;
    assert_directory(base)?;
    let mut current = base.to_owned();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(invalid_entry(
                    &current,
                    "managed path is not a real directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
        assert_directory(&current)?;
        set_private_directory_permissions(&current)?;
    }
    Ok(())
}

pub(super) fn read_bounded_regular_file(
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>, VerifiedImageRecordError> {
    let mut file = open_regular_file_no_follow(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            VerifiedImageRecordError::MissingLocator
        } else {
            error.into()
        }
    })?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid_entry(path, "record entry is not a regular file"));
    }
    if metadata.len() > maximum {
        return Err(VerifiedImageRecordError::LocatorTooLarge { maximum });
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(VerifiedImageRecordError::LocatorTooLarge { maximum });
    }
    Ok(bytes)
}

pub(super) fn atomic_write_private(
    directory: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), VerifiedImageRecordError> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(invalid_entry(
                destination,
                "record locator is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let (temporary_path, mut temporary) = create_temporary_file(directory)?;
    let guard = TemporaryGuard(temporary_path.clone());
    temporary.write_all(bytes)?;
    temporary.sync_all()?;
    drop(temporary);
    fs::rename(&temporary_path, destination)?;
    sync_directory(directory)?;
    drop(guard);
    Ok(())
}

fn create_temporary_file(directory: &Path) -> Result<(PathBuf, File), VerifiedImageRecordError> {
    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = directory.join(format!(
            ".locator-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
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
        "could not allocate a verified-image temporary file",
    )
    .into())
}

fn assert_directory(path: &Path) -> Result<(), VerifiedImageRecordError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(invalid_entry(path, "managed path is not a real directory"))
    }
}

fn invalid_entry(path: &Path, reason: &str) -> VerifiedImageRecordError {
    io::Error::other(format!(
        "unsafe verified-image entry at {}: {reason}",
        path.display()
    ))
    .into()
}

#[cfg(unix)]
fn open_regular_file_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_regular_file_no_follow(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), VerifiedImageRecordError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), VerifiedImageRecordError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(file: &File) -> Result<(), VerifiedImageRecordError> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_file: &File) -> Result<(), VerifiedImageRecordError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), VerifiedImageRecordError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), VerifiedImageRecordError> {
    Ok(())
}

struct TemporaryGuard(PathBuf);

impl Drop for TemporaryGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
