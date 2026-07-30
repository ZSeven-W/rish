use std::cmp::Ordering;

use rish_core::GuestCommand;

use super::common::{ReadBudget, body_and_newline, read_inputs, simple_flags, usage};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(super) fn wc(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, files) = simple_flags(
        command,
        b"lwmcL",
        &[
            ("--lines", b'l'),
            ("--words", b'w'),
            ("--chars", b'm'),
            ("--bytes", b'c'),
            ("--max-line-length", b'L'),
        ],
    )?;
    let mut selected = [false; 5]; // lines, words, chars, bytes, max line
    for (index, flag) in [b'l', b'w', b'm', b'c', b'L'].iter().enumerate() {
        selected[index] = flags[*flag as usize];
    }
    if !selected.iter().any(|value| *value) {
        selected[0] = true;
        selected[1] = true;
        selected[3] = true;
    }
    let explicit = !files.is_empty();
    let inputs = read_inputs(context, command, &files, &mut ReadBudget::default())?;
    let mut output = Vec::new();
    let mut totals = [0usize; 5];
    for input in &inputs {
        let counts = counts(&input.data);
        for index in 0..4 {
            totals[index] =
                totals[index]
                    .checked_add(counts[index])
                    .ok_or(AppletError::InputLimit {
                        limit: context.limits().max_input_bytes,
                    })?;
        }
        totals[4] = totals[4].max(counts[4]);
        write_counts(
            &mut output,
            context,
            &selected,
            &counts,
            explicit.then_some(input.name.as_str()),
        )?;
    }
    if inputs.len() > 1 {
        write_counts(&mut output, context, &selected, &totals, Some("total"))?;
    }
    Ok(AppletOutput::success(output))
}

fn counts(data: &[u8]) -> [usize; 5] {
    let lines = data.iter().filter(|byte| **byte == b'\n').count();
    let words = data
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|word| !word.is_empty())
        .count();
    let mut characters = 0;
    let mut rest = data;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                characters += text.chars().count();
                break;
            }
            Err(error) => {
                characters += std::str::from_utf8(&rest[..error.valid_up_to()])
                    .map_or(0, |text| text.chars().count());
                characters += 1;
                let consumed = error
                    .error_len()
                    .map_or(rest.len(), |length| error.valid_up_to() + length);
                rest = &rest[consumed..];
            }
        }
    }
    let max_line = data
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0);
    [lines, words, characters, data.len(), max_line]
}

fn write_counts(
    output: &mut Vec<u8>,
    context: &AppletContext,
    selected: &[bool; 5],
    counts: &[usize; 5],
    name: Option<&str>,
) -> Result<()> {
    let mut text = selected
        .iter()
        .zip(counts)
        .filter(|(show, _)| **show)
        .map(|(_, value)| value.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(name) = name {
        text.push(' ');
        text.push_str(name);
    }
    text.push('\n');
    push_bounded(output, text.as_bytes(), context.limits().max_output_bytes)
}

pub(super) fn sort(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, files) = simple_flags(
        command,
        b"rnufb",
        &[
            ("--reverse", b'r'),
            ("--numeric-sort", b'n'),
            ("--unique", b'u'),
            ("--ignore-case", b'f'),
            ("--ignore-leading-blanks", b'b'),
        ],
    )?;
    let reverse = flags[b'r' as usize];
    let numeric = flags[b'n' as usize];
    let unique = flags[b'u' as usize];
    let fold = flags[b'f' as usize];
    let blanks = flags[b'b' as usize];
    let inputs = read_inputs(context, command, &files, &mut ReadBudget::default())?;
    let mut lines = Vec::new();
    for input in &inputs {
        for line in input.data.split_inclusive(|byte| *byte == b'\n') {
            if lines.len() >= context.limits().max_filesystem_entries {
                return Err(AppletError::EntryLimit {
                    limit: context.limits().max_filesystem_entries,
                });
            }
            lines.push(body_and_newline(line).0);
        }
    }
    lines.sort_by(|left, right| {
        let ordering = compare(left, right, numeric, fold, blanks);
        if reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let mut output = Vec::new();
    let mut previous: Option<&[u8]> = None;
    for line in lines {
        if unique
            && previous.is_some_and(|value| compare(value, line, numeric, fold, blanks).is_eq())
        {
            continue;
        }
        push_bounded(&mut output, line, context.limits().max_output_bytes)?;
        push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
        previous = Some(line);
    }
    Ok(AppletOutput::success(output))
}

fn compare(left: &[u8], right: &[u8], numeric: bool, fold: bool, blanks: bool) -> Ordering {
    let left = if blanks {
        left.trim_ascii_start()
    } else {
        left
    };
    let right = if blanks {
        right.trim_ascii_start()
    } else {
        right
    };
    if numeric {
        let number = |value: &[u8]| {
            std::str::from_utf8(value)
                .ok()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        number(left).total_cmp(&number(right))
    } else if fold {
        left.iter()
            .map(u8::to_ascii_lowercase)
            .cmp(right.iter().map(u8::to_ascii_lowercase))
    } else {
        left.cmp(right)
    }
}

pub(super) fn uniq(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let (flags, files) = simple_flags(
        command,
        b"cdui",
        &[
            ("--count", b'c'),
            ("--repeated", b'd'),
            ("--unique", b'u'),
            ("--ignore-case", b'i'),
        ],
    )?;
    let count = flags[b'c' as usize];
    let repeated = flags[b'd' as usize];
    let unique_only = flags[b'u' as usize];
    let fold = flags[b'i' as usize];
    if files.len() > 1 {
        return Err(usage(command, "output operands are not supported"));
    }
    let inputs = read_inputs(context, command, &files, &mut ReadBudget::default())?;
    let data = &inputs
        .first()
        .ok_or_else(|| usage(command, "an input stream is required"))?
        .data;
    let mut output = Vec::new();
    let mut group: Option<&[u8]> = None;
    let mut occurrences = 0usize;
    for line in data.split_inclusive(|byte| *byte == b'\n') {
        let same = group.is_some_and(|previous| {
            let previous = body_and_newline(previous).0;
            let current = body_and_newline(line).0;
            if fold {
                compare(previous, current, false, true, false).is_eq()
            } else {
                previous == current
            }
        });
        if !same {
            if let Some(previous) = group {
                emit_uniq(
                    context,
                    &mut output,
                    previous,
                    occurrences,
                    count,
                    repeated,
                    unique_only,
                )?;
            }
            group = Some(line);
            occurrences = 1;
        } else {
            occurrences = occurrences.checked_add(1).ok_or(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            })?;
        }
    }
    if let Some(previous) = group {
        emit_uniq(
            context,
            &mut output,
            previous,
            occurrences,
            count,
            repeated,
            unique_only,
        )?;
    }
    Ok(AppletOutput::success(output))
}

fn emit_uniq(
    context: &AppletContext,
    output: &mut Vec<u8>,
    line: &[u8],
    occurrences: usize,
    count: bool,
    repeated: bool,
    unique_only: bool,
) -> Result<()> {
    if (repeated && occurrences == 1) || (unique_only && !repeated && occurrences != 1) {
        return Ok(());
    }
    if count {
        push_bounded(
            output,
            format!("{occurrences:>7} ").as_bytes(),
            context.limits().max_output_bytes,
        )?;
    }
    push_bounded(output, line, context.limits().max_output_bytes)
}
