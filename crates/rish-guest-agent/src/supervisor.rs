use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rish_guest_protocol::{
    CancelRequest, ErrorCode, EventKind, ExecRequest, Operation, RemoteError, RequestId,
    ResponsePayload, StreamChannel,
};

use crate::{HandlerEvent, HandlerReply, OperationHandler};

pub const DEFAULT_STREAM_CHUNK_SIZE: u32 = 32 * 1024;
pub const DEFAULT_STREAM_OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
pub const DEFAULT_MAX_CONCURRENT_EXEC: u32 = 4;

const ABSOLUTE_MAX_CONCURRENT_EXEC: u32 = 16;
const ABSOLUTE_MAX_STREAM_CHUNK_SIZE: u32 = 32 * 1024;
const ABSOLUTE_MAX_STREAM_OUTPUT: u64 = 64 * 1024 * 1024;
const CANCEL_GRACE_PERIOD: Duration = Duration::from_millis(250);
const POST_EXIT_DRAIN_GRACE_PERIOD: Duration = Duration::from_millis(250);
const SIGKILL_NUMBER: i32 = 9;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeExecutionConfig {
    pub max_concurrent_exec: u32,
    pub max_stream_chunk_size: u32,
    pub max_stdout_bytes: u64,
    pub max_stderr_bytes: u64,
}

impl Default for NativeExecutionConfig {
    fn default() -> Self {
        Self {
            max_concurrent_exec: DEFAULT_MAX_CONCURRENT_EXEC,
            max_stream_chunk_size: DEFAULT_STREAM_CHUNK_SIZE,
            max_stdout_bytes: DEFAULT_STREAM_OUTPUT_LIMIT,
            max_stderr_bytes: DEFAULT_STREAM_OUTPUT_LIMIT,
        }
    }
}

impl NativeExecutionConfig {
    fn validate(&self) -> Result<(), RemoteError> {
        if !(1..=ABSOLUTE_MAX_CONCURRENT_EXEC).contains(&self.max_concurrent_exec) {
            return Err(invalid_config(format!(
                "max_concurrent_exec must be between 1 and {ABSOLUTE_MAX_CONCURRENT_EXEC}"
            )));
        }
        if !(1..=ABSOLUTE_MAX_STREAM_CHUNK_SIZE).contains(&self.max_stream_chunk_size) {
            return Err(invalid_config(format!(
                "max_stream_chunk_size must be between 1 and {ABSOLUTE_MAX_STREAM_CHUNK_SIZE}"
            )));
        }
        if self.max_stdout_bytes > ABSOLUTE_MAX_STREAM_OUTPUT
            || self.max_stderr_bytes > ABSOLUTE_MAX_STREAM_OUTPUT
        {
            return Err(invalid_config(format!(
                "stream output limits must not exceed {ABSOLUTE_MAX_STREAM_OUTPUT} bytes"
            )));
        }
        Ok(())
    }
}

fn invalid_config(message: impl Into<String>) -> RemoteError {
    RemoteError::new(ErrorCode::InvalidRequest, message)
}

#[derive(Debug)]
pub struct NativeOperationHandler {
    config: NativeExecutionConfig,
    next_execution_id: u64,
    executions: BTreeMap<String, Execution>,
    pending_events: VecDeque<HandlerEvent>,
}

impl Default for NativeOperationHandler {
    fn default() -> Self {
        Self::new(NativeExecutionConfig::default())
            .expect("default native execution configuration is valid")
    }
}

impl NativeOperationHandler {
    pub fn new(config: NativeExecutionConfig) -> Result<Self, RemoteError> {
        config.validate()?;
        let event_capacity = usize::try_from(config.max_concurrent_exec)
            .unwrap_or(ABSOLUTE_MAX_CONCURRENT_EXEC as usize)
            .saturating_mul(4);
        Ok(Self {
            config,
            next_execution_id: 1,
            executions: BTreeMap::new(),
            pending_events: VecDeque::with_capacity(event_capacity),
        })
    }

    #[must_use]
    pub fn active_execution_count(&self) -> usize {
        self.executions.len()
    }

