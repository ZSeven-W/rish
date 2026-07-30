use std::fs::{self, OpenOptions};
use std::io::Write;

use regex::bytes::{Regex, RegexBuilder};
use rish_core::GuestCommand;

use super::common::{ReadBudget, body_and_newline, read_inputs, simple_flags, usage};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

#[derive(Default)]
struct GrepOptions {
    extended: bool,
    fixed: bool,
    insensitive: bool,
    invert: bool,
    number: bool,
    count: bool,
    files: bool,
    files_without: bool,
    quiet: bool,
    whole: bool,
    word: bool,
    filename: Option<bool>,
    max: Option<usize>,
}

pub(super) fn grep(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut options = GrepOptions::default();
    let mut patterns = Vec::new();
    let mut pattern_files = Vec::new();
    let mut operands = Vec::new();
    parse_grep(
        command,
        &mut options,
        &mut patterns,
        &mut pattern_files,
        &mut operands,
    )?;
    if patterns.is_empty() && pattern_files.is_empty() {
        let pattern = operands
            .first()
            .ok_or_else(|| usage(command, "a pattern is required"))?
            .clone();
        patterns.push(pattern);
        operands.remove(0);
    }
    if options.files && options.files_without {
        return Err(usage(command, "-l and -L are mutually exclusive"));
    }
    if patterns.len() > context.limits().max_filesystem_entries {
        return Err(AppletError::EntryLimit {
            limit: context.limits().max_filesystem_entries,
        });
    }
    let mut budget = ReadBudget::default();
    if !pattern_files.is_empty() {
        for input in read_inputs(context, command, &pattern_files, &mut budget)? {
            for line in input.data.split_inclusive(|byte| *byte == b'\n') {
                if patterns.len() >= context.limits().max_filesystem_entries {
                    return Err(AppletError::EntryLimit {
                        limit: context.limits().max_filesystem_entries,
                    });
                }
                patterns.push(
                    std::str::from_utf8(body_and_newline(line).0)
                        .map_err(|_| usage(command, "pattern files must be UTF-8"))?
                        .to_owned(),
                );
            }
        }
    }
    let regex = build_regex(context, command, &patterns, &options)?;
    let inputs = read_inputs(context, command, &operands, &mut budget)?;
    let show_name = options.filename.unwrap_or(inputs.len() > 1);
    let mut output = Vec::new();
    let mut success = false;
    for input in inputs {
        let mut matches = 0usize;
        for (line_index, line) in input
            .data
            .split_inclusive(|byte| *byte == b'\n')
            .enumerate()
        {
            let body = body_and_newline(line).0;
            let selected =
                regex.as_ref().is_some_and(|regex| regex.is_match(body)) != options.invert;
            if !selected || options.max.is_some_and(|max| matches >= max) {
                continue;
            }
            matches += 1;
            if options.quiet {
                return Ok(AppletOutput::success(Vec::new()));
            }
            if options.count || options.files || options.files_without {
                continue;
            }
            if show_name {
                push_bounded(
                    &mut output,
                    format!("{}:", display_name(&input.name)).as_bytes(),
                    context.limits().max_output_bytes,
                )?;
            }
            if options.number {
                push_bounded(
                    &mut output,
                    format!("{}:", line_index + 1).as_bytes(),
                    context.limits().max_output_bytes,
                )?;
            }
            push_bounded(&mut output, body, context.limits().max_output_bytes)?;
            push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
        }
        if options.files || options.files_without {
            let print = (options.files && matches > 0) || (options.files_without && matches == 0);
            if print {
                success = true;
                push_bounded(
                    &mut output,
                    format!("{}\n", display_name(&input.name)).as_bytes(),
                    context.limits().max_output_bytes,
                )?;
            }
        } else {
            success |= matches > 0;
            if options.count {
                if show_name {
                    push_bounded(
                        &mut output,
                        format!("{}:", display_name(&input.name)).as_bytes(),
                        context.limits().max_output_bytes,
                    )?;
                }
                push_bounded(
                    &mut output,
                    format!("{matches}\n").as_bytes(),
                    context.limits().max_output_bytes,
                )?;
            }
        }
    }
    Ok(AppletOutput {
        exit_code: if success { 0 } else { 1 },
        stdout: output,
        stderr: Vec::new(),
    })
}

