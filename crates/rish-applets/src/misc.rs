use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rish_core::GuestCommand;
use sha2::{Sha256, Sha512};

use crate::output::push_bounded;
use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(crate) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "[" | "date" | "test" => extra::execute(context, command),
        "base64" => base64_command(context, command),
        "basename" => basename(command),
        "cksum" => cksum(context, command),
        "dirname" => dirname(command),
        "echo" => echo(command),
        "env" => env(command),
        "false" => Ok(AppletOutput::status(1)),
        "groups" => Ok(AppletOutput::success(format!("{}\n", context.user()))),
        "hostname" => hostname(context, command),
        "id" => id(context, command),
        "printenv" => printenv(command),
        "printf" => printf(command),
        "pwd" => pwd(context, command),
        "seq" => seq(context, command),
        "sha256sum" => checksum::<Sha256>(context, command),
        "sha512sum" => checksum::<Sha512>(context, command),
        "true" => Ok(AppletOutput::success(Vec::new())),
        "uname" => uname(context, command),
        "whoami" => no_arguments(
            command,
            AppletOutput::success(format!("{}\n", context.user())),
        ),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}

fn no_arguments(command: &GuestCommand, output: AppletOutput) -> Result<AppletOutput> {
    if command.args.is_empty() {
        Ok(output)
    } else {
        Err(AppletError::usage(
            command.basename(),
            "this applet accepts no operands",
        ))
    }
}

fn echo(command: &GuestCommand) -> Result<AppletOutput> {
    let mut newline = true;
    let mut escapes = false;
    let mut index = 0;
    while let Some(argument) = command.args.get(index) {
        match argument.as_str() {
            "-n" => newline = false,
            "-e" => escapes = true,
            "-E" => escapes = false,
            _ => break,
        }
        index += 1;
    }
    let joined = command.args[index..].join(" ");
    let mut output = if escapes {
        expand_escapes(&joined, false)?
    } else {
        joined.into_bytes()
    };
    if newline {
        output.push(b'\n');
    }
    Ok(AppletOutput::success(output))
}

fn printf(command: &GuestCommand) -> Result<AppletOutput> {
    let Some(format) = command.args.first() else {
        return Err(AppletError::usage("printf", "missing format operand"));
    };
    let mut output = Vec::new();
    let mut arguments = command.args[1..].iter();
    let mut bytes = format.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\\' => {
                let escape = bytes
                    .next()
                    .ok_or_else(|| AppletError::usage("printf", "trailing backslash"))?;
                output.extend_from_slice(&decode_escape(escape)?);
            }
            b'%' => match bytes.next() {
                Some(b'%') => output.push(b'%'),
                Some(b's') => {
                    output.extend_from_slice(arguments.next().map_or("", String::as_str).as_bytes())
                }
                Some(b'b') => output.extend_from_slice(&expand_escapes(
                    arguments.next().map_or("", String::as_str),
                    true,
                )?),
                Some(other) => {
                    return Err(AppletError::usage(
                        "printf",
                        format!("unsupported conversion: %{}", char::from(other)),
                    ));
                }
                None => return Err(AppletError::usage("printf", "trailing percent sign")),
            },
            other => output.push(other),
        }
    }
    Ok(AppletOutput::success(output))
}

fn expand_escapes(value: &str, stop_at_c: bool) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            output.push(byte);
            continue;
        }
        let Some(escape) = bytes.next() else {
            return Err(AppletError::usage("escape", "trailing backslash"));
        };
        if stop_at_c && escape == b'c' {
            break;
        }
        output.extend_from_slice(&decode_escape(escape)?);
    }
    Ok(output)
}

fn decode_escape(value: u8) -> Result<Vec<u8>> {
    let decoded = match value {
        b'\\' => b'\\',
        b'a' => 0x07,
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'v' => 0x0b,
        other => {
            return Err(AppletError::usage(
                "escape",
                format!("unsupported escape: \\{}", char::from(other)),
            ));
        }
    };
    Ok(vec![decoded])
}

fn basename(command: &GuestCommand) -> Result<AppletOutput> {
    if command.args.is_empty() || command.args.len() > 2 {
        return Err(AppletError::usage(
            "basename",
            "usage: basename PATH [SUFFIX]",
        ));
    }
    let value = command.args[0].trim_end_matches('/');
    let mut name = Path::new(if value.is_empty() { "/" } else { value })
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("/")
        .to_owned();
    if let Some(suffix) = command.args.get(1) {
        if suffix != &name && name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
        }
    }
    Ok(AppletOutput::success(format!("{name}\n")))
}