    fn start(
        &mut self,
        request_id: &RequestId,
        request: &ExecRequest,
    ) -> Result<HandlerReply, RemoteError> {
        validate_exec_request(request)?;
        if self
            .executions
            .values()
            .any(|execution| execution.request_id == *request_id)
        {
            return Err(RemoteError::new(
                ErrorCode::AlreadyExists,
                format!("request {request_id} already owns an active execution"),
            ));
        }
        if self.executions.len()
            >= usize::try_from(self.config.max_concurrent_exec).unwrap_or(usize::MAX)
        {
            return Err(RemoteError::new(
                ErrorCode::ResourceExhausted,
                format!(
                    "maximum concurrent exec limit {} is reached",
                    self.config.max_concurrent_exec
                ),
            ));
        }

        let program = &request.argv[0];
        let mut command = Command::new(program);
        command.args(&request.argv[1..]);
        command.env_clear();
        command.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
        command.envs(&request.env);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::null());
        command.stdout(if request.attach_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stderr(if request.attach_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        configure_process_group(&mut command);

        let mut child = command.spawn().map_err(|error| {
            RemoteError::new(ErrorCode::Io, format!("failed to spawn {program}: {error}"))
        })?;
        let pid = child.id();
        let streams = match take_output_streams(&mut child, request, &self.config) {
            Ok(streams) => streams,
            Err(error) => {
                terminate_and_reap(&mut child);
                return Err(error);
            }
        };
        let deadline = request
            .timeout_ms
            .map(Duration::from_millis)
            .and_then(|timeout| Instant::now().checked_add(timeout));
        if request.timeout_ms.is_some() && deadline.is_none() {
            terminate_and_reap(&mut child);
            return Err(RemoteError::new(
                ErrorCode::InvalidRequest,
                "exec timeout is too large",
            ));
        }

        let execution_id = self.allocate_execution_id();
        self.executions.insert(
            execution_id.clone(),
            Execution {
                request_id: request_id.clone(),
                execution_id: execution_id.clone(),
                child,
                pid,
                stdout: streams.stdout,
                stderr: streams.stderr,
                deadline,
                force_kill_at: None,
                termination_requested: false,
                status: None,
                group_cleanup_sent: false,
                drain_deadline: None,
            },
        );

        Ok(HandlerReply {
            response: ResponsePayload::ExecStarted {
                execution_id: execution_id.clone(),
                pid,
            },
            events: vec![EventKind::ExecutionStarted { execution_id, pid }],
        })
    }

    fn cancel(&mut self, request: &CancelRequest) -> Result<HandlerReply, RemoteError> {
        let signal = validate_cancel_signal(request.signal)?;
        let execution_id = self
            .executions
            .iter()
            .find(|(execution_id, execution)| {
                execution.request_id == request.target_request_id
                    && request
                        .execution_id
                        .as_ref()
                        .is_none_or(|expected| expected == *execution_id)
            })
            .map(|(execution_id, _)| execution_id.clone())
            .ok_or_else(|| {
                RemoteError::new(
                    ErrorCode::NotFound,
                    "cancel target does not identify an active execution",
                )
            })?;
        let execution = self
            .executions
            .get_mut(&execution_id)
            .expect("execution was selected from the same table");
        execution.request_cancel(signal).map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to signal execution {execution_id}: {error}"),
            )
        })?;
        Ok(HandlerReply {
            response: ResponsePayload::Cancelled {
                target_request_id: request.target_request_id.clone(),
            },
            events: Vec::new(),
        })
    }

    fn allocate_execution_id(&mut self) -> String {
        loop {
            let candidate = format!("exec-{}", self.next_execution_id);
            self.next_execution_id = self.next_execution_id.checked_add(1).unwrap_or(1);
            if !self.executions.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    fn tick(&mut self) {
        let execution_ids = self.executions.keys().cloned().collect::<Vec<_>>();
        let mut completed = Vec::new();
        for execution_id in execution_ids {
            let Some(execution) = self.executions.get_mut(&execution_id) else {
                continue;
            };
            let (events, finished) =
                execution.tick(usize::try_from(self.config.max_stream_chunk_size).unwrap_or(1));
            for event in events {
                self.pending_events.push_back(HandlerEvent {
                    request_id: Some(execution.request_id.clone()),
                    event,
                });
            }
            if finished {
                completed.push(execution_id);
            }
        }
        for execution_id in completed {
            self.executions.remove(&execution_id);
        }
    }
}

impl OperationHandler for NativeOperationHandler {
    fn handle(
        &mut self,
        request_id: &RequestId,
        operation: &Operation,
    ) -> Result<HandlerReply, RemoteError> {
        match operation {
            Operation::Exec(request) => self.start(request_id, request),
            Operation::Cancel(request) => self.cancel(request),
            Operation::Stream(_)
            | Operation::OciPrepare(_)
            | Operation::OciRun(_)
            | Operation::OciStop(_)
            | Operation::OciDelete(_)
            | Operation::PortForward(_)
            | Operation::Checkpoint(_) => Err(RemoteError::new(
                ErrorCode::UnsupportedOperation,
                "operation is not enabled in the bootstrap guest agent",
            )),
            Operation::Ping(_) => unreachable!("ping is handled by GuestAgent"),
        }
    }

    fn poll_event(&mut self) -> Option<HandlerEvent> {
        if self.pending_events.is_empty() {
            self.tick();
        }
        self.pending_events.pop_front()
    }
}

impl Drop for NativeOperationHandler {
    fn drop(&mut self) {
        for execution in self.executions.values_mut() {
            execution.force_terminate_and_reap();
        }
    }
}

fn validate_exec_request(request: &ExecRequest) -> Result<(), RemoteError> {
    if request.argv.is_empty() {
        return Err(RemoteError::new(
            ErrorCode::InvalidRequest,
            "exec argv must not be empty",
        ));
    }
    if request.tty || request.attach_stdin || request.user.is_some() {
        return Err(RemoteError::new(
            ErrorCode::UnsupportedOperation,
            "bootstrap exec supports non-interactive commands without user overrides",
        ));
    }
    Ok(())
}

struct OutputStreams {
    stdout: Option<OutputPipe<ChildStdout>>,
    stderr: Option<OutputPipe<ChildStderr>>,
}

fn take_output_streams(
    child: &mut Child,
    request: &ExecRequest,
    config: &NativeExecutionConfig,
) -> Result<OutputStreams, RemoteError> {
    let stdout = if request.attach_stdout {
        let stdout = child.stdout.take().ok_or_else(|| {
            RemoteError::new(
                ErrorCode::Internal,
                "spawned process did not expose its piped stdout",
            )
        })?;
        set_nonblocking(&stdout).map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to configure child stdout: {error}"),
            )
        })?;
        Some(OutputPipe::new(stdout, config.max_stdout_bytes))
    } else {
        None
    };
    let stderr = if request.attach_stderr {
        let stderr = child.stderr.take().ok_or_else(|| {
            RemoteError::new(
                ErrorCode::Internal,
                "spawned process did not expose its piped stderr",
            )
        })?;
        set_nonblocking(&stderr).map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to configure child stderr: {error}"),
            )
        })?;
        Some(OutputPipe::new(stderr, config.max_stderr_bytes))
    } else {
        None
    };
    Ok(OutputStreams { stdout, stderr })
}

