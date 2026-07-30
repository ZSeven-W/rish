use rish_core::GuestCommand;

use super::common::{ReadBudget, body_and_newline, read_inputs, simple_flags, usage};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(super) fn cat(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, files) = simple_flags(
        command,
        b"nbsETvAu",
        &[
            ("--number", b'n'),
            ("--number-nonblank", b'b'),
            ("--squeeze-blank", b's'),
            ("--show-ends", b'E'),
            ("--show-tabs", b'T'),
            ("--show-nonprinting", b'v'),
            ("--show-all", b'A'),
        ],
    )?;
    let number = flags[b'n' as usize];
    let nonblank = flags[b'b' as usize];
    let squeeze = flags[b's' as usize];
    let ends = flags[b'E' as usize] || flags[b'A' as usize];
    let tabs = flags[b'T' as usize] || flags[b'A' as usize];
    let nonprinting = flags[b'v' as usize] || flags[b'A' as usize];
    let inputs = read_inputs(context, command, &files, &mut ReadBudget::default())?;
    let mut output = Vec::new();
    let mut line_number = 1usize;
    let mut previous_blank = false;
    for input in inputs {
        for line in input.data.split_inclusive(|byte| *byte == b'\n') {
            let (body, newline) = body_and_newline(line);
            let blank = newline && body.is_empty();
            if squeeze && blank && previous_blank {
                continue;
            }
            previous_blank = blank;
            if (number && !nonblank) || (nonblank && !body.is_empty()) {
                push_bounded(
                    &mut output,
                    format!("{line_number:>6}\t").as_bytes(),
                    context.limits().max_output_bytes,
                )?;
                line_number = line_number.checked_add(1).ok_or(AppletError::OutputLimit {
                    limit: context.limits().max_output_bytes,
                })?;
            }
            for &byte in body {
                write_cat_byte(context, &mut output, byte, tabs, nonprinting)?;
            }
            if newline {
                if ends {
                    push_bounded(&mut output, b"$", context.limits().max_output_bytes)?;
                }
                push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
            }
        }
    }
    Ok(AppletOutput::success(output))
}

fn write_cat_byte(
    context: &AppletContext,
    output: &mut Vec<u8>,
    byte: u8,
    tabs: bool,
    nonprinting: bool,
) -> Result<()> {
    let limit = context.limits().max_output_bytes;
    if byte == b'\t' {
        return push_bounded(output, if tabs { b"^I" } else { b"\t" }, limit);
    }
    if !nonprinting {
        return push_bounded(output, &[byte], limit);
    }
    let (meta, value) = if byte >= 128 {
        (true, byte - 128)
    } else {
        (false, byte)
    };
    if meta {
        push_bounded(output, b"M-", limit)?;
    }
    match value {
        0..=31 => push_bounded(output, &[b'^', value + 64], limit),
        127 => push_bounded(output, b"^?", limit),
        _ => push_bounded(output, &[value], limit),
    }
}

#[derive(Clone, Copy)]
enum Count {
    End(usize),
    Start(usize),
}

fn parse_count(command: &GuestCommand, value: &str, allow_start: bool) -> Result<Count> {
    let (start, digits) = if allow_start {
        value
            .strip_prefix('+')
            .map_or((false, value), |rest| (true, rest))
    } else {
        (false, value)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(usage(command, format!("invalid count: {value}")));
    }
    let count = digits
        .parse::<usize>()
        .map_err(|_| usage(command, format!("count is too large: {value}")))?;
    Ok(if start {
        Count::Start(count.max(1))
    } else {
        Count::End(count)
    })
}