fn dirname(command: &GuestCommand) -> Result<AppletOutput> {
    if command.args.len() != 1 {
        return Err(AppletError::usage("dirname", "usage: dirname PATH"));
    }
    let value = command.args[0].trim_end_matches('/');
    let parent = Path::new(if value.is_empty() { "/" } else { value })
        .parent()
        .and_then(|parent| parent.to_str())
        .filter(|parent| !parent.is_empty())
        .unwrap_or(".");
    Ok(AppletOutput::success(format!("{parent}\n")))
}

fn pwd(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    if command.args.iter().any(|argument| argument != "-L") {
        return Err(AppletError::usage("pwd", "only -L is supported"));
    }
    let path = context.resolve_existing("/", &command.cwd)?;
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| AppletError::io("pwd", error))?;
    if !metadata.is_dir() {
        return Err(AppletError::UnsafePath(
            "working directory is not a directory".to_owned(),
        ));
    }
    Ok(AppletOutput::success(format!("{}\n", command.cwd)))
}

fn hostname(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    if command.args.is_empty() {
        Ok(AppletOutput::success(format!("{}\n", context.hostname())))
    } else {
        Err(AppletError::usage(
            "hostname",
            "changing the portable hostname is not supported",
        ))
    }
}

fn id(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let user = context.user();
    match command.args.as_slice() {
        [] => Ok(AppletOutput::success(format!(
            "uid=1000({user}) gid=1000({user}) groups=1000({user})\n"
        ))),
        [flag] if flag == "-u" => Ok(AppletOutput::success(b"1000\n".to_vec())),
        [flag] if flag == "-g" => Ok(AppletOutput::success(b"1000\n".to_vec())),
        [flag] if flag == "-G" => Ok(AppletOutput::success(b"1000\n".to_vec())),
        [flag] if flag == "-un" || flag == "-gn" => Ok(AppletOutput::success(format!("{user}\n"))),
        _ => Err(AppletError::usage(
            "id",
            "supported forms: id, id -u, id -g, id -G, id -un, id -gn",
        )),
    }
}

fn env(command: &GuestCommand) -> Result<AppletOutput> {
    let mut values = command.env.clone();
    let mut index = 0;
    if command.args.first().is_some_and(|value| value == "-i") {
        values.clear();
        index += 1;
    }
    while command.args.get(index).is_some_and(|value| value == "-u") {
        let name = command
            .args
            .get(index + 1)
            .ok_or_else(|| AppletError::usage("env", "-u requires a name"))?;
        values.remove(name);
        index += 2;
    }
    while let Some(assignment) = command.args.get(index) {
        let Some((name, value)) = assignment.split_once('=') else {
            return Err(AppletError::usage(
                "env",
                "executing another command requires a shell or guest backend",
            ));
        };
        if name.is_empty() {
            return Err(AppletError::usage("env", "empty variable name"));
        }
        values.insert(name.to_owned(), value.to_owned());
        index += 1;
    }
    Ok(AppletOutput::success(render_environment(&values)))
}

fn printenv(command: &GuestCommand) -> Result<AppletOutput> {
    if command.args.is_empty() {
        return Ok(AppletOutput::success(render_environment(&command.env)));
    }
    let mut output = Vec::new();
    let mut missing = false;
    for name in &command.args {
        match command.env.get(name) {
            Some(value) => {
                output.extend_from_slice(value.as_bytes());
                output.push(b'\n');
            }
            None => missing = true,
        }
    }
    Ok(AppletOutput {
        exit_code: i32::from(missing),
        stdout: output,
        stderr: Vec::new(),
    })
}

fn render_environment(values: &BTreeMap<String, String>) -> Vec<u8> {
    let mut output = Vec::new();
    for (name, value) in values {
        output.extend_from_slice(name.as_bytes());
        output.push(b'=');
        output.extend_from_slice(value.as_bytes());
        output.push(b'\n');
    }
    output
}