#[derive(Debug)]
struct Execution {
    request_id: RequestId,
    execution_id: String,
    child: Child,
    pid: u32,
    stdout: Option<OutputPipe<ChildStdout>>,
    stderr: Option<OutputPipe<ChildStderr>>,
    deadline: Option<Instant>,
    force_kill_at: Option<Instant>,
    termination_requested: bool,
    status: Option<ExitStatus>,
    group_cleanup_sent: bool,
    drain_deadline: Option<Instant>,
}

impl Execution {
    fn tick(&mut self, chunk_size: usize) -> (Vec<EventKind>, bool) {
        let now = Instant::now();
        if !self.termination_requested && self.deadline.is_some_and(|deadline| now >= deadline) {
            self.force_kill();
        }
        if self
            .force_kill_at
            .is_some_and(|force_kill_at| now >= force_kill_at)
        {
            self.force_kill();
        }

        self.observe_child_exit();
        let mut events = Vec::with_capacity(3);
        if self
            .drain_deadline
            .is_some_and(|drain_deadline| now >= drain_deadline)
        {
            if let Some(event) =
                close_output(&mut self.stdout, &self.execution_id, StreamChannel::Stdout)
            {
                events.push(event);
            }
            if let Some(event) =
                close_output(&mut self.stderr, &self.execution_id, StreamChannel::Stderr)
            {
                events.push(event);
            }
        } else {
            let stdout = poll_output(
                &mut self.stdout,
                &self.execution_id,
                StreamChannel::Stdout,
                chunk_size,
            );
            if let Some(event) = stdout.event {
                events.push(event);
            }
            let stderr = poll_output(
                &mut self.stderr,
                &self.execution_id,
                StreamChannel::Stderr,
                chunk_size,
            );
            if let Some(event) = stderr.event {
                events.push(event);
            }
            if stdout.fault || stderr.fault {
                self.force_kill();
            }
        }
        self.observe_child_exit();

        let finished = if let Some(status) = self.status.as_ref() {
            if self.stdout.is_none() && self.stderr.is_none() {
                events.push(EventKind::ProcessExited {
                    execution_id: self.execution_id.clone(),
                    exit_code: status.code(),
                    signal: platform_signal(status),
                });
                true
            } else {
                false
            }
        } else {
            false
        };
        (events, finished)
    }