pub(super) fn head_tail(
    context: &AppletContext,
    command: &GuestCommand,
    tail: bool,
) -> Result<AppletOutput> {
    let mut count = Count::End(10);
    let mut bytes = false;
    let mut quiet = false;
    let mut verbose = false;
    let mut files = Vec::new();
    let mut index = 0;
    let mut options = true;
    while index < command.args.len() {
        let arg = &command.args[index];
        if options && arg == "--" {
            options = false;
        } else if options && matches!(arg.as_str(), "-q" | "--quiet" | "--silent") {
            quiet = true;
        } else if options && matches!(arg.as_str(), "-v" | "--verbose") {
            verbose = true;
        } else if options && matches!(arg.as_str(), "-n" | "--lines" | "-c" | "--bytes") {
            bytes = matches!(arg.as_str(), "-c" | "--bytes");
            index += 1;
            let value = command
                .args
                .get(index)
                .ok_or_else(|| usage(command, format!("{arg} requires a count")))?;
            count = parse_count(command, value, tail)?;
        } else if options && (arg.starts_with("--lines=") || arg.starts_with("--bytes=")) {
            bytes = arg.starts_with("--bytes=");
            let value = arg
                .split_once('=')
                .map(|(_, value)| value)
                .ok_or_else(|| usage(command, format!("invalid option: {arg}")))?;
            count = parse_count(command, value, tail)?;
        } else if options && (arg.starts_with("-n") || arg.starts_with("-c")) && arg.len() > 2 {
            bytes = arg.starts_with("-c");
            count = parse_count(command, &arg[2..], tail)?;
        } else if options
            && arg.len() > 1
            && arg[1..].bytes().all(|flag| matches!(flag, b'q' | b'v'))
        {
            quiet |= arg.as_bytes()[1..].contains(&b'q');
            verbose |= arg.as_bytes()[1..].contains(&b'v');
        } else if options && arg.len() > 1 && arg[1..].bytes().all(|byte| byte.is_ascii_digit()) {
            count = parse_count(command, &arg[1..], false)?;
        } else if options && arg.starts_with('-') && arg != "-" {
            return Err(usage(command, format!("unknown option: {arg}")));
        } else {
            files.push(arg.clone());
        }
        index += 1;
    }
    let inputs = read_inputs(context, command, &files, &mut ReadBudget::default())?;
    let headers = !quiet && (verbose || inputs.len() > 1);
    let mut output = Vec::new();
    for (input_index, input) in inputs.iter().enumerate() {
        if headers {
            if input_index > 0 {
                push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
            }
            let name = if input.name == "-" {
                "standard input"
            } else {
                &input.name
            };
            push_bounded(
                &mut output,
                format!("==> {name} <==\n").as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
        push_bounded(
            &mut output,
            select(&input.data, count, bytes, tail),
            context.limits().max_output_bytes,
        )?;
    }
    Ok(AppletOutput::success(output))
}

fn select(data: &[u8], count: Count, bytes: bool, tail: bool) -> &[u8] {
    match (tail, count, bytes) {
        (false, Count::End(count), true) => &data[..data.len().min(count)],
        (false, Count::End(count), false) => {
            let end = data
                .split_inclusive(|byte| *byte == b'\n')
                .take(count)
                .map(<[u8]>::len)
                .sum();
            &data[..end]
        }
        (true, Count::End(0), _) => &data[data.len()..],
        (true, Count::End(count), true) => &data[data.len().saturating_sub(count)..],
        (true, Count::End(count), false) => {
            let scan_end = data.len() - usize::from(data.ends_with(b"\n"));
            let mut seen = 0;
            for index in (0..scan_end).rev() {
                if data[index] == b'\n' {
                    seen += 1;
                    if seen == count {
                        return &data[index + 1..];
                    }
                }
            }
            data
        }
        (true, Count::Start(count), true) => &data[(count - 1).min(data.len())..],
        (true, Count::Start(count), false) => {
            let mut start = 0;
            for _ in 1..count {
                let Some(next) = data[start..].iter().position(|byte| *byte == b'\n') else {
                    return &data[data.len()..];
                };
                start += next + 1;
            }
            &data[start..]
        }
        (false, Count::Start(_), _) => data,
    }
}
