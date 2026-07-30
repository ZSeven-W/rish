use rish_core::GuestCommand;

use super::common::{ReadBudget, body_and_newline, read_inputs, simple_flags, usage};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

#[derive(Clone, Copy)]
enum CutMode {
    Bytes,
    Characters,
    Fields,
}

pub(super) fn cut(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut mode = None;
    let mut list = None;
    let mut delimiter = b'\t';
    let mut output_delimiter = None;
    let mut suppress = false;
    let mut complement = false;
    let mut files = Vec::new();
    let mut index = 0;
    let mut options = true;
    while index < command.args.len() {
        let arg = &command.args[index];
        if options && arg == "--" {
            options = false;
        } else if options && matches!(arg.as_str(), "-s" | "--only-delimited") {
            suppress = true;
        } else if options && arg == "--complement" {
            complement = true;
        } else if options && (arg == "-d" || arg == "--delimiter" || arg.starts_with("-d")) {
            let value = if let Some(value) = arg.strip_prefix("-d").filter(|v| !v.is_empty()) {
                value
            } else {
                index += 1;
                command
                    .args
                    .get(index)
                    .ok_or_else(|| usage(command, format!("{arg} requires a delimiter")))?
            };
            delimiter = one_byte(command, value)?;
        } else if options && arg.starts_with("--delimiter=") {
            let value = arg
                .split_once('=')
                .map(|(_, value)| value)
                .ok_or_else(|| usage(command, format!("invalid option: {arg}")))?;
            delimiter = one_byte(command, value)?;
        } else if options && matches!(arg.as_str(), "--output-delimiter") {
            index += 1;
            let value = command
                .args
                .get(index)
                .ok_or_else(|| usage(command, "--output-delimiter requires a value"))?;
            output_delimiter = Some(bounded_delimiter(context, value)?);
        } else if options && arg.starts_with("--output-delimiter=") {
            let value = arg
                .split_once('=')
                .map(|(_, value)| value)
                .ok_or_else(|| usage(command, format!("invalid option: {arg}")))?;
            output_delimiter = Some(bounded_delimiter(context, value)?);
        } else if options {
            if let Some((new_mode, attached)) = cut_mode(arg) {
                if mode.is_some() {
                    return Err(usage(command, "exactly one of -b, -c, or -f is required"));
                }
                mode = Some(new_mode);
                let value = if let Some(value) = attached {
                    value
                } else {
                    index += 1;
                    command
                        .args
                        .get(index)
                        .ok_or_else(|| usage(command, format!("{arg} requires a list")))?
                };
                list = Some(parse_ranges(context, command, value)?);
            } else if arg.starts_with('-') && arg != "-" {
                return Err(usage(command, format!("unknown option: {arg}")));
            } else {
                files.push(arg.clone());
            }
        } else {
            files.push(arg.clone());
        }
        index += 1;
    }
    let mode = mode.ok_or_else(|| usage(command, "one of -b, -c, or -f is required"))?;
    if suppress && !matches!(mode, CutMode::Fields) {
        return Err(usage(command, "-s is only valid with -f"));
    }
    let ranges = list.ok_or_else(|| usage(command, "a selection list is required"))?;
    let inputs = read_inputs(context, command, &files, &mut ReadBudget::default())?;
    let mut output = Vec::new();
    for input in inputs {
        for line in input.data.split_inclusive(|byte| *byte == b'\n') {
            let (body, newline) = body_and_newline(line);
            if matches!(mode, CutMode::Fields) && !body.contains(&delimiter) {
                if suppress {
                    continue;
                }
                push_bounded(&mut output, body, context.limits().max_output_bytes)?;
            } else {
                emit_cut(
                    context,
                    command,
                    &mut output,
                    body,
                    mode,
                    &ranges,
                    complement,
                    delimiter,
                    output_delimiter.as_deref(),
                )?;
            }
            if newline {
                push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
            }
        }
    }
    Ok(AppletOutput::success(output))
}

fn cut_mode(arg: &str) -> Option<(CutMode, Option<&str>)> {
    for (short, long, mode) in [
        ("-b", "--bytes", CutMode::Bytes),
        ("-c", "--characters", CutMode::Characters),
        ("-f", "--fields", CutMode::Fields),
    ] {
        if arg == short || arg == long {
            return Some((mode, None));
        }
        if let Some(value) = arg.strip_prefix(short).filter(|value| !value.is_empty()) {
            return Some((mode, Some(value)));
        }
        if let Some(value) = arg
            .strip_prefix(long)
            .and_then(|value| value.strip_prefix('='))
        {
            return Some((mode, Some(value)));
        }
    }
    None
}

