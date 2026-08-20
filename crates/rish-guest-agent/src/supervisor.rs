mod config;
mod platform;
mod pty;
mod streams;

use std::collections::{BTreeMap, VecDeque};
#[cfg(target_os = "linux")]
use std::fs::File;
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rish_guest_protocol::{
    CancelRequest, ErrorCode, EventKind, ExecRequest, Operation, RemoteError, RequestId,
    ResponsePayload, StreamAction, StreamChannel, StreamRequest,
};

use crate::{HandlerEvent, HandlerReply, OperationHandler};
#[cfg(target_os = "linux")]
use config::ABSOLUTE_MAX_STREAM_OUTPUT;
use platform::{
    configure_process_group, platform_signal, set_nonblocking, signal_process_group,
    validate_cancel_signal,
};
use streams::{InputPipe, InputWriter, OutputPipe, close_output, decode_stdin_chunk, poll_output};

const CANCEL_GRACE_PERIOD: Duration = Duration::from_millis(250);
const POST_EXIT_DRAIN_GRACE_PERIOD: Duration = Duration::from_millis(250);
const SIGKILL_NUMBER: i32 = 9;

pub use config::{
    DEFAULT_MAX_CONCURRENT_EXEC, DEFAULT_STREAM_CHUNK_SIZE, DEFAULT_STREAM_OUTPUT_LIMIT,
    NativeExecutionConfig,
};

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
            .unwrap_or(usize::MAX)
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
        if request.tty && !request.env.contains_key("TERM") {
            command.env("TERM", "xterm-256color");
        }
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }

        #[cfg(target_os = "linux")]
        let prepared_pty = if request.tty {
            Some(pty::prepare(&mut command, request.attach_stdin)?)
        } else {
            configure_pipe_stdio(&mut command, request);
            configure_process_group(&mut command);
            None
        };
        #[cfg(not(target_os = "linux"))]
        {
            configure_pipe_stdio(&mut command, request);
            configure_process_group(&mut command);
        }

        let mut child = command.spawn().map_err(|error| {
            RemoteError::new(ErrorCode::Io, format!("failed to spawn {program}: {error}"))
        })?;
        let pid = child.id();
        #[cfg(target_os = "linux")]
        let streams = take_execution_streams(&mut child, request, &self.config, prepared_pty);
        #[cfg(not(target_os = "linux"))]
        let streams = take_execution_streams(&mut child, request, &self.config);
        let streams = match streams {
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
                stdin: streams.stdin,
                stdout: streams.stdout,
                stderr: streams.stderr,
                #[cfg(target_os = "linux")]
                console: streams.console,
                #[cfg(target_os = "linux")]
                pty: streams.pty,
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

    fn stream(&mut self, request: &StreamRequest) -> Result<HandlerReply, RemoteError> {
        let execution = self
            .executions
            .get_mut(&request.execution_id)
            .ok_or_else(|| {
                RemoteError::new(
                    ErrorCode::NotFound,
                    format!(
                        "stream target {} does not identify an active execution",
                        request.execution_id
                    ),
                )
            })?;
        if execution.termination_requested || execution.status.is_some() {
            return Err(RemoteError::new(
                ErrorCode::NotFound,
                format!("execution {} is already terminating", request.execution_id),
            ));
        }

        match &request.action {
            StreamAction::WriteStdin { data_base64 } => {
                let bytes = decode_stdin_chunk(data_base64, self.config.max_stream_chunk_size)?;
                execution
                    .stdin
                    .as_mut()
                    .ok_or_else(|| {
                        RemoteError::new(
                            ErrorCode::InvalidRequest,
                            format!(
                                "execution {} was not started with attached stdin",
                                request.execution_id
                            ),
                        )
                    })?
                    .enqueue(&bytes)?;
            }
            StreamAction::CloseStdin => {
                execution
                    .stdin
                    .as_mut()
                    .ok_or_else(|| {
                        RemoteError::new(
                            ErrorCode::InvalidRequest,
                            format!(
                                "execution {} was not started with attached stdin",
                                request.execution_id
                            ),
                        )
                    })?
                    .request_close()?;
            }
            StreamAction::ResizeTty { rows, columns } => {
                if *rows == 0 || *columns == 0 {
                    return Err(RemoteError::new(
                        ErrorCode::InvalidRequest,
                        "PTY rows and columns must both be non-zero",
                    ));
                }
                #[cfg(target_os = "linux")]
                {
                    let control = execution.pty.as_ref().ok_or_else(|| {
                        RemoteError::new(
                            ErrorCode::InvalidRequest,
                            format!("execution {} does not own a PTY", request.execution_id),
                        )
                    })?;
                    control.resize(*rows, *columns).map_err(|error| {
                        RemoteError::new(
                            ErrorCode::Io,
                            format!(
                                "failed to resize PTY for execution {}: {error}",
                                request.execution_id
                            ),
                        )
                    })?;
                }
                #[cfg(not(target_os = "linux"))]
                {
                    return Err(RemoteError::new(
                        ErrorCode::UnsupportedOperation,
                        "PTY resize is supported only by the Linux guest agent",
                    ));
                }
            }
        }

        Ok(HandlerReply {
            response: ResponsePayload::StreamAccepted {
                execution_id: request.execution_id.clone(),
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
            Operation::Stream(request) => self.stream(request),
            Operation::OciPrepare(_)
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
    if request.user.is_some() {
        return Err(RemoteError::new(
            ErrorCode::UnsupportedOperation,
            "bootstrap exec does not support user overrides",
        ));
    }
    if request.tty && !(request.attach_stdout && request.attach_stderr) {
        return Err(RemoteError::new(
            ErrorCode::InvalidRequest,
            "TTY exec requires both stdout and stderr attachment because the PTY exposes one merged console stream",
        ));
    }
    #[cfg(not(target_os = "linux"))]
    if request.tty {
        return Err(RemoteError::new(
            ErrorCode::UnsupportedOperation,
            "real PTY exec is supported only by the Linux guest agent",
        ));
    }
    Ok(())
}

fn configure_pipe_stdio(command: &mut Command, request: &ExecRequest) {
    command.stdin(if request.attach_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    });
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
}

struct ExecutionStreams {
    stdin: Option<InputPipe>,
    stdout: Option<OutputPipe<ChildStdout>>,
    stderr: Option<OutputPipe<ChildStderr>>,
    #[cfg(target_os = "linux")]
    console: Option<OutputPipe<File>>,
    #[cfg(target_os = "linux")]
    pty: Option<pty::PtyControl>,
}

#[cfg(target_os = "linux")]
fn take_execution_streams(
    child: &mut Child,
    request: &ExecRequest,
    config: &NativeExecutionConfig,
    prepared_pty: Option<pty::PreparedPty>,
) -> Result<ExecutionStreams, RemoteError> {
    if let Some(prepared_pty) = prepared_pty {
        let console_limit = config
            .max_stdout_bytes
            .saturating_add(config.max_stderr_bytes)
            .min(ABSOLUTE_MAX_STREAM_OUTPUT);
        return Ok(ExecutionStreams {
            stdin: prepared_pty.writer.map(|writer| {
                InputPipe::new(InputWriter::Pty(writer), config.max_stream_chunk_size, true)
            }),
            stdout: None,
            stderr: None,
            console: Some(OutputPipe::pty(prepared_pty.reader, console_limit)),
            pty: Some(prepared_pty.control),
        });
    }
    let (stdin, stdout, stderr) = take_pipe_streams(child, request, config)?;
    Ok(ExecutionStreams {
        stdin,
        stdout,
        stderr,
        console: None,
        pty: None,
    })
}

#[cfg(not(target_os = "linux"))]
fn take_execution_streams(
    child: &mut Child,
    request: &ExecRequest,
    config: &NativeExecutionConfig,
) -> Result<ExecutionStreams, RemoteError> {
    let (stdin, stdout, stderr) = take_pipe_streams(child, request, config)?;
    Ok(ExecutionStreams {
        stdin,
        stdout,
        stderr,
    })
}

type PipeStreams = (
    Option<InputPipe>,
    Option<OutputPipe<ChildStdout>>,
    Option<OutputPipe<ChildStderr>>,
);

fn take_pipe_streams(
    child: &mut Child,
    request: &ExecRequest,
    config: &NativeExecutionConfig,
) -> Result<PipeStreams, RemoteError> {
    let stdin = if request.attach_stdin {
        let stdin = child.stdin.take().ok_or_else(|| {
            RemoteError::new(
                ErrorCode::Internal,
                "spawned process did not expose its piped stdin",
            )
        })?;
        set_nonblocking(&stdin).map_err(|error| {
            RemoteError::new(
                ErrorCode::Io,
                format!("failed to configure child stdin: {error}"),
            )
        })?;
        Some(InputPipe::new(
            InputWriter::Pipe(stdin),
            config.max_stream_chunk_size,
            false,
        ))
    } else {
        None
    };
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
    Ok((stdin, stdout, stderr))
}

#[derive(Debug)]
struct Execution {
    request_id: RequestId,
    execution_id: String,
    child: Child,
    pid: u32,
    stdin: Option<InputPipe>,
    stdout: Option<OutputPipe<ChildStdout>>,
    stderr: Option<OutputPipe<ChildStderr>>,
    #[cfg(target_os = "linux")]
    console: Option<OutputPipe<File>>,
    #[cfg(target_os = "linux")]
    pty: Option<pty::PtyControl>,
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
        if self
            .stdin
            .as_mut()
            .is_some_and(|stdin| stdin.flush_pending().is_err())
        {
            self.stdin = None;
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
            #[cfg(target_os = "linux")]
            if let Some(event) = close_output(
                &mut self.console,
                &self.execution_id,
                StreamChannel::Console,
            ) {
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
            #[cfg(target_os = "linux")]
            let console = poll_output(
                &mut self.console,
                &self.execution_id,
                StreamChannel::Console,
                chunk_size,
            );
            #[cfg(target_os = "linux")]
            if let Some(event) = console.event {
                events.push(event);
            }
            #[cfg(target_os = "linux")]
            let output_fault = stdout.fault || stderr.fault || console.fault;
            #[cfg(not(target_os = "linux"))]
            let output_fault = stdout.fault || stderr.fault;
            if output_fault {
                self.force_kill();
            }
        }
        self.observe_child_exit();

        let finished = if let Some(status) = self.status.as_ref() {
            #[cfg(target_os = "linux")]
            let output_closed =
                self.stdout.is_none() && self.stderr.is_none() && self.console.is_none();
            #[cfg(not(target_os = "linux"))]
            let output_closed = self.stdout.is_none() && self.stderr.is_none();
            if output_closed {
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
        self.stdin = None;
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
        self.stdin = None;
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
                self.stdin = None;
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
        self.stdin = None;
        self.stdout = None;
        self.stderr = None;
        #[cfg(target_os = "linux")]
        {
            self.console = None;
            self.pty = None;
        }
    }
}

fn terminate_and_reap(child: &mut Child) {
    let _ = signal_process_group(child.id(), SIGKILL_NUMBER);
    let _ = child.kill();
    let _ = child.wait();
}
