//! Blocking OCI lifecycle primitives for use inside the Linux guest.
//!
//! This module invokes an installed OCI runtime. It does not emulate
//! namespaces, cgroups, mounts, or devices in the mobile host process.
//! Calls are bounded but blocking, so a control-plane integration must execute
//! them on a worker and keep the guest protocol polling thread responsive.

mod process;
mod validation;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use process::{ProcessRuntimeRunner, RuntimeCommandRunner, RuntimeOutput};
use rish_guest_protocol::{
    ContainerState, ErrorCode, EventKind, OciDeleteRequest, OciPrepareRequest, OciRunRequest,
    OciStopRequest, Operation, RemoteError, ResponsePayload,
};
use serde::Deserialize;
use validation::{
    is_clean_absolute_path, normalize_signal, prepare_directory, validate_container_id,
    validate_duration, validate_image_identity, validate_rootfs_record, validate_runtime_path,
    validate_spec, write_spec_atomically,
};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_FORCE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_STOP_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_FORCE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RUNTIME_OUTPUT_BYTES: u64 = 256 * 1024;
const ABSOLUTE_MAX_RUNTIME_OUTPUT_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_SPEC_BYTES: usize = 2 * 1024 * 1024;
const ABSOLUTE_MAX_SPEC_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTAINER_ID_BYTES: usize = 128;
const MAX_IMAGE_REFERENCE_BYTES: usize = 1024;
const STATE_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OciRuntimeConfig {
    runtime_path: PathBuf,
    bundle_root: PathBuf,
    runtime_root: Option<PathBuf>,
    command_timeout: Duration,
    stop_timeout: Duration,
    force_cleanup_timeout: Duration,
    max_runtime_output_bytes: u64,
    max_spec_bytes: usize,
}

impl OciRuntimeConfig {
    #[must_use]
    pub fn new(runtime_path: impl Into<PathBuf>, bundle_root: impl Into<PathBuf>) -> Self {
        Self {
            runtime_path: runtime_path.into(),
            bundle_root: bundle_root.into(),
            runtime_root: None,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            stop_timeout: DEFAULT_STOP_TIMEOUT,
            force_cleanup_timeout: DEFAULT_FORCE_CLEANUP_TIMEOUT,
            max_runtime_output_bytes: DEFAULT_MAX_RUNTIME_OUTPUT_BYTES,
            max_spec_bytes: DEFAULT_MAX_SPEC_BYTES,
        }
    }

    #[must_use]
    pub fn with_runtime_root(mut self, runtime_root: impl Into<PathBuf>) -> Self {
        self.runtime_root = Some(runtime_root.into());
        self
    }

    #[must_use]
    pub fn with_timeouts(
        mut self,
        command_timeout: Duration,
        stop_timeout: Duration,
        force_cleanup_timeout: Duration,
    ) -> Self {
        self.command_timeout = command_timeout;
        self.stop_timeout = stop_timeout;
        self.force_cleanup_timeout = force_cleanup_timeout;
        self
    }