    fn request_cancel(&mut self, signal: i32) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        self.send_signal(signal)?;
        self.termination_requested = true;
        self.deadline = None;
        self.force_kill_at =
            (signal != SIGKILL_NUMBER).then(|| Instant::now() + CANCEL_GRACE_PERIOD);
        Ok(())
    }

    fn force_kill(&mut self) {
        if self.status.is_none() && self.send_signal(SIGKILL_NUMBER).is_err() {
            let _ = self.child.kill();
        }
        self.termination_requested = true;
        self.deadline = None;
        self.force_kill_at = None;
    }

    fn send_signal(&mut self, signal: i32) -> io::Result<()> {
        match signal_process_group(self.pid, signal) {
            Ok(()) => Ok(()),
            Err(group_error) if signal == SIGKILL_NUMBER => self.child.kill().or(Err(group_error)),
            Err(error) => Err(error),
        }
    }

    fn observe_child_exit(&mut self) {
        if self.status.is_some() {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.status = Some(status);
                if !self.group_cleanup_sent {
                    let _ = signal_process_group(self.pid, SIGKILL_NUMBER);
                    self.group_cleanup_sent = true;
                }
                self.drain_deadline = Some(Instant::now() + POST_EXIT_DRAIN_GRACE_PERIOD);
            }
            Ok(None) => {}
            Err(_) => self.force_kill(),
        }
    }

    fn force_terminate_and_reap(&mut self) {
        let _ = signal_process_group(self.pid, SIGKILL_NUMBER);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stdout = None;
        self.stderr = None;
    }
}

#[derive(Debug)]
struct OutputPipe<R> {
    reader: R,
    sequence: u64,
    sent: u64,
    limit: u64,
}

impl<R> OutputPipe<R> {
    fn new(reader: R, limit: u64) -> Self {
        Self {
            reader,
            sequence: 0,
            sent: 0,
            limit,
        }
    }
}

struct OutputPoll {
    event: Option<EventKind>,
    fault: bool,
}

