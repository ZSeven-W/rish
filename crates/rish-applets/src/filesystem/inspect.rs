use std::fs;

use rish_core::GuestCommand;

use super::common::{file_kind, unix_mode};
use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(super) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "readlink" => read_link(context, command),
        "realpath" => realpath(context, command),
        "stat" => stat(context, command),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}

fn read_link(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut newline = true;
    let mut operand = None;
    for argument in &command.args {
        match argument.as_str() {
            "-n" => newline = false,
            value if value.starts_with('-') => {
                return Err(AppletError::usage(
                    "readlink",
                    format!("unsupported flag: {value}"),
                ));
            }
            value if operand.is_none() => operand = Some(value),
            _ => return Err(AppletError::usage("readlink", "too many operands")),
        }
    }
    let operand = operand.ok_or_else(|| AppletError::usage("readlink", "missing operand"))?;
    let path = context.resolve_entry(&command.cwd, operand)?;
    let target = fs::read_link(&path).map_err(|error| AppletError::io(operand, error))?;
    let mut output = target.as_os_str().as_encoded_bytes().to_vec();
    if newline {
        output.push(b'\n');
    }
    Ok(AppletOutput::success(output))
}

fn realpath(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    if command.args.is_empty() || command.args.iter().any(|value| value.starts_with('-')) {
        return Err(AppletError::usage("realpath", "usage: realpath PATH..."));
    }
    let mut output = Vec::new();
    for operand in &command.args {
        let entry = context.resolve_entry(&command.cwd, operand)?;
        let canonical =
            fs::canonicalize(&entry).map_err(|error| AppletError::io(operand, error))?;
        let guest = context.display_guest_path(&canonical)?;
        push_bounded(
            &mut output,
            format!("{guest}\n").as_bytes(),
            context.limits().max_output_bytes,
        )?;
    }
    Ok(AppletOutput::success(output))
}

fn stat(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut format = None;
    let mut operands = Vec::new();
    let mut index = 0;
    while let Some(argument) = command.args.get(index) {
        if argument == "-c" || argument == "--format" {
            format = Some(
                command
                    .args
                    .get(index + 1)
                    .ok_or_else(|| AppletError::usage("stat", "-c requires a format"))?
                    .as_str(),
            );
            index += 2;
        } else if argument.starts_with('-') {
            return Err(AppletError::usage(
                "stat",
                format!("unsupported flag: {argument}"),
            ));
        } else {
            operands.push(argument.as_str());
            index += 1;
        }
    }
    if operands.is_empty() {
        return Err(AppletError::usage("stat", "missing operand"));
    }
    let mut output = Vec::new();
    for operand in operands {
        let path = context.resolve_entry(&command.cwd, operand)?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| AppletError::io(operand, error))?;
        let kind = file_kind(&metadata);
        match format {
            Some(value) => render_stat_format(
                &mut output,
                context.limits().max_output_bytes,
                value,
                operand,
                &metadata,
                kind,
            )?,
            None => push_bounded(
                &mut output,
                format!(
                    "  File: {operand}\n  Size: {}\tType: {kind}\n  Mode: {:04o}\n",
                    metadata.len(),
                    unix_mode(&metadata) & 0o7777
                )
                .as_bytes(),
                context.limits().max_output_bytes,
            )?,
        }
    }
    Ok(AppletOutput::success(output))
}

fn render_stat_format(
    output: &mut Vec<u8>,
    output_limit: usize,
    format_value: &str,
    name: &str,
    metadata: &fs::Metadata,
    kind: &str,
) -> Result<()> {
    let mut chars = format_value.chars();
    while let Some(value) = chars.next() {
        if value != '%' {
            let mut encoded = [0; 4];
            push_bounded(
                output,
                value.encode_utf8(&mut encoded).as_bytes(),
                output_limit,
            )?;
            continue;
        }
        match chars.next() {
            Some('%') => push_bounded(output, b"%", output_limit)?,
            Some('n') => push_bounded(output, name.as_bytes(), output_limit)?,
            Some('s') => {
                push_bounded(output, metadata.len().to_string().as_bytes(), output_limit)?;
            }
            Some('F') => push_bounded(output, kind.as_bytes(), output_limit)?,
            Some('a') => {
                push_bounded(
                    output,
                    format!("{:o}", unix_mode(metadata) & 0o7777).as_bytes(),
                    output_limit,
                )?;
            }
            Some(other) => {
                return Err(AppletError::usage(
                    "stat",
                    format!("unsupported format conversion: %{other}"),
                ));
            }
            None => return Err(AppletError::usage("stat", "trailing percent sign")),
        }
    }
    push_bounded(output, b"\n", output_limit)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn realpath_never_reports_a_host_path() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("dir")).unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
        let command = GuestCommand::new("realpath", ["dir".to_owned()]);
        assert_eq!(execute(&context, &command).unwrap().stdout, b"/dir\n");
    }

    #[test]
    fn stat_format_expansion_is_bounded_while_rendering() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("alpha"), b"one").unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap())
            .unwrap()
            .with_limits(crate::AppletLimits {
                max_output_bytes: 8,
                ..crate::AppletLimits::default()
            })
            .unwrap();
        let command = GuestCommand::new(
            "stat",
            ["-c".to_owned(), "%n".repeat(1024), "alpha".to_owned()],
        );

        assert!(matches!(
            execute(&context, &command),
            Err(AppletError::OutputLimit { limit: 8 })
        ));
    }
}
