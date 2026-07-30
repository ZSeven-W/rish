use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use rish_core::GuestCommand;

use super::common::{count_entry, executable_suffix, mode_string};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(super) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "du" => disk_usage(context, command),
        "ls" => list(context, command),
        "find" => find(context, command),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}

#[derive(Default)]
struct DuOptions {
    all: bool,
    summarize: bool,
    human: bool,
}

fn disk_usage(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut options = DuOptions::default();
    let mut operands = Vec::new();
    let mut parse_options = true;
    for argument in &command.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument.starts_with('-') && argument != "-" {
            for flag in argument[1..].chars() {
                match flag {
                    'a' => options.all = true,
                    's' => options.summarize = true,
                    'h' => options.human = true,
                    'k' => {}
                    _ => {
                        return Err(AppletError::usage(
                            "du",
                            format!("unsupported flag: -{flag}"),
                        ));
                    }
                }
            }
        } else {
            operands.push(argument.as_str());
        }
    }
    if options.all && options.summarize {
        return Err(AppletError::usage("du", "-a and -s are mutually exclusive"));
    }
    if operands.is_empty() {
        operands.push(".");
    }
    let mut output = Vec::new();
    let mut visited = 0usize;
    let mut discovered = 0usize;
    for operand in operands {
        let path = context.resolve_entry(&command.cwd, operand)?;
        let bytes = du_size(
            context,
            &path,
            operand,
            &options,
            0,
            &mut visited,
            &mut discovered,
            &mut output,
        )?;
        if options.summarize {
            render_du_line(context, bytes, operand, options.human, &mut output)?;
        }
    }
    Ok(AppletOutput::success(output))
}

#[allow(clippy::too_many_arguments)]
fn du_size(
    context: &AppletContext,
    path: &Path,
    display: &str,
    options: &DuOptions,
    depth: usize,
    visited: &mut usize,
    discovered: &mut usize,
    output: &mut Vec<u8>,
) -> Result<u64> {
    count_entry(context, visited, depth)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| AppletError::io(display, error))?;
    let mut bytes = metadata.len();
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let entries = sorted_entries(context, path, display, discovered)?;
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_display = if display == "." {
                format!("./{name}")
            } else {
                format!("{display}/{name}")
            };
            bytes = bytes
                .checked_add(du_size(
                    context,
                    &entry.path(),
                    &child_display,
                    options,
                    depth + 1,
                    visited,
                    discovered,
                    output,
                )?)
                .ok_or_else(|| AppletError::usage("du", "size overflow"))?;
        }
        if !options.summarize {
            render_du_line(context, bytes, display, options.human, output)?;
        }
    } else if options.all && !options.summarize {
        render_du_line(context, bytes, display, options.human, output)?;
    }
    Ok(bytes)
}

fn render_du_line(
    context: &AppletContext,
    bytes: u64,
    display: &str,
    human: bool,
    output: &mut Vec<u8>,
) -> Result<()> {
    let size = if human {
        human_size(bytes)
    } else {
        bytes.div_ceil(1024).to_string()
    };
    push_bounded(
        output,
        format!("{size}\t{display}\n").as_bytes(),
        context.limits().max_output_bytes,
    )
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T", "P"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

#[derive(Default)]
struct ListOptions {
    all: bool,
    almost_all: bool,
    directory: bool,
    long: bool,
    classify: bool,
    recursive: bool,
}

fn list(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut options = ListOptions::default();
    let mut operands = Vec::new();
    let mut parse_options = true;
    for argument in &command.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument.starts_with('-') && argument != "-" {
            for flag in argument[1..].chars() {
                match flag {
                    'a' => options.all = true,
                    'A' => options.almost_all = true,
                    'd' => options.directory = true,
                    'l' => options.long = true,
                    'F' => options.classify = true,
                    'R' => options.recursive = true,
                    '1' | 'h' => {}
                    _ => {
                        return Err(AppletError::usage(
                            "ls",
                            format!("unsupported flag: -{flag}"),
                        ));
                    }
                }
            }
        } else {
            operands.push(argument.as_str());
        }
    }
    if operands.is_empty() {
        operands.push(".");
    }

    let mut output = Vec::new();
    let mut visited = 0usize;
    let mut discovered = 0usize;
    for (index, operand) in operands.iter().enumerate() {
        let path = context.resolve_entry(&command.cwd, operand)?;
        if operands.len() > 1 {
            if index > 0 {
                push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
            }
            push_bounded(
                &mut output,
                format!("{operand}:\n").as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
        list_path(
            context,
            &path,
            operand,
            &options,
            0,
            &mut visited,
            &mut discovered,
            &mut output,
        )?;
    }
    Ok(AppletOutput::success(output))
}

#[allow(clippy::too_many_arguments)]
fn list_path(
    context: &AppletContext,
    path: &Path,
    display: &str,
    options: &ListOptions,
    depth: usize,
    visited: &mut usize,
    discovered: &mut usize,
    output: &mut Vec<u8>,
) -> Result<()> {
    count_entry(context, visited, depth)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| AppletError::io(display, error))?;
    if options.directory || !metadata.is_dir() {
        return render_entry(context, display, &metadata, options, output);
    }

    let entries = sorted_entries(context, path, display, discovered)?;
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !visible(&name, options) {
            continue;
        }
        count_entry(context, visited, depth + 1)?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|error| AppletError::io(&name, error))?;
        render_entry(context, &name, &metadata, options, output)?;
    }

    if options.recursive {
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !visible(&name, options) || matches!(name.as_str(), "." | "..") {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| AppletError::io(&name, error))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                let guest = context.display_guest_path(&entry.path())?;
                push_bounded(
                    output,
                    format!("\n{guest}:\n").as_bytes(),
                    context.limits().max_output_bytes,
                )?;
                list_path(
                    context,
                    &entry.path(),
                    &guest,
                    options,
                    depth + 1,
                    visited,
                    discovered,
                    output,
                )?;
            }
        }
    }
    Ok(())
}