fn poll_output<R: io::Read>(
    output: &mut Option<OutputPipe<R>>,
    execution_id: &str,
    channel: StreamChannel,
    chunk_size: usize,
) -> OutputPoll {
    let Some(pipe) = output.as_mut() else {
        return OutputPoll {
            event: None,
            fault: false,
        };
    };
    let remaining = pipe.limit.saturating_sub(pipe.sent);
    let read_limit = chunk_size
        .min(usize::try_from(remaining.saturating_add(1)).unwrap_or(usize::MAX))
        .max(1);
    let mut buffer = [0_u8; ABSOLUTE_MAX_STREAM_CHUNK_SIZE as usize];
    match pipe.reader.read(&mut buffer[..read_limit]) {
        Ok(0) => {
            let event = stream_event(execution_id, channel, pipe.sequence, &[], true);
            *output = None;
            OutputPoll {
                event: Some(event),
                fault: false,
            }
        }
        Ok(read) => {
            let retained = read.min(usize::try_from(remaining).unwrap_or(usize::MAX));
            pipe.sent = pipe
                .sent
                .saturating_add(u64::try_from(retained).unwrap_or(u64::MAX));
            let exceeded = retained < read;
            let event = stream_event(
                execution_id,
                channel,
                pipe.sequence,
                &buffer[..retained],
                exceeded,
            );
            pipe.sequence = pipe.sequence.saturating_add(1);
            if exceeded {
                *output = None;
            }
            OutputPoll {
                event: Some(event),
                fault: exceeded,
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => OutputPoll {
            event: None,
            fault: false,
        },
        Err(error) if error.kind() == io::ErrorKind::Interrupted => OutputPoll {
            event: None,
            fault: false,
        },
        Err(_) => {
            let event = stream_event(execution_id, channel, pipe.sequence, &[], true);
            *output = None;
            OutputPoll {
                event: Some(event),
                fault: true,
            }
        }
    }
}

fn close_output<R>(
    output: &mut Option<OutputPipe<R>>,
    execution_id: &str,
    channel: StreamChannel,
) -> Option<EventKind> {
    let pipe = output.take()?;
    Some(stream_event(
        execution_id,
        channel,
        pipe.sequence,
        &[],
        true,
    ))
}

fn stream_event(
    execution_id: &str,
    channel: StreamChannel,
    sequence: u64,
    bytes: &[u8],
    eof: bool,
) -> EventKind {
    EventKind::Stream {
        execution_id: execution_id.to_owned(),
        channel,
        stream_sequence: sequence,
        data_base64: BASE64.encode(bytes),
        eof,
    }
}

fn terminate_and_reap(child: &mut Child) {
    let _ = signal_process_group(child.id(), SIGKILL_NUMBER);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn set_nonblocking(stream: &impl std::os::fd::AsFd) -> io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

    let flags = fcntl_getfl(stream).map_err(io::Error::from)?;
    fcntl_setfl(stream, flags | OFlags::NONBLOCK).map_err(io::Error::from)
}

#[cfg(not(unix))]
fn set_nonblocking(_stream: &impl Sized) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native guest execution requires Unix nonblocking pipes",
    ))
}

#[cfg(unix)]
fn validate_cancel_signal(signal: Option<i32>) -> Result<i32, RemoteError> {
    use std::num::NonZeroI32;

    use rustix::process::Signal;

    let signal = signal.unwrap_or(SIGKILL_NUMBER);
    let parsed = NonZeroI32::new(signal)
        .and_then(Signal::from_named_raw_nonzero)
        .filter(|signal| {
            matches!(
                *signal,
                Signal::HUP | Signal::INT | Signal::QUIT | Signal::KILL | Signal::TERM
            )
        });
    parsed.map(|_| signal).ok_or_else(|| {
        RemoteError::new(
            ErrorCode::InvalidRequest,
            "cancel signal must be one of SIGHUP, SIGINT, SIGQUIT, SIGKILL, or SIGTERM",
        )
    })
}

#[cfg(not(unix))]
fn validate_cancel_signal(signal: Option<i32>) -> Result<i32, RemoteError> {
    match signal.unwrap_or(SIGKILL_NUMBER) {
        SIGKILL_NUMBER => Ok(SIGKILL_NUMBER),
        _ => Err(RemoteError::new(
            ErrorCode::UnsupportedOperation,
            "only forced cancellation is supported on this guest platform",
        )),
    }
}

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: i32) -> io::Result<()> {
    use std::num::NonZeroI32;

    use rustix::process::{Pid, Signal, kill_process_group};

    let raw_pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid child pid"))?;
    let signal = NonZeroI32::new(signal)
        .and_then(Signal::from_named_raw_nonzero)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid signal"))?;
    kill_process_group(raw_pid, signal).map_err(io::Error::from)
}

#[cfg(not(unix))]
fn signal_process_group(_pid: u32, _signal: i32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process groups are unavailable",
    ))
}

#[cfg(unix)]
fn platform_signal(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn platform_signal(_status: &ExitStatus) -> Option<i32> {
    None
}