fn uname(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut fields = Vec::new();
    let flags = if command.args.is_empty() {
        vec!["-s".to_owned()]
    } else {
        command.args.clone()
    };
    let all = flags.iter().any(|flag| flag == "-a");
    for (flag, value) in [
        ("-s", "Rish".to_owned()),
        ("-n", context.hostname().to_owned()),
        ("-r", "rish-portable-0.1".to_owned()),
        ("-v", "#1 portable-applet".to_owned()),
        ("-m", "aarch64".to_owned()),
        ("-o", "rish".to_owned()),
    ] {
        if all || flags.iter().any(|argument| argument == flag) {
            fields.push(value);
        }
    }
    if fields.is_empty()
        || flags
            .iter()
            .any(|flag| !["-a", "-s", "-n", "-r", "-v", "-m", "-o"].contains(&flag.as_str()))
    {
        return Err(AppletError::usage(
            "uname",
            "supported flags: -a -s -n -r -v -m -o",
        ));
    }
    Ok(AppletOutput::success(format!("{}\n", fields.join(" "))))
}

fn seq(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut separator = "\n".to_owned();
    let mut values = Vec::new();
    let mut index = 0;
    while let Some(argument) = command.args.get(index) {
        if argument == "-s" {
            separator = command
                .args
                .get(index + 1)
                .ok_or_else(|| AppletError::usage("seq", "-s requires a separator"))?
                .clone();
            index += 2;
        } else if argument.starts_with('-') && argument.parse::<i64>().is_err() {
            return Err(AppletError::usage("seq", "supported option: -s SEP"));
        } else {
            values.push(
                argument
                    .parse::<i64>()
                    .map_err(|_| AppletError::usage("seq", "operands must be integers"))?,
            );
            index += 1;
        }
    }
    let (first, step, last) = match values.as_slice() {
        [last] => (1, 1, *last),
        [first, last] => (*first, 1, *last),
        [first, step, last] if *step != 0 => (*first, *step, *last),
        _ => return Err(AppletError::usage("seq", "usage: seq [FIRST [STEP]] LAST")),
    };
    let mut output = Vec::new();
    let mut value = first;
    let forward = step > 0;
    let mut count = 0usize;
    while (forward && value <= last) || (!forward && value >= last) {
        if count > 0 {
            push_bounded(
                &mut output,
                separator.as_bytes(),
                context.limits().max_output_bytes,
            )?;
        }
        push_bounded(
            &mut output,
            value.to_string().as_bytes(),
            context.limits().max_output_bytes,
        )?;
        value = value
            .checked_add(step)
            .ok_or_else(|| AppletError::usage("seq", "integer overflow"))?;
        count += 1;
    }
    if count > 0 {
        push_bounded(&mut output, b"\n", context.limits().max_output_bytes)?;
    }
    Ok(AppletOutput::success(output))
}

fn base64_command(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut decode = false;
    let mut operand = None;
    for argument in &command.args {
        match argument.as_str() {
            "-d" | "--decode" => decode = true,
            "-w" | "--wrap" => {
                return Err(AppletError::usage(
                    "base64",
                    "line wrapping is not supported",
                ));
            }
            value if operand.is_none() => operand = Some(value),
            _ => return Err(AppletError::usage("base64", "too many operands")),
        }
    }
    let input = read_small_input(context, command, operand)?;
    let output = if decode {
        STANDARD
            .decode(
                input
                    .iter()
                    .copied()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| AppletError::usage("base64", error.to_string()))?
    } else {
        let mut encoded = STANDARD.encode(input).into_bytes();
        encoded.push(b'\n');
        encoded
    };
    Ok(AppletOutput::success(output))
}