fn visible(name: &str, options: &ListOptions) -> bool {
    if options.all {
        true
    } else if options.almost_all {
        !matches!(name, "." | "..")
    } else {
        !name.starts_with('.')
    }
}

fn render_entry(
    context: &AppletContext,
    display: &str,
    metadata: &fs::Metadata,
    options: &ListOptions,
    output: &mut Vec<u8>,
) -> Result<()> {
    let suffix = if options.classify {
        if metadata.is_dir() {
            "/"
        } else if metadata.file_type().is_symlink() {
            "@"
        } else {
            executable_suffix(metadata)
        }
    } else {
        ""
    };
    let line = if options.long {
        format!(
            "{} {:>10} {display}{suffix}\n",
            mode_string(metadata),
            metadata.len()
        )
    } else {
        format!("{display}{suffix}\n")
    };
    push_bounded(output, line.as_bytes(), context.limits().max_output_bytes)
}

#[derive(Default)]
struct FindOptions {
    max_depth: Option<usize>,
    min_depth: usize,
    name: Option<String>,
    kind: Option<char>,
    nul: bool,
}

const MAX_FIND_PATTERN_BYTES: usize = 4096;
const MAX_FIND_GLOB_STEPS: usize = 1_000_000;

fn find(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut roots = Vec::new();
    let mut index = 0;
    while let Some(argument) = command.args.get(index) {
        if argument.starts_with('-') {
            break;
        }
        roots.push(argument.as_str());
        index += 1;
    }
    if roots.is_empty() {
        roots.push(".");
    }

    let mut options = FindOptions::default();
    while let Some(argument) = command.args.get(index) {
        match argument.as_str() {
            "-maxdepth" => {
                options.max_depth = Some(parse_depth(command, index, "-maxdepth")?);
                index += 2;
            }
            "-mindepth" => {
                options.min_depth = parse_depth(command, index, "-mindepth")?;
                index += 2;
            }
            "-name" => {
                let pattern = command
                    .args
                    .get(index + 1)
                    .ok_or_else(|| AppletError::usage("find", "-name requires a pattern"))?;
                if pattern.len() > MAX_FIND_PATTERN_BYTES {
                    return Err(AppletError::usage(
                        "find",
                        format!("-name pattern exceeds {MAX_FIND_PATTERN_BYTES} bytes"),
                    ));
                }
                options.name = Some(pattern.clone());
                index += 2;
            }
            "-type" => {
                let value = command
                    .args
                    .get(index + 1)
                    .ok_or_else(|| AppletError::usage("find", "-type requires f, d or l"))?;
                let mut chars = value.chars();
                let kind = chars
                    .next()
                    .filter(|kind| matches!(kind, 'f' | 'd' | 'l'))
                    .filter(|_| chars.next().is_none())
                    .ok_or_else(|| AppletError::usage("find", "-type supports f, d or l"))?;
                options.kind = Some(kind);
                index += 2;
            }
            "-print" => index += 1,
            "-print0" => {
                options.nul = true;
                index += 1;
            }
            value => {
                return Err(AppletError::usage(
                    "find",
                    format!("unsupported predicate: {value}"),
                ));
            }
        }
    }

    let mut output = Vec::new();
    let mut visited = 0usize;
    let mut discovered = 0usize;
    let mut glob_steps = 0usize;
    for root in roots {
        let path = context.resolve_entry(&command.cwd, root)?;
        walk_find(
            context,
            &path,
            root,
            &options,
            0,
            &mut visited,
            &mut discovered,
            &mut glob_steps,
            &mut output,
        )?;
    }
    Ok(AppletOutput::success(output))
}

