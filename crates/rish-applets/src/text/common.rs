use std::fs::{self, File};
use std::io::Read;

use rish_core::GuestCommand;

use crate::{AppletContext, AppletError, Result};

pub(super) fn usage(command: &GuestCommand, message: impl Into<String>) -> AppletError {
    AppletError::usage(command.basename(), message)
}

pub(super) fn simple_flags(
    command: &GuestCommand,
    allowed: &[u8],
    long: &[(&str, u8)],
) -> Result<([bool; 128], Vec<String>)> {
    let mut flags = [false; 128];
    let mut operands = Vec::new();
    let mut options = true;
    for arg in &command.args {
        if options && arg == "--" {
            options = false;
        } else if options && arg.starts_with("--") {
            let flag = long
                .iter()
                .find_map(|(name, flag)| (*name == arg).then_some(*flag))
                .ok_or_else(|| usage(command, format!("unknown option: {arg}")))?;
            flags[flag as usize] = true;
        } else if options && arg.starts_with('-') && arg != "-" {
            for &flag in &arg.as_bytes()[1..] {
                if !flag.is_ascii() || !allowed.contains(&flag) {
                    return Err(usage(command, format!("unknown option: -{}", flag as char)));
                }
                flags[flag as usize] = true;
            }
        } else {
            operands.push(arg.clone());
        }
    }
    Ok((flags, operands))
}

#[derive(Default)]
pub(super) struct ReadBudget {
    bytes: usize,
    paths: usize,
    stdin_used: bool,
}

pub(super) struct Input {
    pub name: String,
    pub data: Vec<u8>,
}

pub(super) fn read_inputs(
    context: &AppletContext,
    command: &GuestCommand,
    names: &[String],
    budget: &mut ReadBudget,
) -> Result<Vec<Input>> {
    let implicit = names.is_empty();
    let stdin = "-".to_owned();
    let sources = if implicit {
        std::slice::from_ref(&stdin)
    } else {
        names
    };
    let mut inputs = Vec::with_capacity(sources.len().min(context.limits().max_filesystem_entries));
    for name in sources {
        let data = if name == "-" {
            if budget.stdin_used {
                Vec::new()
            } else {
                budget.stdin_used = true;
                command.stdin.clone()
            }
        } else {
            budget.paths = budget.paths.checked_add(1).ok_or(AppletError::EntryLimit {
                limit: context.limits().max_filesystem_entries,
            })?;
            if budget.paths > context.limits().max_filesystem_entries {
                return Err(AppletError::EntryLimit {
                    limit: context.limits().max_filesystem_entries,
                });
            }
            let path = context.resolve_existing(&command.cwd, name)?;
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| AppletError::io(name, error))?;
            if !metadata.is_file() {
                return Err(AppletError::UnsafePath(format!(
                    "text input must be a regular file: {name}"
                )));
            }
            let remaining = context
                .limits()
                .max_input_bytes
                .checked_sub(budget.bytes)
                .ok_or(AppletError::InputLimit {
                    limit: context.limits().max_input_bytes,
                })?;
            let read_limit = u64::try_from(remaining)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut data = Vec::with_capacity(remaining.min(64 * 1024));
            File::open(&path)
                .map_err(|error| AppletError::io(name, error))?
                .take(read_limit)
                .read_to_end(&mut data)
                .map_err(|error| AppletError::io(name, error))?;
            if data.len() > remaining {
                return Err(AppletError::InputLimit {
                    limit: context.limits().max_input_bytes,
                });
            }
            data
        };
        budget.bytes = budget
            .bytes
            .checked_add(data.len())
            .ok_or(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            })?;
        if budget.bytes > context.limits().max_input_bytes {
            return Err(AppletError::InputLimit {
                limit: context.limits().max_input_bytes,
            });
        }
        inputs.push(Input {
            name: name.to_owned(),
            data,
        });
    }
    Ok(inputs)
}

pub(super) fn body_and_newline(line: &[u8]) -> (&[u8], bool) {
    line.strip_suffix(b"\n")
        .map_or((line, false), |body| (body, true))
}