fn display_name(name: &str) -> &str {
    if name == "-" {
        "(standard input)"
    } else {
        name
    }
}

fn parse_grep(
    command: &GuestCommand,
    options: &mut GrepOptions,
    patterns: &mut Vec<String>,
    pattern_files: &mut Vec<String>,
    operands: &mut Vec<String>,
) -> Result<()> {
    let mut index = 0;
    let mut parsing = true;
    while index < command.args.len() {
        let arg = &command.args[index];
        if parsing && arg == "--" {
            parsing = false;
        } else if parsing && arg.starts_with("--") {
            match arg.as_str() {
                "--extended-regexp" => options.extended = true,
                "--fixed-strings" => options.fixed = true,
                "--ignore-case" => options.insensitive = true,
                "--invert-match" => options.invert = true,
                "--line-number" => options.number = true,
                "--count" => options.count = true,
                "--files-with-matches" => options.files = true,
                "--files-without-match" => options.files_without = true,
                "--quiet" | "--silent" => options.quiet = true,
                "--with-filename" => options.filename = Some(true),
                "--no-filename" => options.filename = Some(false),
                "--line-regexp" => options.whole = true,
                "--word-regexp" => options.word = true,
                "--regexp" | "--file" | "--max-count" => {
                    index += 1;
                    let value = command
                        .args
                        .get(index)
                        .ok_or_else(|| usage(command, format!("{arg} requires a value")))?;
                    set_grep_value(command, options, patterns, pattern_files, arg, value)?;
                }
                _ if arg.starts_with("--regexp=")
                    || arg.starts_with("--file=")
                    || arg.starts_with("--max-count=") =>
                {
                    let (name, value) = arg
                        .split_once('=')
                        .ok_or_else(|| usage(command, format!("invalid option: {arg}")))?;
                    set_grep_value(command, options, patterns, pattern_files, name, value)?;
                }
                _ => return Err(usage(command, format!("unknown option: {arg}"))),
            }
        } else if parsing && arg.starts_with('-') && arg != "-" {
            let bytes = arg.as_bytes();
            let mut offset = 1;
            while offset < bytes.len() {
                match bytes[offset] {
                    b'E' => options.extended = true,
                    b'F' => options.fixed = true,
                    b'i' => options.insensitive = true,
                    b'v' => options.invert = true,
                    b'n' => options.number = true,
                    b'c' => options.count = true,
                    b'l' => options.files = true,
                    b'L' => options.files_without = true,
                    b'q' => options.quiet = true,
                    b'H' => options.filename = Some(true),
                    b'h' => options.filename = Some(false),
                    b'x' => options.whole = true,
                    b'w' => options.word = true,
                    b'e' | b'f' | b'm' => {
                        let value = if offset + 1 < bytes.len() {
                            &arg[offset + 1..]
                        } else {
                            index += 1;
                            command.args.get(index).ok_or_else(|| {
                                usage(
                                    command,
                                    format!("-{} requires a value", bytes[offset] as char),
                                )
                            })?
                        };
                        match bytes[offset] {
                            b'e' => patterns.push(value.to_owned()),
                            b'f' => pattern_files.push(value.to_owned()),
                            _ => {
                                options.max = Some(parse_count(command, value)?);
                            }
                        }
                        break;
                    }
                    flag => {
                        return Err(usage(command, format!("unknown option: -{}", flag as char)));
                    }
                }
                offset += 1;
            }
        } else {
            operands.push(arg.clone());
        }
        index += 1;
    }
    Ok(())
}

fn set_grep_value(
    command: &GuestCommand,
    options: &mut GrepOptions,
    patterns: &mut Vec<String>,
    pattern_files: &mut Vec<String>,
    name: &str,
    value: &str,
) -> Result<()> {
    match name {
        "--regexp" => patterns.push(value.to_owned()),
        "--file" => pattern_files.push(value.to_owned()),
        "--max-count" => options.max = Some(parse_count(command, value)?),
        _ => return Err(usage(command, format!("unknown option: {name}"))),
    }
    Ok(())
}

fn parse_count(command: &GuestCommand, value: &str) -> Result<usize> {
    value
        .parse()
        .map_err(|_| usage(command, format!("invalid match count: {value}")))
}