    #[must_use]
    pub fn with_limits(mut self, max_runtime_output_bytes: u64, max_spec_bytes: usize) -> Self {
        self.max_runtime_output_bytes = max_runtime_output_bytes;
        self.max_spec_bytes = max_spec_bytes;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OciLifecycleReply {
    pub response: ResponsePayload,
    pub events: Vec<EventKind>,
}

pub struct OciRuntimeBackend {
    config: ValidatedConfig,
    runner: Box<dyn RuntimeCommandRunner>,
}

impl std::fmt::Debug for OciRuntimeBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OciRuntimeBackend")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OciRuntimeBackend {
    pub fn new(config: OciRuntimeConfig) -> Result<Self, RemoteError> {
        let config = ValidatedConfig::new(config)?;
        let runner = ProcessRuntimeRunner::new(
            config.runtime_path.clone(),
            config.output_directory.clone(),
            config.max_runtime_output_bytes,
        );
        Ok(Self {
            config,
            runner: Box::new(runner),
        })
    }

    /// Dispatches one OCI lifecycle operation.
    ///
    /// The caller must run this blocking method outside the guest protocol
    /// polling thread. Interactive attach is rejected until a console/OCI-exec
    /// stream is connected to the protocol.
    pub fn dispatch(&mut self, operation: &Operation) -> Result<OciLifecycleReply, RemoteError> {
        match operation {
            Operation::OciPrepare(request) => self.prepare(request),
            Operation::OciRun(request) => self.run(request),
            Operation::OciStop(request) => self.stop(request),
            Operation::OciDelete(request) => self.delete(request),
            _ => Err(RemoteError::new(
                ErrorCode::UnsupportedOperation,
                "operation is not an OCI lifecycle request",
            )),
        }
    }

    pub fn prepare(
        &mut self,
        request: &OciPrepareRequest,
    ) -> Result<OciLifecycleReply, RemoteError> {
        validate_container_id(&request.container_id)?;
        let image_digest = validate_image_identity(request)?;
        let bundle = self.validate_bundle_path(&request.container_id, &request.bundle_path)?;
        let spec = validate_spec(&request.oci_spec, &bundle, self.config.max_spec_bytes)?;
        validate_rootfs_record(&bundle, &image_digest)?;

        if self
            .query_state(&request.container_id, self.config.command_timeout)?
            .is_some()
        {
            if !request.replace {
                return Err(RemoteError::new(
                    ErrorCode::AlreadyExists,
                    format!("container {} already exists", request.container_id),
                ));
            }
            self.runtime_delete(&request.container_id, true)?;
            if self
                .query_state(&request.container_id, self.config.command_timeout)?
                .is_some()
            {
                return Err(RemoteError::new(
                    ErrorCode::Oci,
                    format!(
                        "OCI runtime retained container {} after forced delete",
                        request.container_id
                    ),
                ));
            }
        }

        write_spec_atomically(&bundle, &spec)?;
        let args = self.runtime_args([
            OsString::from("create"),
            OsString::from("--bundle"),
            bundle.as_os_str().to_owned(),
            OsString::from(&request.container_id),
        ]);
        if let Err(error) = self.run_checked("create", &args, self.config.command_timeout) {
            let _ = self.runtime_delete(&request.container_id, true);
            return Err(error);
        }

        let state = match self.query_state(&request.container_id, self.config.command_timeout) {
            Ok(Some(state)) if state.state == ContainerState::Created => state,
            Ok(Some(state)) => {
                let _ = self.runtime_delete(&request.container_id, true);
                return Err(RemoteError::new(
                    ErrorCode::Oci,
                    format!(
                        "OCI runtime created container {} in unexpected {:?} state",
                        request.container_id, state.state
                    ),
                ));
            }
            Ok(None) => {
                return Err(RemoteError::new(
                    ErrorCode::Oci,
                    format!(
                        "OCI runtime did not retain newly created container {}",
                        request.container_id
                    ),
                ));
            }
            Err(error) => {
                let _ = self.runtime_delete(&request.container_id, true);
                return Err(error);
            }
        };

        Ok(reply(
            ResponsePayload::OciPrepared {
                container_id: request.container_id.clone(),
                image_digest,
            },
            EventKind::OciStateChanged {
                container_id: request.container_id.clone(),
                state: state.state,
                exit_code: None,
            },
        ))
    }

    pub fn run(&mut self, request: &OciRunRequest) -> Result<OciLifecycleReply, RemoteError> {
        validate_container_id(&request.container_id)?;
        if request.attach {
            return Err(RemoteError::new(
                ErrorCode::UnsupportedOperation,
                "OCI attach requires a negotiated console or OCI-exec stream",
            ));
        }
        let current = self.require_state(&request.container_id)?;
        match current.state {
            ContainerState::Created => {}
            ContainerState::Running => {
                return Err(RemoteError::new(
                    ErrorCode::AlreadyExists,
                    format!("container {} is already running", request.container_id),
                ));
            }
            _ => {
                return Err(RemoteError::new(
                    ErrorCode::Oci,
                    format!(
                        "container {} cannot start from {:?} state",
                        request.container_id, current.state
                    ),
                ));
            }
        }

        let args = self.runtime_args([
            OsString::from("start"),
            OsString::from(&request.container_id),
        ]);
        self.run_checked("start", &args, self.config.command_timeout)?;
        let state = self.require_state(&request.container_id)?;
        if !matches!(
            state.state,
            ContainerState::Running | ContainerState::Stopped
        ) {
            return Err(RemoteError::new(
                ErrorCode::Oci,
                format!(
                    "container {} remained in {:?} state after start",
                    request.container_id, state.state
                ),
            ));
        }
        Ok(state_reply(&request.container_id, state))
    }

    pub fn stop(&mut self, request: &OciStopRequest) -> Result<OciLifecycleReply, RemoteError> {
        validate_container_id(&request.container_id)?;
        let signal = normalize_signal(request.signal.as_deref())?;
        let timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(self.config.stop_timeout);
        if timeout.is_zero() || timeout > MAX_STOP_TIMEOUT {
            return Err(RemoteError::new(
                ErrorCode::InvalidRequest,
                format!(
                    "OCI stop timeout must be between 1 and {} ms",
                    MAX_STOP_TIMEOUT.as_millis()
                ),
            ));
        }

        let current = self.require_state(&request.container_id)?;
        if current.state != ContainerState::Running {
            return Ok(state_reply(&request.container_id, current));
        }
        self.runtime_kill(&request.container_id, signal)?;
        if let Some(state) = self.wait_until_stopped(&request.container_id, timeout)? {
            return Ok(state_reply(&request.container_id, state));
        }

        if signal != "SIGKILL" {
            self.runtime_kill(&request.container_id, "SIGKILL")?;
        }
        if let Some(state) =
            self.wait_until_stopped(&request.container_id, self.config.force_cleanup_timeout)?
        {
            return Ok(state_reply(&request.container_id, state));
        }
        Err(RemoteError::new(
            ErrorCode::DeadlineExceeded,
            format!(
                "container {} did not stop before its deadline",
                request.container_id
            ),
        ))
    }

    pub fn delete(&mut self, request: &OciDeleteRequest) -> Result<OciLifecycleReply, RemoteError> {
        validate_container_id(&request.container_id)?;
        let state = self.query_state(&request.container_id, self.config.command_timeout)?;
        if state.is_none() {
            if request.force {
                return Ok(state_reply(
                    &request.container_id,
                    RuntimeState {
                        state: ContainerState::Deleted,
                        pid: None,
                    },
                ));
            }
            return Err(RemoteError::new(
                ErrorCode::NotFound,
                format!("container {} does not exist", request.container_id),
            ));
        }
        self.runtime_delete(&request.container_id, request.force)?;
        if self
            .query_state(&request.container_id, self.config.command_timeout)?
            .is_some()
        {
            return Err(RemoteError::new(
                ErrorCode::Oci,
                format!(
                    "OCI runtime retained container {} after delete",
                    request.container_id
                ),
            ));
        }
        Ok(state_reply(
            &request.container_id,
            RuntimeState {
                state: ContainerState::Deleted,
                pid: None,
            },
        ))
    }

    fn validate_bundle_path(
        &self,
        container_id: &str,
        supplied: &str,
    ) -> Result<PathBuf, RemoteError> {
        let supplied = Path::new(supplied);
        if !is_clean_absolute_path(supplied) {
            return Err(invalid("bundle_path must be a clean absolute path"));
        }
        let expected = self.config.bundle_root.join(container_id);
        if supplied != expected {
            return Err(RemoteError::new(
                ErrorCode::PermissionDenied,
                format!("bundle_path must equal the configured bundle root plus {container_id}"),
            ));
        }
        let metadata = fs::symlink_metadata(supplied).map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to inspect OCI bundle: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid(
                "OCI bundle must be an existing non-symlink directory",
            ));
        }
        let canonical = supplied.canonicalize().map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to resolve OCI bundle: {error}"),
            )
        })?;
        if canonical.parent() != Some(self.config.bundle_root.as_path()) {
            return Err(RemoteError::new(
                ErrorCode::PermissionDenied,
                "OCI bundle escaped the configured bundle root",
            ));
        }
        Ok(canonical)
    }

    fn runtime_args<const N: usize>(&self, command: [OsString; N]) -> Vec<OsString> {
        let mut args = Vec::with_capacity(N.saturating_add(2));
        if let Some(root) = &self.config.runtime_root {
            args.push(OsString::from("--root"));
            args.push(root.as_os_str().to_owned());
        }
        args.extend(command);
        args
    }

    fn run_checked(
        &mut self,
        operation: &str,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<RuntimeOutput, RemoteError> {
        let output = self.runner.run(args, timeout)?;
        if output.success() {
            Ok(output)
        } else {
            Err(runtime_failure(operation, &output))
        }
    }

    fn query_state(
        &mut self,
        container_id: &str,
        timeout: Duration,
    ) -> Result<Option<RuntimeState>, RemoteError> {
        let args = self.runtime_args([OsString::from("state"), OsString::from(container_id)]);
        let output = self.runner.run(&args, timeout)?;
        if !output.success() {
            if output_indicates_missing(&output) {
                return Ok(None);
            }
            return Err(runtime_failure("state", &output));
        }
        let state: WireRuntimeState = serde_json::from_slice(&output.stdout).map_err(|error| {
            RemoteError::new(
                ErrorCode::Oci,
                format!("OCI runtime returned invalid state JSON: {error}"),
            )
        })?;
        if state.id != container_id {
            return Err(RemoteError::new(
                ErrorCode::Oci,
                "OCI runtime state response used a different container id",
            ));
        }
        Ok(Some(RuntimeState {
            state: parse_runtime_status(&state.status)?,
            pid: state.pid.filter(|pid| *pid != 0),
        }))
    }

    fn require_state(&mut self, container_id: &str) -> Result<RuntimeState, RemoteError> {
        self.query_state(container_id, self.config.command_timeout)?
            .ok_or_else(|| {
                RemoteError::new(
                    ErrorCode::NotFound,
                    format!("container {container_id} does not exist"),
                )
            })
    }

    fn runtime_kill(&mut self, container_id: &str, signal: &str) -> Result<(), RemoteError> {
        let args = self.runtime_args([
            OsString::from("kill"),
            OsString::from(container_id),
            OsString::from(signal),
        ]);
        self.run_checked("kill", &args, self.config.command_timeout)?;
        Ok(())
    }

    fn runtime_delete(&mut self, container_id: &str, force: bool) -> Result<(), RemoteError> {
        let mut command = vec![OsString::from("delete")];
        if force {
            command.push(OsString::from("--force"));
        }
        command.push(OsString::from(container_id));
        let args = if let Some(root) = &self.config.runtime_root {
            let mut args = vec![OsString::from("--root"), root.as_os_str().to_owned()];
            args.extend(command);
            args
        } else {
            command
        };
        self.run_checked("delete", &args, self.config.command_timeout)?;
        Ok(())
    }

    fn wait_until_stopped(
        &mut self,
        container_id: &str,
        timeout: Duration,
    ) -> Result<Option<RuntimeState>, RemoteError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| invalid("OCI stop timeout cannot be represented on this platform"))?;
        loop {
            let now = Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            let state_timeout = self
                .config
                .command_timeout
                .min(remaining.max(Duration::from_millis(1)));
            match self.query_state(container_id, state_timeout)? {
                Some(state) if state.state == ContainerState::Stopped => {
                    return Ok(Some(state));
                }
                None => return Ok(None),
                Some(_) if now >= deadline => return Ok(None),
                Some(_) => thread::sleep(STATE_POLL_INTERVAL.min(remaining)),
            }
        }
    }

    #[cfg(test)]
    fn with_runner(
        config: OciRuntimeConfig,
        runner: Box<dyn RuntimeCommandRunner>,
    ) -> Result<Self, RemoteError> {
        Ok(Self {
            config: ValidatedConfig::new(config)?,
            runner,
        })
    }
}

