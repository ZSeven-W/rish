use std::fs::{self, FileTimes, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rish_core::GuestCommand;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::common::{count_entry, reject_root};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(super) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "cp" => copy(context, command),
        "ln" => link(context, command),
        "mkdir" => make_directory(context, command),
        "mv" => move_entry(context, command),
        "rm" => remove(context, command),
        "rmdir" => remove_directory(context, command),
        "touch" => touch(context, command),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}

fn make_directory(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, operands) = split_flags(command);
    let mut parents = false;
    let mut verbose = false;
    for flag in flags {
        match flag {
            Flag::Short('p') | Flag::Long("--parents") => parents = true,
            Flag::Short('v') | Flag::Long("--verbose") => verbose = true,
            value => {
                return Err(AppletError::usage(
                    "mkdir",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.is_empty() {
        return Err(AppletError::usage("mkdir", "missing directory operand"));
    }
    let mut output = Vec::new();
    for operand in operands {
        if parents {
            create_parent_chain(context, &command.cwd, operand)?;
        } else {
            let path = context.resolve_for_create(&command.cwd, operand)?;
            fs::create_dir(&path).map_err(|error| AppletError::io(operand, error))?;
        }
        if verbose {
            push_bounded(
                &mut output,
                format!("mkdir: created directory '{operand}'\n").as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
    }
    Ok(AppletOutput::success(output))
}

fn create_parent_chain(context: &AppletContext, cwd: &str, operand: &str) -> Result<()> {
    let relative = context.guest_relative(cwd, operand)?;
    let mut current = context.root().to_owned();
    for (depth, component) in relative.components().enumerate() {
        if depth >= context.limits().max_recursion_depth {
            return Err(AppletError::EntryLimit {
                limit: context.limits().max_recursion_depth,
            });
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(AppletError::UnsafePath(format!(
                    "mkdir component is not a real directory: {operand}"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| AppletError::io(operand, error))?;
            }
            Err(error) => return Err(AppletError::io(operand, error)),
        }
    }
    Ok(())
}

fn remove_directory(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, operands) = split_flags(command);
    let mut parents = false;
    let mut verbose = false;
    for flag in flags {
        match flag {
            Flag::Short('p') | Flag::Long("--parents") => parents = true,
            Flag::Short('v') | Flag::Long("--verbose") => verbose = true,
            value => {
                return Err(AppletError::usage(
                    "rmdir",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.is_empty() {
        return Err(AppletError::usage("rmdir", "missing directory operand"));
    }
    let mut output = Vec::new();
    for operand in operands {
        let path = context.resolve_existing(&command.cwd, operand)?;
        reject_root(context, &path)?;
        fs::remove_dir(&path).map_err(|error| AppletError::io(operand, error))?;
        if verbose {
            push_bounded(
                &mut output,
                format!("rmdir: removed directory '{operand}'\n").as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
        if parents {
            let mut parent = path.parent();
            while let Some(path) = parent {
                if path == context.root() {
                    break;
                }
                match fs::remove_dir(path) {
                    Ok(()) => parent = path.parent(),
                    Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
                    Err(error) => return Err(AppletError::io(operand, error)),
                }
            }
        }
    }
    Ok(AppletOutput::success(output))
}

fn touch(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, operands) = split_flags(command);
    let mut no_create = false;
    let mut access_time = false;
    let mut modification_time = false;
    let mut selective_time = false;
    for flag in flags {
        match flag {
            Flag::Short('c') | Flag::Long("--no-create") => no_create = true,
            Flag::Short('a') => {
                access_time = true;
                selective_time = true;
            }
            Flag::Short('m') => {
                modification_time = true;
                selective_time = true;
            }
            value => {
                return Err(AppletError::usage(
                    "touch",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.is_empty() {
        return Err(AppletError::usage("touch", "missing file operand"));
    }
    if !selective_time {
        access_time = true;
        modification_time = true;
    }
    let now = SystemTime::now();
    for operand in operands {
        let path = match context.resolve_existing(&command.cwd, operand) {
            Ok(path) => path,
            Err(AppletError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound && !no_create =>
            {
                context.resolve_for_create(&command.cwd, operand)?
            }
            Err(AppletError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound && no_create =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let file = OpenOptions::new()
            .create(!no_create)
            .write(true)
            .open(&path)
            .map_err(|error| AppletError::io(operand, error))?;
        let mut times = FileTimes::new();
        if access_time {
            times = times.set_accessed(now);
        }
        if modification_time {
            times = times.set_modified(now);
        }
        file.set_times(times)
            .map_err(|error| AppletError::io(operand, error))?;
    }
    Ok(AppletOutput::success(Vec::new()))
}

#[derive(Default)]
struct RemoveOptions {
    force: bool,
    recursive: bool,
    verbose: bool,
}

fn remove(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, operands) = split_flags(command);
    let mut options = RemoveOptions::default();
    for flag in flags {
        match flag {
            Flag::Short('f') | Flag::Long("--force") => options.force = true,
            Flag::Short('r' | 'R') | Flag::Long("--recursive") => options.recursive = true,
            Flag::Short('v') | Flag::Long("--verbose") => options.verbose = true,
            value => {
                return Err(AppletError::usage(
                    "rm",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.is_empty() && !options.force {
        return Err(AppletError::usage("rm", "missing operand"));
    }
    let mut output = Vec::new();
    let mut visited = 0usize;
    for operand in operands {
        let path = match context.resolve_entry(&command.cwd, operand) {
            Ok(path) => path,
            Err(AppletError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound && options.force =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        reject_root(context, &path)?;
        remove_path(context, &path, operand, &options, 0, &mut visited)?;
        if options.verbose {
            push_bounded(
                &mut output,
                format!("removed '{operand}'\n").as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
    }
    Ok(AppletOutput::success(output))
}

fn remove_path(
    context: &AppletContext,
    path: &Path,
    display: &str,
    options: &RemoveOptions,
    depth: usize,
    visited: &mut usize,
) -> Result<()> {
    count_entry(context, visited, depth)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| AppletError::io(display, error))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        if !options.recursive {
            return Err(AppletError::usage(
                "rm",
                format!("{display} is a directory"),
            ));
        }
        for entry in fs::read_dir(path).map_err(|error| AppletError::io(display, error))? {
            let entry = entry.map_err(|error| AppletError::io(display, error))?;
            remove_path(
                context,
                &entry.path(),
                &entry.file_name().to_string_lossy(),
                options,
                depth + 1,
                visited,
            )?;
        }
        fs::remove_dir(path).map_err(|error| AppletError::io(display, error))
    } else {
        fs::remove_file(path).map_err(|error| AppletError::io(display, error))
    }
}

fn copy(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, mut operands) = split_flags(command);
    let mut recursive = false;
    let mut no_clobber = false;
    let mut verbose = false;
    for flag in flags {
        match flag {
            Flag::Short('r' | 'R') | Flag::Long("--recursive") => recursive = true,
            Flag::Short('a') | Flag::Long("--archive") => {
                return Err(AppletError::usage(
                    "cp",
                    "archive mode requires a guest Linux backend",
                ));
            }
            Flag::Short('n') | Flag::Long("--no-clobber") => no_clobber = true,
            Flag::Short('f') | Flag::Long("--force") => {}
            Flag::Short('v') | Flag::Long("--verbose") => verbose = true,
            value => {
                return Err(AppletError::usage(
                    "cp",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.len() < 2 {
        return Err(AppletError::usage("cp", "missing source or destination"));
    }
    let destination_operand = operands.pop().expect("length checked");
    let destination = destination_path(context, &command.cwd, destination_operand, operands.len())?;
    let destination_is_directory = destination
        .as_ref()
        .is_some_and(|(_, metadata)| metadata.is_dir());
    if operands.len() > 1 && !destination_is_directory {
        return Err(AppletError::usage(
            "cp",
            "multiple sources require a directory destination",
        ));
    }
    let destination_path = destination.map_or_else(
        || context.resolve_for_create(&command.cwd, destination_operand),
        |(path, _)| Ok(path),
    )?;

    let mut visited = 0usize;
    let mut copied_bytes = 0usize;
    let mut output = Vec::new();
    for source_operand in operands {
        let source = context.resolve_entry(&command.cwd, source_operand)?;
        let target = if destination_is_directory {
            destination_path.join(
                source
                    .file_name()
                    .ok_or_else(|| AppletError::UnsafePath(source_operand.to_owned()))?,
            )
        } else {
            destination_path.clone()
        };
        copy_path(
            context,
            &source,
            &target,
            source_operand,
            recursive,
            no_clobber,
            0,
            &mut visited,
            &mut copied_bytes,
        )?;
        if verbose {
            push_bounded(
                &mut output,
                format!("'{source_operand}' -> '{destination_operand}'\n").as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
    }
    Ok(AppletOutput::success(output))
}

#[allow(clippy::too_many_arguments)]
fn copy_path(
    context: &AppletContext,
    source: &Path,
    destination: &Path,
    display: &str,
    recursive: bool,
    no_clobber: bool,
    depth: usize,
    visited: &mut usize,
    copied_bytes: &mut usize,
) -> Result<()> {
    count_entry(context, visited, depth)?;
    let metadata = fs::symlink_metadata(source).map_err(|error| AppletError::io(display, error))?;
    if metadata.file_type().is_symlink() {
        return Err(AppletError::UnsafePath(format!(
            "cp does not follow symbolic links: {display}"
        )));
    }
    if metadata.is_dir() {
        if !recursive {
            return Err(AppletError::usage(
                "cp",
                format!("{display} is a directory"),
            ));
        }
        if destination.starts_with(source) {
            return Err(AppletError::UnsafePath(
                "cannot copy a directory into itself".to_owned(),
            ));
        }
        match fs::create_dir(destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let target_metadata = fs::symlink_metadata(destination)
                    .map_err(|error| AppletError::io(display, error))?;
                if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
                    return Err(AppletError::UnsafePath(display.to_owned()));
                }
            }
            Err(error) => return Err(AppletError::io(display, error)),
        }
        for entry in fs::read_dir(source).map_err(|error| AppletError::io(display, error))? {
            let entry = entry.map_err(|error| AppletError::io(display, error))?;
            copy_path(
                context,
                &entry.path(),
                &destination.join(entry.file_name()),
                &entry.file_name().to_string_lossy(),
                recursive,
                no_clobber,
                depth + 1,
                visited,
                copied_bytes,
            )?;
        }
    } else {
        if !metadata.is_file() {
            return Err(AppletError::UnsafePath(format!(
                "cp source must be a regular file or directory: {display}"
            )));
        }
        let file_bytes = usize::try_from(metadata.len()).map_err(|_| AppletError::InputLimit {
            limit: context.limits().max_input_bytes,
        })?;
        *copied_bytes = copied_bytes
            .checked_add(file_bytes)
            .ok_or(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            })?;
        if *copied_bytes > context.limits().max_input_bytes {
            return Err(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            });
        }
        match fs::symlink_metadata(destination) {
            Ok(target) if target.file_type().is_symlink() => {
                return Err(AppletError::UnsafePath(format!(
                    "cp destination is a symbolic link for: {display}"
                )));
            }
            Ok(target) if same_file(source, &metadata, destination, &target) => {
                return Err(AppletError::usage(
                    "cp",
                    format!("{display} and its destination are the same file"),
                ));
            }
            Ok(_) if no_clobber => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppletError::io(display, error)),
        }
        fs::copy(source, destination).map_err(|error| AppletError::io(display, error))?;
    }
    Ok(())
}

fn same_file(
    _source_path: &Path,
    source: &fs::Metadata,
    _destination_path: &Path,
    destination: &fs::Metadata,
) -> bool {
    #[cfg(unix)]
    {
        source.dev() == destination.dev() && source.ino() == destination.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (source, destination);
        _source_path == _destination_path
    }
}

fn move_entry(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, operands) = split_flags(command);
    let mut no_clobber = false;
    let mut verbose = false;
    for flag in flags {
        match flag {
            Flag::Short('n') | Flag::Long("--no-clobber") => no_clobber = true,
            Flag::Short('f') | Flag::Long("--force") => {}
            Flag::Short('v') | Flag::Long("--verbose") => verbose = true,
            value => {
                return Err(AppletError::usage(
                    "mv",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.len() != 2 {
        return Err(AppletError::usage("mv", "usage: mv SOURCE DEST"));
    }
    let source = context.resolve_entry(&command.cwd, operands[0])?;
    reject_root(context, &source)?;
    let existing = destination_path(context, &command.cwd, operands[1], 1)?;
    let destination = match existing {
        Some((path, metadata)) if metadata.is_dir() => path.join(
            source
                .file_name()
                .ok_or_else(|| AppletError::UnsafePath(operands[0].to_owned()))?,
        ),
        Some((path, _)) => path,
        None => context.resolve_for_create(&command.cwd, operands[1])?,
    };
    if no_clobber && destination.exists() {
        return Ok(AppletOutput::success(Vec::new()));
    }
    fs::rename(&source, &destination).map_err(|error| AppletError::io(operands[1], error))?;
    Ok(AppletOutput::success(if verbose {
        format!("renamed '{}' -> '{}'\n", operands[0], operands[1]).into_bytes()
    } else {
        Vec::new()
    }))
}

fn destination_path(
    context: &AppletContext,
    cwd: &str,
    operand: &str,
    source_count: usize,
) -> Result<Option<(PathBuf, fs::Metadata)>> {
    match context.resolve_entry(cwd, operand) {
        Ok(path) => {
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| AppletError::io(operand, error))?;
            if metadata.file_type().is_symlink() {
                return Err(AppletError::UnsafePath(format!(
                    "destination is a symbolic link: {operand}"
                )));
            }
            Ok(Some((path, metadata)))
        }
        Err(AppletError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound && source_count == 1 =>
        {
            Ok(None)
        }
        Err(AppletError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(AppletError::usage(
                "copy",
                "multiple sources require an existing directory destination",
            ))
        }
        Err(error) => Err(error),
    }
}

fn link(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, operands) = split_flags(command);
    let mut symbolic = false;
    let mut force = false;
    for flag in flags {
        match flag {
            Flag::Short('s') | Flag::Long("--symbolic") => symbolic = true,
            Flag::Short('f') | Flag::Long("--force") => force = true,
            value => {
                return Err(AppletError::usage(
                    "ln",
                    format!("unsupported flag: {}", value.display()),
                ));
            }
        }
    }
    if operands.len() != 2 {
        return Err(AppletError::usage("ln", "usage: ln [-s] TARGET LINK_NAME"));
    }
    let hard_source = if symbolic {
        validate_symlink_target(context, &command.cwd, operands[1], operands[0])?;
        None
    } else {
        Some(context.resolve_existing(&command.cwd, operands[0])?)
    };
    let destination = match context.resolve_entry(&command.cwd, operands[1]) {
        Ok(path) if force => {
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| AppletError::io(operands[1], error))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                return Err(AppletError::usage("ln", "will not replace a directory"));
            }
            if let Some(source) = &hard_source {
                let source_metadata = fs::symlink_metadata(source)
                    .map_err(|error| AppletError::io(operands[0], error))?;
                if same_file(source, &source_metadata, &path, &metadata) {
                    return Err(AppletError::usage(
                        "ln",
                        "target and link name are the same file",
                    ));
                }
            }
            fs::remove_file(&path).map_err(|error| AppletError::io(operands[1], error))?;
            path
        }
        Ok(_) => {
            return Err(AppletError::usage(
                "ln",
                "destination exists; use -f to replace it",
            ));
        }
        Err(AppletError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            context.resolve_for_create(&command.cwd, operands[1])?
        }
        Err(error) => return Err(error),
    };
    if symbolic {
        create_symlink(Path::new(operands[0]), &destination)
            .map_err(|error| AppletError::io(operands[1], error))?;
    } else {
        fs::hard_link(
            hard_source
                .as_deref()
                .expect("hard-link source was resolved"),
            &destination,
        )
        .map_err(|error| AppletError::io(operands[1], error))?;
    }
    Ok(AppletOutput::success(Vec::new()))
}

fn validate_symlink_target(
    context: &AppletContext,
    cwd: &str,
    link_name: &str,
    target: &str,
) -> Result<()> {
    if target.starts_with('/') {
        return Err(AppletError::UnsafePath(
            "portable symbolic-link targets must be relative".to_owned(),
        ));
    } else {
        let link_relative = context.guest_relative(cwd, link_name)?;
        let parent = link_relative.parent().unwrap_or_else(|| Path::new(""));
        context.guest_relative("/", &format!("/{}/{}", parent.display(), target))?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symbolic links are unavailable",
    ))
}

#[derive(Clone, Copy)]
enum Flag<'a> {
    Short(char),
    Long(&'a str),
}

impl Flag<'_> {
    fn display(self) -> String {
        match self {
            Self::Short(value) => format!("-{value}"),
            Self::Long(value) => value.to_owned(),
        }
    }
}

fn split_flags(command: &GuestCommand) -> (Vec<Flag<'_>>, Vec<&str>) {
    let mut flags = Vec::new();
    let mut operands = Vec::new();
    let mut parse_options = true;
    for argument in &command.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument.starts_with("--") {
            flags.push(Flag::Long(argument));
        } else if parse_options && argument.starts_with('-') && argument != "-" {
            flags.extend(argument[1..].chars().map(Flag::Short));
        } else {
            operands.push(argument.as_str());
        }
    }
    (flags, operands)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::AppletLimits;

    fn setup() -> (TempDir, AppletContext) {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("alpha"), b"one").unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
        (root, context)
    }

    fn run(context: &AppletContext, program: &str, args: &[&str]) -> Result<AppletOutput> {
        let command = GuestCommand::new(program, args.iter().map(|value| (*value).to_owned()));
        execute(context, &command)
    }

    #[test]
    fn copy_move_and_remove_stay_inside_root() {
        let (root, context) = setup();
        run(&context, "cp", &["alpha", "copy"]).unwrap();
        run(&context, "mv", &["copy", "moved"]).unwrap();
        run(&context, "rm", &["moved"]).unwrap();
        assert!(!root.path().join("moved").exists());
        assert!(run(&context, "rm", &["-r", "/"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_following_fails_closed() {
        let (root, context) = setup();
        std::os::unix::fs::symlink("/tmp", root.path().join("escape")).unwrap();
        let command = GuestCommand::new("touch", ["escape/file".to_owned()]);
        assert!(execute(&context, &command).is_err());
    }

    #[test]
    fn copy_archive_mode_fails_closed() {
        let (_root, context) = setup();
        let command = GuestCommand::new(
            "cp",
            ["-a".to_owned(), "alpha".to_owned(), "copy".to_owned()],
        );

        assert!(matches!(
            execute(&context, &command),
            Err(AppletError::InvalidArguments { .. })
        ));
    }

    #[test]
    fn copy_obeys_the_cumulative_byte_limit() {
        let (root, context) = setup();
        fs::write(root.path().join("large"), b"12345").unwrap();
        let context = context
            .with_limits(AppletLimits {
                max_input_bytes: 4,
                ..AppletLimits::default()
            })
            .unwrap();
        let command = GuestCommand::new("cp", ["large".to_owned(), "copy".to_owned()]);

        assert!(matches!(
            execute(&context, &command),
            Err(AppletError::InputLimit { limit: 4 })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn copy_and_forced_link_never_destroy_the_same_file() {
        let (root, context) = setup();
        let alpha = root.path().join("alpha");
        let alias = root.path().join("alias");
        fs::hard_link(&alpha, &alias).unwrap();

        assert!(run(&context, "cp", &["alpha", "alpha"]).is_err());
        assert!(run(&context, "cp", &["alpha", "alias"]).is_err());
        assert!(run(&context, "ln", &["-f", "alpha", "alpha"]).is_err());
        assert!(run(&context, "ln", &["-f", "alpha", "alias"]).is_err());

        assert_eq!(fs::read(alpha).unwrap(), b"one");
        assert_eq!(fs::read(alias).unwrap(), b"one");
    }
}
