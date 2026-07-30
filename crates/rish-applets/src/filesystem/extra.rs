use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rish_core::GuestCommand;

use super::common::count_entry;
use crate::{AppletContext, AppletError, AppletOutput, Result};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "chmod" => chmod(context, command),
        "mktemp" => mktemp(context, command),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}

fn chmod(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut recursive = false;
    let mut operands = Vec::new();
    let mut options = true;
    for argument in &command.args {
        if options && argument == "--" {
            options = false;
        } else if options && matches!(argument.as_str(), "-R" | "--recursive") {
            recursive = true;
        } else if options && argument.starts_with('-') {
            return Err(AppletError::usage(
                "chmod",
                format!("unsupported flag: {argument}"),
            ));
        } else {
            operands.push(argument.as_str());
        }
    }
    if operands.len() < 2 {
        return Err(AppletError::usage("chmod", "usage: chmod MODE FILE..."));
    }
    let mode = u32::from_str_radix(operands[0].trim_start_matches("0o"), 8)
        .ok()
        .filter(|mode| *mode <= 0o7777)
        .ok_or_else(|| {
            AppletError::usage("chmod", "portable chmod currently accepts octal modes")
        })?;
    let mut visited = 0usize;
    for operand in &operands[1..] {
        let path = context.resolve_existing(&command.cwd, operand)?;
        chmod_path(context, &path, operand, mode, recursive, 0, &mut visited)?;
    }
    Ok(AppletOutput::success(Vec::new()))
}

fn chmod_path(
    context: &AppletContext,
    path: &Path,
    display: &str,
    mode: u32,
    recursive: bool,
    depth: usize,
    visited: &mut usize,
) -> Result<()> {
    count_entry(context, visited, depth)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| AppletError::io(display, error))?;
    if metadata.file_type().is_symlink() {
        return Err(AppletError::UnsafePath(format!(
            "chmod does not follow symbolic links: {display}"
        )));
    }
    set_mode(path, mode).map_err(|error| AppletError::io(display, error))?;
    if recursive && metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| AppletError::io(display, error))? {
            let entry = entry.map_err(|error| AppletError::io(display, error))?;
            chmod_path(
                context,
                &entry.path(),
                &entry.file_name().to_string_lossy(),
                mode,
                true,
                depth + 1,
                visited,
            )?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    fs::set_permissions(path, permissions)
}

fn mktemp(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut directory = false;
    let mut parent = None;
    let mut template = None;
    let mut index = 0;
    while let Some(argument) = command.args.get(index) {
        match argument.as_str() {
            "-d" | "--directory" => {
                directory = true;
                index += 1;
            }
            "-p" | "--tmpdir" => {
                parent = Some(
                    command
                        .args
                        .get(index + 1)
                        .ok_or_else(|| AppletError::usage("mktemp", "-p requires a directory"))?
                        .as_str(),
                );
                index += 2;
            }
            "-u" | "--dry-run" => {
                return Err(AppletError::usage(
                    "mktemp",
                    "-u is unsafe and is not supported",
                ));
            }
            value if value.starts_with('-') => {
                return Err(AppletError::usage(
                    "mktemp",
                    format!("unsupported flag: {value}"),
                ));
            }
            value if template.is_none() => {
                template = Some(value);
                index += 1;
            }
            _ => return Err(AppletError::usage("mktemp", "too many templates")),
        }
    }
    let template = template.unwrap_or("tmp.XXXXXX");
    let (prefix, suffix) = split_template(template)?;
    let parent_path = match parent {
        Some(value) => context.resolve_existing(&command.cwd, value)?,
        None => {
            let placeholder = context.resolve_for_create(&command.cwd, template)?;
            placeholder
                .parent()
                .ok_or_else(|| AppletError::UnsafePath(template.to_owned()))?
                .to_owned()
        }
    };
    let metadata = fs::symlink_metadata(&parent_path)
        .map_err(|error| AppletError::io("mktemp parent", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppletError::UnsafePath(
            "mktemp parent is not a real directory".to_owned(),
        ));
    }

    for _ in 0..128 {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("{prefix}{:06x}{suffix}", (timestamp as u64) ^ sequence);
        let path = parent_path.join(name);
        let created = if directory {
            fs::create_dir(&path)
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map(drop)
        };
        match created {
            Ok(()) => {
                set_mode(&path, if directory { 0o700 } else { 0o600 })
                    .map_err(|error| AppletError::io(template, error))?;
                let guest = context.display_guest_path(&path)?;
                return Ok(AppletOutput::success(format!("{guest}\n")));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(AppletError::io(template, error)),
        }
    }
    Err(AppletError::io(
        template,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary path",
        ),
    ))
}

fn split_template(template: &str) -> Result<(&str, &str)> {
    if template.contains('/') {
        let name = Path::new(template)
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| AppletError::usage("mktemp", "template is not UTF-8"))?;
        return split_template(name);
    }
    let position = template
        .rfind("XXXXXX")
        .ok_or_else(|| AppletError::usage("mktemp", "template must contain XXXXXX"))?;
    Ok((&template[..position], &template[position + 6..]))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn mktemp_creates_private_entry_inside_root() {
        let root = TempDir::new().unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
        let command = GuestCommand::new("mktemp", ["work.XXXXXX".to_owned()]);
        let output = execute(&context, &command).unwrap();
        let guest = String::from_utf8(output.stdout).unwrap();
        assert!(guest.starts_with("/work."));
        assert!(
            root.path()
                .join(guest.trim_start_matches('/').trim())
                .exists()
        );
    }
}