fn checksum<D>(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput>
where
    D: sha2::Digest + Default,
{
    if command
        .args
        .iter()
        .any(|argument| argument.starts_with('-') && argument != "-")
    {
        return Err(AppletError::usage(
            command.basename(),
            "verification and binary-mode flags are not yet supported",
        ));
    }
    let operands = if command.args.is_empty() {
        vec!["-"]
    } else {
        command.args.iter().map(String::as_str).collect()
    };
    let mut output = Vec::new();
    let mut input_bytes = 0usize;
    for operand in operands {
        let mut digest = D::default();
        if operand == "-" {
            account_input(context, &mut input_bytes, command.stdin.len())?;
            digest.update(&command.stdin);
        } else {
            let path = context.resolve_existing(&command.cwd, operand)?;
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| AppletError::io(operand, error))?;
            if !metadata.is_file() {
                return Err(AppletError::UnsafePath(format!(
                    "checksum input must be a regular file: {operand}"
                )));
            }
            let mut reader =
                BufReader::new(File::open(&path).map_err(|error| AppletError::io(operand, error))?);
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = reader
                    .read(&mut buffer)
                    .map_err(|error| AppletError::io(operand, error))?;
                if read == 0 {
                    break;
                }
                account_input(context, &mut input_bytes, read)?;
                digest.update(&buffer[..read]);
            }
        }
        let line = format!("{}  {operand}\n", hex_lower(&digest.finalize()));
        push_bounded(
            &mut output,
            line.as_bytes(),
            context.limits().max_output_bytes,
        )?;
    }
    Ok(AppletOutput::success(output))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn cksum(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    if command
        .args
        .iter()
        .any(|argument| argument.starts_with('-') && argument != "-")
    {
        return Err(AppletError::usage("cksum", "options are not supported"));
    }
    let operands = if command.args.is_empty() {
        vec!["-"]
    } else {
        command.args.iter().map(String::as_str).collect()
    };
    let mut output = Vec::new();
    let mut input_bytes = 0usize;
    for operand in operands {
        let input = read_small_input(context, command, Some(operand))?;
        account_input(context, &mut input_bytes, input.len())?;
        let crc = posix_crc32(&input);
        let suffix = if operand == "-" {
            String::new()
        } else {
            format!(" {operand}")
        };
        push_bounded(
            &mut output,
            format!("{crc} {}{suffix}\n", input.len()).as_bytes(),
            context.limits().max_output_bytes,
        )?;
    }
    Ok(AppletOutput::success(output))
}

fn read_small_input(
    context: &AppletContext,
    command: &GuestCommand,
    operand: Option<&str>,
) -> Result<Vec<u8>> {
    let Some(operand) = operand else {
        return Ok(command.stdin.clone());
    };
    if operand == "-" {
        return Ok(command.stdin.clone());
    }
    let path = context.resolve_existing(&command.cwd, operand)?;
    let metadata = std::fs::metadata(&path).map_err(|error| AppletError::io(operand, error))?;
    if metadata.len() > context.limits().max_input_bytes as u64 {
        return Err(AppletError::InputLimit {
            limit: context.limits().max_input_bytes,
        });
    }
    std::fs::read(&path).map_err(|error| AppletError::io(operand, error))
}

fn account_input(context: &AppletContext, used: &mut usize, amount: usize) -> Result<()> {
    *used = used.checked_add(amount).ok_or(AppletError::InputLimit {
        limit: context.limits().max_input_bytes,
    })?;
    if *used > context.limits().max_input_bytes {
        return Err(AppletError::InputLimit {
            limit: context.limits().max_input_bytes,
        });
    }
    Ok(())
}

fn posix_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    let mut length = bytes.len();
    while length != 0 {
        crc ^= (length as u32 & 0xff) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
        length >>= 8;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn run(program: &str, args: &[&str], stdin: &[u8]) -> AppletOutput {
        let root = TempDir::new().unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
        let mut command = GuestCommand::new(program, args.iter().map(|value| (*value).to_owned()));
        command.stdin = stdin.to_vec();
        execute(&context, &command).unwrap()
    }

    #[test]
    fn printf_and_echo_are_binary_safe() {
        assert_eq!(run("printf", &["%s\\n", "hi"], b"").stdout, b"hi\n");
        assert_eq!(run("echo", &["-n", "hello"], b"").stdout, b"hello");
    }

    #[test]
    fn hashes_and_base64_match_known_values() {
        assert_eq!(
            run("sha256sum", &[], b"abc").stdout,
            b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  -\n"
        );
        assert_eq!(run("base64", &[], b"abc").stdout, b"YWJj\n");
        assert_eq!(run("base64", &["-d"], b"YWJj\n").stdout, b"abc");
    }

    #[test]
    fn seq_is_bounded_and_supports_descending_ranges() {
        assert_eq!(run("seq", &["3", "-1", "1"], b"").stdout, b"3\n2\n1\n");
    }

    #[test]
    fn uname_reports_the_virtual_rish_personality() {
        assert_eq!(run("uname", &[], b"").stdout, b"Rish\n");
        assert_eq!(run("uname", &["-v"], b"").stdout, b"#1 portable-applet\n");
    }
}
mod extra;