#[derive(Clone, Debug)]
struct ValidatedConfig {
    runtime_path: PathBuf,
    bundle_root: PathBuf,
    runtime_root: Option<PathBuf>,
    output_directory: PathBuf,
    command_timeout: Duration,
    stop_timeout: Duration,
    force_cleanup_timeout: Duration,
    max_runtime_output_bytes: u64,
    max_spec_bytes: usize,
}

impl ValidatedConfig {
    fn new(config: OciRuntimeConfig) -> Result<Self, RemoteError> {
        validate_duration(
            config.command_timeout,
            MAX_COMMAND_TIMEOUT,
            "command_timeout",
        )?;
        validate_duration(config.stop_timeout, MAX_STOP_TIMEOUT, "stop_timeout")?;
        validate_duration(
            config.force_cleanup_timeout,
            MAX_FORCE_CLEANUP_TIMEOUT,
            "force_cleanup_timeout",
        )?;
        if !(1..=ABSOLUTE_MAX_RUNTIME_OUTPUT_BYTES).contains(&config.max_runtime_output_bytes) {
            return Err(invalid(format!(
                "max_runtime_output_bytes must be between 1 and {ABSOLUTE_MAX_RUNTIME_OUTPUT_BYTES}"
            )));
        }
        if !(1..=ABSOLUTE_MAX_SPEC_BYTES).contains(&config.max_spec_bytes) {
            return Err(invalid(format!(
                "max_spec_bytes must be between 1 and {ABSOLUTE_MAX_SPEC_BYTES}"
            )));
        }

        let runtime_path = validate_runtime_path(&config.runtime_path)?;
        let bundle_root = prepare_directory(&config.bundle_root, "bundle root")?;
        let output_directory = prepare_directory(
            &bundle_root.join(".rish-runtime-output"),
            "output directory",
        )?;
        if output_directory.parent() != Some(bundle_root.as_path()) {
            return Err(RemoteError::new(
                ErrorCode::PermissionDenied,
                "OCI runtime output directory escaped the bundle root",
            ));
        }
        let runtime_root = config
            .runtime_root
            .as_deref()
            .map(|path| prepare_directory(path, "runtime state root"))
            .transpose()?;

        Ok(Self {
            runtime_path,
            bundle_root,
            runtime_root,
            output_directory,
            command_timeout: config.command_timeout,
            stop_timeout: config.stop_timeout,
            force_cleanup_timeout: config.force_cleanup_timeout,
            max_runtime_output_bytes: config.max_runtime_output_bytes,
            max_spec_bytes: config.max_spec_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct RuntimeState {
    state: ContainerState,
    pid: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct WireRuntimeState {
    id: String,
    status: String,
    #[serde(default)]
    pid: Option<u32>,
}

fn parse_runtime_status(status: &str) -> Result<ContainerState, RemoteError> {
    match status {
        "creating" => Ok(ContainerState::Preparing),
        "created" => Ok(ContainerState::Created),
        "running" => Ok(ContainerState::Running),
        "stopped" => Ok(ContainerState::Stopped),
        _ => Err(RemoteError::new(
            ErrorCode::Oci,
            format!("OCI runtime returned unsupported state {status:?}"),
        )),
    }
}

fn runtime_failure(operation: &str, output: &RuntimeOutput) -> RemoteError {
    let summary = output_summary(output);
    let code = output
        .exit_code
        .map_or_else(|| "signal".to_owned(), |code| code.to_string());
    let suffix = if summary.is_empty() {
        String::new()
    } else {
        format!(": {summary}")
    };
    RemoteError::new(
        ErrorCode::Oci,
        format!("OCI runtime {operation} failed with status {code}{suffix}"),
    )
}

fn output_indicates_missing(output: &RuntimeOutput) -> bool {
    let summary = output_summary(output).to_ascii_lowercase();
    [
        "does not exist",
        "doesn't exist",
        "not found",
        "not exist",
        "no such container",
    ]
    .iter()
    .any(|needle| summary.contains(needle))
}

fn output_summary(output: &RuntimeOutput) -> String {
    let source = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let mut summary = String::from_utf8_lossy(source)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if summary.len() > 512 {
        let mut boundary = 512;
        while !summary.is_char_boundary(boundary) {
            boundary -= 1;
        }
        summary.truncate(boundary);
    }
    summary.trim().to_owned()
}

fn state_reply(container_id: &str, state: RuntimeState) -> OciLifecycleReply {
    reply(
        ResponsePayload::OciState {
            container_id: container_id.to_owned(),
            state: state.state,
            pid: state.pid,
        },
        EventKind::OciStateChanged {
            container_id: container_id.to_owned(),
            state: state.state,
            exit_code: None,
        },
    )
}

fn reply(response: ResponsePayload, event: EventKind) -> OciLifecycleReply {
    OciLifecycleReply {
        response,
        events: vec![event],
    }
}

fn invalid(message: impl Into<String>) -> RemoteError {
    RemoteError::new(ErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests;
