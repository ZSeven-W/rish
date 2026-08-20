use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::ImportError;

pub(crate) fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf, ImportError> {
    if bytes.contains(&0) {
        return Err(ImportError::InvalidPath {
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
        let value = std::str::from_utf8(bytes).map_err(|_| ImportError::InvalidPath {
            path: PathBuf::new(),
            reason: "non-UTF-8 path on a non-Unix build host",
        })?;
        Ok(PathBuf::from(value))
    }
}

pub(crate) fn normalize_entry_path(path: &Path) -> Result<PathBuf, ImportError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(ImportError::InvalidPath {
                    path: path.to_owned(),
                    reason: "parent-directory component",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ImportError::InvalidPath {
                    path: path.to_owned(),
                    reason: "absolute path",
                });
            }
        }
    }
    Ok(normalized)
}

pub(crate) fn encoded_len(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Whiteout {
    Remove(PathBuf),
    Opaque(PathBuf),
}

pub(crate) fn whiteout_for(path: &Path) -> Result<Option<Whiteout>, ImportError> {
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
    let Some(target) = name.strip_prefix(PREFIX) else {
        return Ok(None);
    };
    if target.is_empty() || target == b"." || target == b".." {
        return Err(ImportError::MalformedWhiteout(path.to_owned()));
    }
    let mut result = path.parent().unwrap_or_else(|| Path::new("")).to_owned();
    result.push(path_from_bytes(target)?);
    Ok(Some(Whiteout::Remove(result)))
}
