use rish_core::GuestCommand;
use tempfile::TempDir;

use super::*;

fn executor() -> (TempDir, AppletExecutor) {
    let root = TempDir::new().unwrap();
    let context = AppletContext::new(root.path().canonicalize().unwrap()).unwrap();
    (root, AppletExecutor::new(context))
}

#[test]
fn unknown_commands_fail_closed() {
    let (_root, executor) = executor();
    let error = executor
        .execute(&GuestCommand::new("python3", Vec::<String>::new()))
        .unwrap_err();
    assert!(matches!(error, AppletError::UnknownApplet(_)));
}

#[test]
fn command_envelope_limits_fail_before_dispatch() {
    let (_root, executor) = executor();
    let command = GuestCommand::new("echo", (0..257).map(|_| "x".to_owned()));
    assert!(matches!(
        executor.execute(&command),
        Err(AppletError::InvalidArguments { .. })
    ));
}

#[test]
fn read_only_context_rejects_mutating_applets() {
    let root = TempDir::new().unwrap();
    let context = AppletContext::new(root.path().canonicalize().unwrap())
        .unwrap()
        .read_only(true);
    let executor = AppletExecutor::new(context);
    let command = GuestCommand::new("touch", ["file".to_owned()]);

    assert!(matches!(
        executor.execute(&command),
        Err(AppletError::ReadOnlyFilesystem)
    ));
}

#[test]
fn cwd_cannot_escape_the_guest_root() {
    let (_root, executor) = executor();
    let mut command = GuestCommand::new("pwd", Vec::<String>::new());
    command.cwd = "/../../outside".to_owned();

    assert!(matches!(
        executor.execute(&command),
        Err(AppletError::UnsafePath(_))
    ));
}

#[test]
fn pwd_requires_an_existing_directory() {
    let (_root, executor) = executor();
    let mut command = GuestCommand::new("pwd", Vec::<String>::new());
    command.cwd = "/missing".to_owned();

    assert!(matches!(
        executor.execute(&command),
        Err(AppletError::Io { .. })
    ));
}

#[test]
fn checksum_file_reads_obey_the_cumulative_input_limit() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("large"), b"12345").unwrap();
    let limits = AppletLimits {
        max_input_bytes: 4,
        ..AppletLimits::default()
    };
    let context = AppletContext::new(root.path().canonicalize().unwrap())
        .unwrap()
        .with_limits(limits)
        .unwrap();
    let command = GuestCommand::new("sha256sum", ["large".to_owned()]);

    assert!(matches!(
        AppletExecutor::new(context).execute(&command),
        Err(AppletError::InputLimit { limit: 4 })
    ));
}

#[test]
fn read_only_virtual_files_are_not_reported_writable() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("file"), b"x").unwrap();
    let context = AppletContext::new(root.path().canonicalize().unwrap())
        .unwrap()
        .read_only(true);
    let command = GuestCommand::new("test", ["-w".to_owned(), "file".to_owned()]);

    let outcome = AppletExecutor::new(context).execute(&command).unwrap();
    assert_eq!(outcome.exit_code, 1);
}
