use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AppletError, Result};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const DEFAULT_INPUT_LIMIT: usize = 8 * 1024 * 1024;
const DEFAULT_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const DEFAULT_ENTRY_LIMIT: usize = 10_000;
const DEFAULT_RECURSION_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppletLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_filesystem_entries: usize,
    pub max_recursion_depth: usize,
}

impl Default for AppletLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: DEFAULT_INPUT_LIMIT,
            max_output_bytes: DEFAULT_OUTPUT_LIMIT,
            max_filesystem_entries: DEFAULT_ENTRY_LIMIT,
            max_recursion_depth: DEFAULT_RECURSION_LIMIT,
        }
    }
}

impl AppletLimits {
    pub fn validate(self) -> Result<Self> {
        if self.max_input_bytes == 0 || self.max_input_bytes > 64 * 1024 * 1024 {
            return Err(AppletError::usage(
                "limits",
                "max_input_bytes must be between 1 and 67108864",
            ));
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > 64 * 1024 * 1024 {
            return Err(AppletError::usage(
                "limits",
                "max_output_bytes must be between 1 and 67108864",
            ));
        }
        if self.max_filesystem_entries == 0 || self.max_filesystem_entries > 100_000 {
            return Err(AppletError::usage(
                "limits",
                "max_filesystem_entries must be between 1 and 100000",
            ));
        }
        if self.max_recursion_depth == 0 || self.max_recursion_depth > 256 {
            return Err(AppletError::usage(
                "limits",
                "max_recursion_depth must be between 1 and 256",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct AppletContext {
    root: PathBuf,
    root_identity: RootIdentity,
    limits: AppletLimits,
    read_only: bool,
    hostname: String,
    user: String,
}

#[derive(Clone, Debug)]
struct RootIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl RootIdentity {
    fn capture(metadata: &fs::Metadata) -> Self {
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }

    fn matches(&self, metadata: &fs::Metadata) -> bool {
        #[cfg(unix)]
        {
            self.device == metadata.dev() && self.inode == metadata.ino()
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            true
        }
    }
}

impl AppletContext {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let supplied_root = root.as_ref();
        if !supplied_root.is_absolute() {
            return Err(AppletError::UnsafePath(
                "sandbox root must be an absolute host path".to_owned(),
            ));
        }
        let lexical_root = normalize_absolute_host_path(supplied_root)?;
        let supplied_metadata = fs::symlink_metadata(&lexical_root)
            .map_err(|error| AppletError::io("sandbox root", error))?;
        if !supplied_metadata.is_dir() || supplied_metadata.file_type().is_symlink() {
            return Err(AppletError::UnsafePath(
                "sandbox root must be an existing real directory".to_owned(),
            ));
        }
        let root = fs::canonicalize(&lexical_root)
            .map_err(|error| AppletError::io("sandbox root", error))?;
        if root != lexical_root {
            return Err(AppletError::UnsafePath(
                "sandbox root must not traverse symbolic links".to_owned(),
            ));
        }
        let metadata =
            fs::symlink_metadata(&root).map_err(|error| AppletError::io("sandbox root", error))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(AppletError::UnsafePath(
                "sandbox root must be a real directory".to_owned(),
            ));
        }
        Ok(Self {
            root,
            root_identity: RootIdentity::capture(&metadata),
            limits: AppletLimits::default(),
            read_only: false,
            hostname: "rish".to_owned(),
            user: "rish".to_owned(),
        })
    }

    pub fn with_limits(mut self, limits: AppletLimits) -> Result<Self> {
        self.limits = limits.validate()?;
        Ok(self)
    }

    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    #[must_use]
    pub fn identity(mut self, user: impl Into<String>, hostname: impl Into<String>) -> Self {
        self.user = user.into();
        self.hostname = hostname.into();
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn limits(&self) -> AppletLimits {
        self.limits
    }

    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    #[must_use]
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    pub(crate) fn require_writable(&self) -> Result<()> {
        if self.read_only {
            Err(AppletError::ReadOnlyFilesystem)
        } else {
            Ok(())
        }
    }

    pub(crate) fn verify_root(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.root)
            .map_err(|error| AppletError::io("sandbox root", error))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !self.root_identity.matches(&metadata)
        {
            return Err(AppletError::UnsafePath(
                "sandbox root changed after context creation".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn guest_relative(&self, cwd: &str, value: &str) -> Result<PathBuf> {
        let combined = if value.starts_with('/') {
            PathBuf::from(value.trim_start_matches('/'))
        } else {
            let mut path = PathBuf::from(cwd.trim_start_matches('/'));
            path.push(value);
            path
        };
        normalize_relative(&combined)
    }

    pub(crate) fn resolve_existing(&self, cwd: &str, value: &str) -> Result<PathBuf> {
        let relative = self.guest_relative(cwd, value)?;
        let mut current = self.root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            let metadata =
                fs::symlink_metadata(&current).map_err(|error| AppletError::io(value, error))?;
            if metadata.file_type().is_symlink() {
                return Err(AppletError::UnsafePath(format!(
                    "symbolic links are not followed: {value}"
                )));
            }
        }
        Ok(current)
    }

    pub(crate) fn resolve_entry(&self, cwd: &str, value: &str) -> Result<PathBuf> {
        let relative = self.guest_relative(cwd, value)?;
        if relative.as_os_str().is_empty() {
            return Ok(self.root.clone());
        }
        let parent = relative
            .parent()
            .ok_or_else(|| AppletError::UnsafePath(value.to_owned()))?;
        let parent_value = format!("/{}", parent.display());
        let parent_path = self.resolve_existing("/", &parent_value)?;
        let name = relative
            .file_name()
            .ok_or_else(|| AppletError::UnsafePath(value.to_owned()))?;
        let path = parent_path.join(name);
        fs::symlink_metadata(&path).map_err(|error| AppletError::io(value, error))?;
        Ok(path)
    }

    pub(crate) fn resolve_for_create(&self, cwd: &str, value: &str) -> Result<PathBuf> {
        let relative = self.guest_relative(cwd, value)?;
        let Some(parent) = relative.parent() else {
            return Err(AppletError::UnsafePath(value.to_owned()));
        };
        let parent_value = format!("/{}", parent.display());
        let parent_path = self.resolve_existing("/", &parent_value)?;
        let name = relative
            .file_name()
            .ok_or_else(|| AppletError::UnsafePath(value.to_owned()))?;
        Ok(parent_path.join(name))
    }

    pub(crate) fn display_guest_path(&self, path: &Path) -> Result<String> {
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            AppletError::UnsafePath("resolved path escapes the applet sandbox".to_owned())
        })?;
        if relative.as_os_str().is_empty() {
            Ok("/".to_owned())
        } else {
            Ok(format!("/{}", relative.display()))
        }
    }
}

fn normalize_absolute_host_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(value) => normalized.push(value.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(value) => normalized.push(value),
            Component::CurDir | Component::ParentDir => {
                return Err(AppletError::UnsafePath(
                    "sandbox root must be a normalized absolute path".to_owned(),
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() || !normalized.is_absolute() {
        return Err(AppletError::UnsafePath(
            "sandbox root must be an absolute host path".to_owned(),
        ));
    }
    Ok(normalized)
}

fn normalize_relative(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(AppletError::UnsafePath(
                        "path escapes the applet sandbox".to_owned(),
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AppletError::UnsafePath(
                    "path must be guest-relative after normalization".to_owned(),
                ));
            }
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_paths_cannot_escape() {
        assert!(normalize_relative(Path::new("../../outside")).is_err());
        assert_eq!(
            normalize_relative(Path::new("a/../b")).unwrap(),
            PathBuf::from("b")
        );
    }

    #[test]
    fn sandbox_root_must_already_exist() {
        let parent = tempfile::tempdir().unwrap();
        assert!(AppletContext::new(parent.path().join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_root_cannot_be_a_symlink() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let real = parent.path().join("real");
        fs::create_dir(&real).unwrap();
        let linked = parent.path().join("linked");
        symlink(&real, &linked).unwrap();

        assert!(AppletContext::new(&linked).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_sandbox_root_fails_identity_verification() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).unwrap();
        let context = AppletContext::new(root.canonicalize().unwrap()).unwrap();

        fs::rename(&root, parent.path().join("old-root")).unwrap();
        fs::create_dir(&root).unwrap();

        assert!(matches!(
            context.verify_root(),
            Err(AppletError::UnsafePath(_))
        ));
    }
}