fn parse_depth(command: &GuestCommand, index: usize, flag: &str) -> Result<usize> {
    command
        .args
        .get(index + 1)
        .ok_or_else(|| AppletError::usage("find", format!("{flag} requires a value")))?
        .parse()
        .map_err(|_| AppletError::usage("find", format!("invalid depth for {flag}")))
}

#[allow(clippy::too_many_arguments)]
fn walk_find(
    context: &AppletContext,
    path: &Path,
    display: &str,
    options: &FindOptions,
    depth: usize,
    visited: &mut usize,
    discovered: &mut usize,
    glob_steps: &mut usize,
    output: &mut Vec<u8>,
) -> Result<()> {
    count_entry(context, visited, depth)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| AppletError::io(display, error))?;
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or(display);
    let name_matches = match &options.name {
        Some(pattern) => glob_matches(pattern, file_name, glob_steps)?,
        None => true,
    };
    let kind_matches = match options.kind {
        Some('f') => metadata.is_file(),
        Some('d') => metadata.is_dir(),
        Some('l') => metadata.file_type().is_symlink(),
        Some(_) => false,
        None => true,
    };
    if depth >= options.min_depth && name_matches && kind_matches {
        push_bounded(
            output,
            display.as_bytes(),
            context.limits().max_output_bytes,
        )?;
        push_bounded(
            output,
            if options.nul { b"\0" } else { b"\n" },
            context.limits().max_output_bytes,
        )?;
    }
    let below_max = match options.max_depth {
        Some(limit) => depth < limit,
        None => true,
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() && below_max {
        let entries = sorted_entries(context, path, display, discovered)?;
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_display = if display == "." {
                format!("./{name}")
            } else if display.ends_with('/') {
                format!("{display}{name}")
            } else {
                format!("{display}/{name}")
            };
            walk_find(
                context,
                &entry.path(),
                &child_display,
                options,
                depth + 1,
                visited,
                discovered,
                glob_steps,
                output,
            )?;
        }
    }
    Ok(())
}

fn sorted_entries(
    context: &AppletContext,
    path: &Path,
    display: &str,
    discovered: &mut usize,
) -> Result<Vec<fs::DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| AppletError::io(display, error))? {
        *discovered = discovered.saturating_add(1);
        if *discovered > context.limits().max_filesystem_entries {
            return Err(AppletError::EntryLimit {
                limit: context.limits().max_filesystem_entries,
            });
        }
        entries.push(entry.map_err(|error| AppletError::io(display, error))?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn glob_matches(pattern: &str, value: &str, work: &mut usize) -> Result<bool> {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut pattern_index = 0usize;
    let mut value_index = 0usize;
    let mut last_star = None;
    let mut star_value_index = 0usize;

    while value_index < value.len() {
        consume_glob_work(work)?;
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            last_star = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(star) = last_star {
            pattern_index = star + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return Ok(false);
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        consume_glob_work(work)?;
        pattern_index += 1;
    }
    Ok(pattern_index == pattern.len())
}

fn consume_glob_work(work: &mut usize) -> Result<()> {
    *work = work.checked_add(1).ok_or(AppletError::WorkLimit {
        limit: MAX_FIND_GLOB_STEPS,
    })?;
    if *work > MAX_FIND_GLOB_STEPS {
        return Err(AppletError::WorkLimit {
            limit: MAX_FIND_GLOB_STEPS,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn list_and_find_are_sorted_and_scoped() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("alpha"), b"one").unwrap();
        fs::create_dir(root.path().join("dir")).unwrap();
        fs::write(root.path().join("dir/beta"), b"two").unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();

        let ls = GuestCommand::new("ls", Vec::<String>::new());
        assert_eq!(execute(&context, &ls).unwrap().stdout, b"alpha\ndir\n");
        let find = GuestCommand::new("find", [".".to_owned(), "-type".to_owned(), "f".to_owned()]);
        assert_eq!(
            execute(&context, &find).unwrap().stdout,
            b"./alpha\n./dir/beta\n"
        );
    }

    #[test]
    fn find_glob_matching_has_explicit_pattern_and_work_bounds() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("alpha"), b"one").unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
        let oversized = GuestCommand::new(
            "find",
            [
                ".".to_owned(),
                "-name".to_owned(),
                "a".repeat(MAX_FIND_PATTERN_BYTES + 1),
            ],
        );
        assert!(matches!(
            execute(&context, &oversized),
            Err(AppletError::InvalidArguments { .. })
        ));

        let mut normal_work = 0;
        assert!(glob_matches("a*ha", "alpha", &mut normal_work).unwrap());
        assert!(glob_matches("a?pha", "alpha", &mut normal_work).unwrap());
        assert!(!glob_matches("*.txt", "alpha", &mut normal_work).unwrap());

        let mut work = MAX_FIND_GLOB_STEPS;
        assert!(matches!(
            glob_matches("*", "alpha", &mut work),
            Err(AppletError::WorkLimit {
                limit: MAX_FIND_GLOB_STEPS
            })
        ));
    }
}
