use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::LayerError;

pub(crate) fn path_from_tar_bytes(bytes: &[u8]) -> Result<PathBuf, LayerError> {
    if bytes.contains(&0) {
        return Err(LayerError::InvalidPath {
            path: PathBuf::new(),
            reason: "NUL byte",
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
    }

    #[cfg(not(unix))]
    {
        let text = std::str::from_utf8(bytes).map_err(|_| LayerError::InvalidPath {
            path: PathBuf::new(),
            reason: "non-UTF-8 path is unsupported on this host",
        })?;
        Ok(PathBuf::from(text))
    }
}

pub(crate) fn normalize_entry_path(path: &Path) -> Result<PathBuf, LayerError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(LayerError::InvalidPath {
                    path: path.to_owned(),
                    reason: "parent-directory component",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(LayerError::InvalidPath {
                    path: path.to_owned(),
                    reason: "absolute path",
                });
            }
        }
    }
    Ok(normalized)
}

pub(crate) fn validate_symlink_target(path: &Path, target: &Path) -> Result<(), LayerError> {
    if target.as_os_str().is_empty() || target.is_absolute() {
        return Err(LayerError::SymlinkEscape {
            path: path.to_owned(),
            target: target.to_owned(),
        });
    }

    let mut depth = path
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        match component {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LayerError::SymlinkEscape {
                    path: path.to_owned(),
                    target: target.to_owned(),
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn encoded_len(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

pub(crate) fn whiteout_target(path: &Path) -> Result<Option<Whiteout>, LayerError> {
    const PREFIX: &[u8] = b".wh.";
    const OPAQUE: &[u8] = b".wh..wh..opq";

    let Some(name) = path.file_name() else {
        return Ok(None);
    };
    let name = name.as_encoded_bytes();
    if name == OPAQUE {
        return Ok(Some(Whiteout::Opaque(
            path.parent().unwrap_or_else(|| Path::new("")).to_owned(),
        )));
    }
    let Some(target_name) = name.strip_prefix(PREFIX) else {
        return Ok(None);
    };
    if target_name.is_empty() || target_name == b"." || target_name == b".." {
        return Err(LayerError::MalformedWhiteout(path.to_owned()));
    }

    let target_name = path_from_tar_bytes(target_name)?;
    let mut target = path.parent().unwrap_or_else(|| Path::new("")).to_owned();
    target.push(target_name);
    Ok(Some(Whiteout::Remove(target)))
}

#[derive(Clone, Debug)]
pub(crate) enum Whiteout {
    Remove(PathBuf),
    Opaque(PathBuf),
}