fn build_regex(
    context: &AppletContext,
    command: &GuestCommand,
    patterns: &[String],
    options: &GrepOptions,
) -> Result<Option<Regex>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut expression = String::new();
    for (index, pattern) in patterns.iter().enumerate() {
        if pattern.len() > context.limits().max_input_bytes {
            return Err(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            });
        }
        let pattern = if options.fixed {
            regex::escape(pattern)
        } else if options.extended {
            pattern.clone()
        } else {
            basic_regex(pattern)
        };
        let addition = pattern
            .len()
            .checked_add(4)
            .and_then(|length| length.checked_add(usize::from(index > 0)))
            .ok_or(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            })?;
        if expression.len().saturating_add(addition) > context.limits().max_input_bytes {
            return Err(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            });
        }
        if index > 0 {
            expression.push('|');
        }
        expression.push_str("(?:");
        expression.push_str(&pattern);
        expression.push(')');
    }
    if options.whole {
        expression = format!("^(?:{expression})$");
    } else if options.word {
        expression = format!(r"\b(?:{expression})\b");
    }
    if expression.len() > context.limits().max_input_bytes {
        return Err(AppletError::InputLimit {
            limit: context.limits().max_input_bytes,
        });
    }
    let size = context
        .limits()
        .max_input_bytes
        .clamp(64 * 1024, 64 * 1024 * 1024);
    RegexBuilder::new(&expression)
        .case_insensitive(options.insensitive)
        .size_limit(size)
        .dfa_size_limit(size)
        .build()
        .map(Some)
        .map_err(|error| usage(command, format!("invalid pattern: {error}")))
}

fn basic_regex(pattern: &str) -> String {
    let mut output = String::with_capacity(pattern.len());
    let mut characters = pattern.chars().peekable();
    let mut bracket = false;
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(escaped) = characters.next() {
                if !bracket && matches!(escaped, '(' | ')' | '+' | '?' | '|' | '{' | '}') {
                    output.push(escaped);
                } else {
                    output.push('\\');
                    output.push(escaped);
                }
            } else {
                output.push('\\');
            }
        } else if character == '[' && !bracket {
            bracket = true;
            output.push(character);
        } else if character == ']' && bracket {
            bracket = false;
            output.push(character);
        } else if !bracket && matches!(character, '(' | ')' | '+' | '?' | '|' | '{' | '}') {
            output.push('\\');
            output.push(character);
        } else {
            output.push(character);
        }
    }
    output
}

pub(super) fn tee(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, files) = simple_flags(
        command,
        b"ai",
        &[("--append", b'a'), ("--ignore-interrupts", b'i')],
    )?;
    let append = flags[b'a' as usize];
    if files.len() > context.limits().max_filesystem_entries {
        return Err(AppletError::EntryLimit {
            limit: context.limits().max_filesystem_entries,
        });
    }
    if command.stdin.len() > context.limits().max_output_bytes {
        return Err(AppletError::OutputLimit {
            limit: context.limits().max_output_bytes,
        });
    }
    let write_bytes =
        command
            .stdin
            .len()
            .checked_mul(files.len())
            .ok_or(AppletError::OutputLimit {
                limit: context.limits().max_output_bytes,
            })?;
    if write_bytes > context.limits().max_output_bytes {
        return Err(AppletError::OutputLimit {
            limit: context.limits().max_output_bytes,
        });
    }
    for name in files {
        let path = match context.resolve_existing(&command.cwd, &name) {
            Ok(path) => {
                let metadata =
                    fs::symlink_metadata(&path).map_err(|error| AppletError::io(&name, error))?;
                if !metadata.is_file() {
                    return Err(AppletError::UnsafePath(format!(
                        "tee output must be a regular file: {name}"
                    )));
                }
                path
            }
            Err(AppletError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                context.resolve_for_create(&command.cwd, &name)?
            }
            Err(error) => return Err(error),
        };
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        if append {
            options.append(true);
        } else {
            options.truncate(true);
        }
        options
            .open(&path)
            .and_then(|mut file| file.write_all(&command.stdin))
            .map_err(|error| AppletError::io(&name, error))?;
    }
    Ok(AppletOutput::success(command.stdin.clone()))
}