fn one_byte(command: &GuestCommand, value: &str) -> Result<u8> {
    if value.len() != 1 {
        return Err(usage(command, "delimiter must be exactly one byte"));
    }
    Ok(value.as_bytes()[0])
}

fn bounded_delimiter(context: &AppletContext, value: &str) -> Result<Vec<u8>> {
    if value.len() > context.limits().max_output_bytes {
        return Err(AppletError::OutputLimit {
            limit: context.limits().max_output_bytes,
        });
    }
    Ok(value.as_bytes().to_vec())
}

fn parse_ranges(
    context: &AppletContext,
    command: &GuestCommand,
    value: &str,
) -> Result<Vec<(usize, usize)>> {
    if value.len() > context.limits().max_input_bytes {
        return Err(AppletError::InputLimit {
            limit: context.limits().max_input_bytes,
        });
    }
    let mut ranges = Vec::new();
    for item in value.split(',') {
        if ranges.len() >= context.limits().max_filesystem_entries {
            return Err(AppletError::EntryLimit {
                limit: context.limits().max_filesystem_entries,
            });
        }
        let (start, end) = if let Some((left, right)) = item.split_once('-') {
            let start = if left.is_empty() {
                1
            } else {
                positive(command, left)?
            };
            let end = if right.is_empty() {
                usize::MAX
            } else {
                positive(command, right)?
            };
            (start, end)
        } else {
            let point = positive(command, item)?;
            (point, point)
        };
        if end < start {
            return Err(usage(command, format!("descending range: {item}")));
        }
        ranges.push((start, end));
    }
    if ranges.is_empty() {
        return Err(usage(command, "selection list cannot be empty"));
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1.saturating_add(1) => {
                last.1 = last.1.max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    Ok(merged)
}

fn positive(command: &GuestCommand, value: &str) -> Result<usize> {
    let number = value
        .parse::<usize>()
        .map_err(|_| usage(command, format!("invalid position: {value}")))?;
    if number == 0 {
        return Err(usage(command, "positions are numbered from 1"));
    }
    Ok(number)
}

#[allow(clippy::too_many_arguments)]
fn emit_cut(
    context: &AppletContext,
    command: &GuestCommand,
    output: &mut Vec<u8>,
    body: &[u8],
    mode: CutMode,
    ranges: &[(usize, usize)],
    complement: bool,
    delimiter: u8,
    output_delimiter: Option<&[u8]>,
) -> Result<()> {
    let selected = |position: usize| {
        let index = ranges.partition_point(|(_, end)| *end < position);
        complement
            != ranges
                .get(index)
                .is_some_and(|(start, end)| position >= *start && position <= *end)
    };
    match mode {
        CutMode::Bytes => {
            for (index, byte) in body.iter().enumerate() {
                if selected(index + 1) {
                    push_bounded(output, &[*byte], context.limits().max_output_bytes)?;
                }
            }
        }
        CutMode::Characters => {
            let text = std::str::from_utf8(body)
                .map_err(|_| usage(command, "-c requires valid UTF-8 input"))?;
            for (index, character) in text.chars().enumerate() {
                if selected(index + 1) {
                    let mut encoded = [0; 4];
                    push_bounded(
                        output,
                        character.encode_utf8(&mut encoded).as_bytes(),
                        context.limits().max_output_bytes,
                    )?;
                }
            }
        }
        CutMode::Fields => {
            let default_separator = [delimiter];
            let separator = output_delimiter.unwrap_or(&default_separator);
            let mut wrote = false;
            for (index, field) in body.split(|byte| *byte == delimiter).enumerate() {
                if selected(index + 1) {
                    if wrote {
                        push_bounded(output, separator, context.limits().max_output_bytes)?;
                    }
                    push_bounded(output, field, context.limits().max_output_bytes)?;
                    wrote = true;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn tr(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, sets) = simple_flags(
        command,
        b"dscCt",
        &[
            ("--delete", b'd'),
            ("--squeeze-repeats", b's'),
            ("--complement", b'c'),
            ("--truncate-set1", b't'),
        ],
    )?;
    let delete = flags[b'd' as usize];
    let squeeze = flags[b's' as usize];
    let complement = flags[b'c' as usize] || flags[b'C' as usize];
    let truncate = flags[b't' as usize];
    let valid_arity = match (delete, squeeze) {
        (false, false) => sets.len() == 2,
        (false, true) => (1..=2).contains(&sets.len()),
        (true, false) => sets.len() == 1,
        (true, true) => sets.len() == 2,
    };
    if !valid_arity {
        return Err(usage(command, "invalid number of character sets"));
    }
    let first_value = sets
        .first()
        .ok_or_else(|| usage(command, "SET1 is required"))?;
    let mut first = expand_set(context, command, first_value)?;
    if complement {
        let mut present = [false; 256];
        for byte in &first {
            present[*byte as usize] = true;
        }
        first = (0u8..=u8::MAX)
            .filter(|byte| !present[*byte as usize])
            .collect();
    }
    let second = if sets.len() == 2 {
        Some(expand_set(
            context,
            command,
            sets.get(1)
                .ok_or_else(|| usage(command, "SET2 is required"))?,
        )?)
    } else {
        None
    };
    if !delete && second.as_ref().is_some_and(Vec::is_empty) {
        return Err(usage(command, "SET2 cannot be empty"));
    }
    let mut table: [u8; 256] = std::array::from_fn(|byte| byte as u8);
    if !delete {
        if let Some(second) = &second {
            let first = if truncate && second.len() < first.len() {
                &first[..second.len()]
            } else {
                &first
            };
            for (index, byte) in first.iter().enumerate() {
                table[*byte as usize] = second[index.min(second.len() - 1)];
            }
        }
    }
    let mut deleted = [false; 256];
    if delete {
        for byte in &first {
            deleted[*byte as usize] = true;
        }
    }
    let squeeze_set: &[u8] = if squeeze {
        second.as_deref().unwrap_or(&first)
    } else {
        &[]
    };
    let mut squeezed = [false; 256];
    for byte in squeeze_set {
        squeezed[*byte as usize] = true;
    }
    let mut output = Vec::with_capacity(command.stdin.len().min(context.limits().max_output_bytes));
    let mut previous = None;
    for &byte in &command.stdin {
        if deleted[byte as usize] {
            continue;
        }
        let translated = table[byte as usize];
        if squeeze && previous == Some(translated) && squeezed[translated as usize] {
            continue;
        }
        push_bounded(
            &mut output,
            &[translated],
            context.limits().max_output_bytes,
        )?;
        previous = Some(translated);
    }
    Ok(AppletOutput::success(output))
}

fn expand_set(context: &AppletContext, command: &GuestCommand, value: &str) -> Result<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"[:") {
            let end = bytes[index + 2..]
                .windows(2)
                .position(|pair| pair == b":]")
                .ok_or_else(|| usage(command, "unterminated character class"))?
                + index
                + 2;
            append_class(command, &mut output, &value[index + 2..end])?;
            index = end + 2;
        } else {
            let (start, used) = set_byte(command, &bytes[index..])?;
            index += used;
            if index < bytes.len() && bytes[index] == b'-' && index + 1 < bytes.len() {
                let (end, used) = set_byte(command, &bytes[index + 1..])?;
                if end < start {
                    return Err(usage(command, "descending character range"));
                }
                output.extend(start..=end);
                index += used + 1;
            } else {
                output.push(start);
            }
        }
        if output.len() > context.limits().max_input_bytes {
            return Err(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            });
        }
    }
    Ok(output)
}

fn set_byte(command: &GuestCommand, bytes: &[u8]) -> Result<(u8, usize)> {
    let first = *bytes
        .first()
        .ok_or_else(|| usage(command, "empty character set atom"))?;
    if first != b'\\' {
        return Ok((first, 1));
    }
    let Some(&escaped) = bytes.get(1) else {
        return Err(usage(command, "trailing backslash in character set"));
    };
    let value = match escaped {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'b' => 8,
        b'f' => 12,
        b'v' => 11,
        b'0'..=b'7' => {
            let mut value = 0u16;
            let mut used = 1;
            for &digit in bytes[1..].iter().take(3) {
                if !(b'0'..=b'7').contains(&digit) {
                    break;
                }
                value = value * 8 + u16::from(digit - b'0');
                used += 1;
            }
            if value > 255 {
                return Err(usage(command, "octal escape exceeds one byte"));
            }
            return Ok((value as u8, used));
        }
        value => value,
    };
    Ok((value, 2))
}

fn append_class(command: &GuestCommand, output: &mut Vec<u8>, name: &str) -> Result<()> {
    match name {
        "lower" => output.extend(b'a'..=b'z'),
        "upper" => output.extend(b'A'..=b'Z'),
        "digit" => output.extend(b'0'..=b'9'),
        "alpha" => {
            output.extend(b'A'..=b'Z');
            output.extend(b'a'..=b'z');
        }
        "alnum" => {
            output.extend(b'0'..=b'9');
            output.extend(b'A'..=b'Z');
            output.extend(b'a'..=b'z');
        }
        "blank" => output.extend_from_slice(b" \t"),
        "space" => output.extend_from_slice(b" \t\r\n\x0b\x0c"),
        "xdigit" => output.extend_from_slice(b"0123456789ABCDEFabcdef"),
        _ => {
            return Err(usage(
                command,
                format!("unsupported character class: {name}"),
            ));
        }
    }
    Ok(())
}
