use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use rish_core::GuestCommand;

use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(super) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "[" | "test" => test(context, command),
        "date" => date(command),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}

fn test(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    let mut arguments = command.args.as_slice();
    if command.basename() == "[" {
        if arguments.last().map(String::as_str) != Some("]") {
            return Err(AppletError::usage("[", "missing closing ]"));
        }
        arguments = &arguments[..arguments.len() - 1];
    }
    let value = evaluate_test(context, &command.cwd, arguments)?;
    Ok(AppletOutput::status(i32::from(!value)))
}

fn evaluate_test(context: &AppletContext, cwd: &str, arguments: &[String]) -> Result<bool> {
    match arguments {
        [] => Ok(false),
        [value] => Ok(!value.is_empty()),
        [operator, value] if operator == "!" => Ok(value.is_empty()),
        [operator, value] => unary_test(context, cwd, operator, value),
        [left, operator, right] => binary_test(left, operator, right),
        [not, rest @ ..] if not == "!" => Ok(!evaluate_test(context, cwd, rest)?),
        _ => Err(AppletError::usage(
            "test",
            "unsupported or ambiguous expression",
        )),
    }
}

fn unary_test(context: &AppletContext, cwd: &str, operator: &str, value: &str) -> Result<bool> {
    match operator {
        "-n" => Ok(!value.is_empty()),
        "-z" => Ok(value.is_empty()),
        "-e" | "-f" | "-d" | "-L" | "-h" | "-s" | "-r" | "-w" | "-x" => {
            let metadata = match context.resolve_entry(cwd, value) {
                Ok(path) => fs::symlink_metadata(path),
                Err(AppletError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(error),
            };
            let metadata = match metadata {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(AppletError::io(value, error)),
            };
            Ok(match operator {
                "-e" => true,
                "-f" => metadata.is_file(),
                "-d" => metadata.is_dir(),
                "-L" | "-h" => metadata.file_type().is_symlink(),
                "-s" => metadata.len() > 0,
                "-r" => mode(&metadata) & 0o444 != 0,
                "-w" => !context.is_read_only() && mode(&metadata) & 0o222 != 0,
                "-x" => mode(&metadata) & 0o111 != 0,
                _ => false,
            })
        }
        _ => Err(AppletError::usage(
            "test",
            format!("unsupported unary operator: {operator}"),
        )),
    }
}

fn binary_test(left: &str, operator: &str, right: &str) -> Result<bool> {
    match operator {
        "=" | "==" => Ok(left == right),
        "!=" => Ok(left != right),
        "<" => Ok(left < right),
        ">" => Ok(left > right),
        "-eq" => Ok(integer(left)? == integer(right)?),
        "-ne" => Ok(integer(left)? != integer(right)?),
        "-lt" => Ok(integer(left)? < integer(right)?),
        "-le" => Ok(integer(left)? <= integer(right)?),
        "-gt" => Ok(integer(left)? > integer(right)?),
        "-ge" => Ok(integer(left)? >= integer(right)?),
        "-a" => Ok(!left.is_empty() && !right.is_empty()),
        "-o" => Ok(!left.is_empty() || !right.is_empty()),
        _ => Err(AppletError::usage(
            "test",
            format!("unsupported binary operator: {operator}"),
        )),
    }
}

fn integer(value: &str) -> Result<i128> {
    value
        .parse()
        .map_err(|_| AppletError::usage("test", format!("not an integer: {value}")))
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o666
    }
}

fn date(command: &GuestCommand) -> Result<AppletOutput> {
    let mut format = None;
    for argument in &command.args {
        match argument.as_str() {
            "-u" | "--utc" | "--universal" => {}
            "-I" | "--iso-8601" => format = Some("%Y-%m-%d"),
            value if value.starts_with('+') && format.is_none() => {
                format = Some(&value[1..]);
            }
            value if value == "-s" || value.starts_with("--set") => {
                return Err(AppletError::usage(
                    "date",
                    "setting a clock requires a privileged Linux backend",
                ));
            }
            value => {
                return Err(AppletError::usage(
                    "date",
                    format!("unsupported operand: {value}"),
                ));
            }
        }
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppletError::usage("date", "host clock predates the Unix epoch"))?
        .as_secs() as i64;
    let fields = UtcFields::from_epoch(seconds);
    let rendered = match format {
        Some(value) => format_date(value, seconds, fields)?,
        None => {
            const WEEKDAYS: &[&str] = &["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
            const MONTHS: &[&str] = &[
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let weekday = WEEKDAYS[(seconds.div_euclid(86_400).rem_euclid(7)) as usize];
            format!(
                "{weekday} {} {:2} {:02}:{:02}:{:02} UTC {:04}",
                MONTHS[(fields.month - 1) as usize],
                fields.day,
                fields.hour,
                fields.minute,
                fields.second,
                fields.year
            )
        }
    };
    Ok(AppletOutput::success(format!("{rendered}\n")))
}

#[derive(Clone, Copy)]
struct UtcFields {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

impl UtcFields {
    fn from_epoch(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let day_seconds = seconds.rem_euclid(86_400);
        let shifted = days + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prime = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
        let month = month_prime + if month_prime < 10 { 3 } else { -9 };
        year += i64::from(month <= 2);
        Self {
            year,
            month,
            day,
            hour: day_seconds / 3_600,
            minute: day_seconds % 3_600 / 60,
            second: day_seconds % 60,
        }
    }
}

fn format_date(format: &str, epoch: i64, fields: UtcFields) -> Result<String> {
    let mut output = String::new();
    let mut chars = format.chars();
    while let Some(value) = chars.next() {
        if value != '%' {
            output.push(value);
            continue;
        }
        match chars.next() {
            Some('%') => output.push('%'),
            Some('Y') => output.push_str(&format!("{:04}", fields.year)),
            Some('m') => output.push_str(&format!("{:02}", fields.month)),
            Some('d') => output.push_str(&format!("{:02}", fields.day)),
            Some('H') => output.push_str(&format!("{:02}", fields.hour)),
            Some('M') => output.push_str(&format!("{:02}", fields.minute)),
            Some('S') => output.push_str(&format!("{:02}", fields.second)),
            Some('s') => output.push_str(&epoch.to_string()),
            Some('F') => output.push_str(&format!(
                "{:04}-{:02}-{:02}",
                fields.year, fields.month, fields.day
            )),
            Some('T') => output.push_str(&format!(
                "{:02}:{:02}:{:02}",
                fields.hour, fields.minute, fields.second
            )),
            Some('z') => output.push_str("+0000"),
            Some(other) => {
                return Err(AppletError::usage(
                    "date",
                    format!("unsupported format conversion: %{other}"),
                ));
            }
            None => return Err(AppletError::usage("date", "trailing percent sign")),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_supports_strings_integers_and_scoped_files() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("file"), b"x").unwrap();
        let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
        for arguments in [
            vec!["x".to_owned(), "=".to_owned(), "x".to_owned()],
            vec!["2".to_owned(), "-gt".to_owned(), "1".to_owned()],
            vec!["-f".to_owned(), "/file".to_owned()],
        ] {
            let command = GuestCommand::new("test", arguments);
            assert_eq!(execute(&context, &command).unwrap().exit_code, 0);
        }
    }

    #[test]
    fn epoch_calendar_conversion_is_stable() {
        let fields = UtcFields::from_epoch(0);
        assert_eq!(
            format_date("%F %T %z", 0, fields).unwrap(),
            "1970-01-01 00:00:00 +0000"
        );
    }
}
