use std::fs;
use std::path::Path;

use crate::{AppletContext, AppletError, Result};

pub(super) fn count_entry(
    context: &AppletContext,
    visited: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > context.limits().max_recursion_depth {
        return Err(AppletError::EntryLimit {
            limit: context.limits().max_recursion_depth,
        });
    }
    *visited = visited.saturating_add(1);
    if *visited > context.limits().max_filesystem_entries {
        return Err(AppletError::EntryLimit {
            limit: context.limits().max_filesystem_entries,
        });
    }
    Ok(())
}

pub(super) fn reject_root(context: &AppletContext, path: &Path) -> Result<()> {
    if path == context.root() {
        Err(AppletError::UnsafePath(
            "the applet sandbox root cannot be removed".to_owned(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn file_kind(metadata: &fs::Metadata) -> &'static str {
    if metadata.file_type().is_symlink() {
        "symbolic link"
    } else if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "regular file"
    } else {
        "special file"
    }
}

#[cfg(unix)]
pub(super) fn unix_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
pub(super) fn unix_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o666
    }
}

pub(super) fn mode_string(metadata: &fs::Metadata) -> String {
    let mut output = String::with_capacity(10);
    output.push(if metadata.is_dir() {
        'd'
    } else if metadata.file_type().is_symlink() {
        'l'
    } else {
        '-'
    });
    let mode = unix_mode(metadata);
    for (read, write, execute) in [
        (0o400, 0o200, 0o100),
        (0o040, 0o020, 0o010),
        (0o004, 0o002, 0o001),
    ] {
        output.push(if mode & read != 0 { 'r' } else { '-' });
        output.push(if mode & write != 0 { 'w' } else { '-' });
        output.push(if mode & execute != 0 { 'x' } else { '-' });
    }
    output
}

pub(super) fn executable_suffix(metadata: &fs::Metadata) -> &'static str {
    if unix_mode(metadata) & 0o111 != 0 {
        "*"
    } else {
        ""
    }
}
