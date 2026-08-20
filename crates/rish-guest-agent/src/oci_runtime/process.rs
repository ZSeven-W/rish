use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rish_guest_protocol::{ErrorCode, RemoteError};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(5);
const SIGKILL_NUMBER: i32 = 9;
static NEXT_OUTPUT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(super) struct RuntimeOutput {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RuntimeOutput {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

pub(super) trait RuntimeCommandRunner: Send {
    fn run(&mut self, args: &[OsString], timeout: Duration) -> Result<RuntimeOutput, RemoteError>;
}

#[derive(Debug)]
pub(super) struct ProcessRuntimeRunner {
    runtime_path: PathBuf,
    runtime_name: String,
    output_directory: PathBuf,
    max_output_bytes: u64,
}

impl ProcessRuntimeRunner {
    pub fn new(runtime_path: PathBuf, output_directory: PathBuf, max_output_bytes: u64) -> Self {
        let runtime_name = runtime_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("OCI runtime")
            .to_owned();
        Self {
            runtime_path,
            runtime_name,
            output_directory,
            max_output_bytes,
        }
    }

    fn run_inner(
        &self,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<RuntimeOutput, RemoteError> {
        let mut stdout = TemporaryOutput::create(&self.output_directory, "stdout")?;
        let mut stderr = TemporaryOutput::create(&self.output_directory, "stderr")?;

        let mut command = Command::new(&self.runtime_path);
        command
            .args(args)
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout.clone_file()?))
            .stderr(Stdio::from(stderr.clone_file()?));
        configure_process_group(&mut command);

        let mut child = command.spawn().map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to start {}: {error}", self.runtime_name),
            )
        })?;
        let started_at = Instant::now();
        let status = loop {
            if stdout.length()? > self.max_output_bytes || stderr.length()? > self.max_output_bytes
            {
                terminate_and_reap(&mut child);
                return Err(RemoteError::new(
                    ErrorCode::ResourceExhausted,
                    format!(
                        "{} output exceeded the configured {} byte limit",
                        self.runtime_name, self.max_output_bytes
                    ),
                ));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started_at.elapsed() >= timeout => {
                    terminate_and_reap(&mut child);
                    return Err(RemoteError::new(
                        ErrorCode::DeadlineExceeded,
                        format!(
                            "{} command exceeded its {} ms deadline",
                            self.runtime_name,
                            timeout.as_millis()
                        ),
                    ));
                }
                Ok(None) => thread::sleep(PROCESS_POLL_INTERVAL),
                Err(error) => {
                    terminate_and_reap(&mut child);
                    return Err(RemoteError::new(
                        ErrorCode::Io,
                        format!("failed to wait for {}: {error}", self.runtime_name),
                    ));
                }
            }
        };

        let stdout = stdout.read_bounded(self.max_output_bytes)?;
        let stderr = stderr.read_bounded(self.max_output_bytes)?;
        Ok(RuntimeOutput {
            exit_code: status.code(),
            stdout,
            stderr,
        })
    }
}

impl RuntimeCommandRunner for ProcessRuntimeRunner {
    fn run(&mut self, args: &[OsString], timeout: Duration) -> Result<RuntimeOutput, RemoteError> {
        self.run_inner(args, timeout)
    }
}

#[derive(Debug)]
struct TemporaryOutput {
    path: PathBuf,
    file: File,
}

impl TemporaryOutput {
    fn create(directory: &Path, channel: &str) -> Result<Self, RemoteError> {
        for _ in 0..32 {
            let id = NEXT_OUTPUT_ID.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(
                ".rish-oci-{}-{id}-{channel}.tmp",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            configure_private_file(&mut options);
            match options.open(&path) {
                Ok(file) => return Ok(Self { path, file }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(RemoteError::new(
                        ErrorCode::Io,
                        format!("failed to create bounded OCI runtime output: {error}"),
                    ));
                }
            }
        }
        Err(RemoteError::new(
            ErrorCode::ResourceExhausted,
            "could not allocate a unique OCI runtime output file",
        ))
    }

    fn clone_file(&self) -> Result<File, RemoteError> {
        self.file.try_clone().map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to clone OCI runtime output descriptor: {error}"),
            )
        })
    }

    fn length(&self) -> Result<u64, RemoteError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| {
                RemoteError::new(
                    ErrorCode::Io,
                    format!("failed to inspect OCI runtime output: {error}"),
                )
            })
    }

    fn read_bounded(&mut self, limit: u64) -> Result<Vec<u8>, RemoteError> {
        let length = self.length()?;
        if length > limit {
            return Err(RemoteError::new(
                ErrorCode::ResourceExhausted,
                format!("OCI runtime output exceeded the configured {limit} byte limit"),
            ));
        }
        self.file.seek(SeekFrom::Start(0)).map_err(output_error)?;
        let capacity = usize::try_from(length).map_err(|_| {
            RemoteError::new(
                ErrorCode::ResourceExhausted,
                "OCI runtime output does not fit in memory",
            )
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        self.file
            .by_ref()
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(output_error)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
            return Err(RemoteError::new(
                ErrorCode::ResourceExhausted,
                format!("OCI runtime output exceeded the configured {limit} byte limit"),
            ));
        }
        Ok(bytes)
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn output_error(error: io::Error) -> RemoteError {
    RemoteError::new(
        ErrorCode::Io,
        format!("failed to read OCI runtime output: {error}"),
    )
}

#[cfg(unix)]
fn configure_private_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_private_file(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn terminate_and_reap(child: &mut Child) {
    let _ = signal_process_group(child.id(), SIGKILL_NUMBER);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: i32) -> io::Result<()> {
    use std::num::NonZeroI32;

    use rustix::process::{Pid, Signal, kill_process_group};

    let pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid child pid"))?;
    let signal = NonZeroI32::new(signal)
        .and_then(Signal::from_named_raw_nonzero)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid signal"))?;
    kill_process_group(pid, signal).map_err(io::Error::from)
}

#[cfg(not(unix))]
fn signal_process_group(_pid: u32, _signal: i32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process groups are unavailable",
    ))
}
