use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::process::ChildStdin;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rish_guest_protocol::{ErrorCode, EventKind, RemoteError, StreamChannel};

use super::config::ABSOLUTE_MAX_STREAM_CHUNK_SIZE;

const STDIN_QUEUE_CHUNKS: usize = 4;
const PTY_END_OF_TRANSMISSION: u8 = 0x04;

#[derive(Debug)]
pub(super) enum InputWriter {
    Pipe(ChildStdin),
    #[cfg(target_os = "linux")]
    Pty(std::fs::File),
}

impl io::Write for InputWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Pipe(writer) => writer.write(buffer),
            #[cfg(target_os = "linux")]
            Self::Pty(writer) => writer.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Pipe(writer) => writer.flush(),
            #[cfg(target_os = "linux")]
            Self::Pty(writer) => writer.flush(),
        }
    }
}

#[derive(Debug)]
pub(super) struct InputPipe {
    writer: Option<InputWriter>,
    pending: VecDeque<u8>,
    capacity: usize,
    close_requested: bool,
    is_pty: bool,
}

impl InputPipe {
    pub(super) fn new(writer: InputWriter, max_stream_chunk_size: u32, is_pty: bool) -> Self {
        let chunk_size = usize::try_from(max_stream_chunk_size).unwrap_or(1).max(1);
        let capacity = chunk_size.saturating_mul(STDIN_QUEUE_CHUNKS);
        Self {
            writer: Some(writer),
            pending: VecDeque::with_capacity(capacity),
            capacity,
            close_requested: false,
            is_pty,
        }
    }

    pub(super) fn enqueue(&mut self, bytes: &[u8]) -> Result<(), RemoteError> {
        if self.close_requested || self.writer.is_none() {
            return Err(RemoteError::new(
                ErrorCode::InvalidRequest,
                "stdin is already closed for this execution",
            ));
        }
        self.flush_pending().map_err(stdin_io_error)?;
        if bytes.len() > self.capacity.saturating_sub(self.pending.len()) {
            return Err(RemoteError::new(
                ErrorCode::ResourceExhausted,
                format!(
                    "stdin queue cannot accept {} bytes; {} of {} bytes are already pending",
                    bytes.len(),
                    self.pending.len(),
                    self.capacity
                ),
            ));
        }
        self.pending.extend(bytes);
        Ok(())
    }

    pub(super) fn request_close(&mut self) -> Result<(), RemoteError> {
        if self.writer.is_none() {
            return Ok(());
        }
        self.close_requested = true;
        self.flush_pending().map_err(stdin_io_error)
    }

    pub(super) fn flush_pending(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            let Some(writer) = self.writer.as_mut() else {
                self.pending.clear();
                return Ok(());
            };
            let (first, _) = self.pending.as_slices();
            match writer.write(first) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "child stdin accepted zero bytes",
                    ));
                }
                Ok(written) => {
                    self.pending.drain(..written);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }

        if self.close_requested {
            if self.is_pty {
                let Some(writer) = self.writer.as_mut() else {
                    return Ok(());
                };
                match writer.write(&[PTY_END_OF_TRANSMISSION]) {
                    Ok(1) => {}
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "PTY stdin accepted zero bytes",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
            self.writer = None;
        }
        Ok(())
    }
}

fn stdin_io_error(error: io::Error) -> RemoteError {
    RemoteError::new(
        ErrorCode::Io,
        format!("failed to write execution stdin: {error}"),
    )
}

pub(super) fn decode_stdin_chunk(
    data_base64: &str,
    max_stream_chunk_size: u32,
) -> Result<Vec<u8>, RemoteError> {
    let max_size = usize::try_from(max_stream_chunk_size).unwrap_or(usize::MAX);
    let maximum_encoded_size = max_size
        .saturating_add(2)
        .checked_div(3)
        .unwrap_or(usize::MAX)
        .saturating_mul(4);
    if data_base64.len() > maximum_encoded_size {
        return Err(RemoteError::new(
            ErrorCode::ResourceExhausted,
            format!("stdin chunk exceeds the {max_stream_chunk_size}-byte limit"),
        ));
    }
    let bytes = BASE64.decode(data_base64).map_err(|_| {
        RemoteError::new(
            ErrorCode::InvalidRequest,
            "stdin data_base64 is not valid canonical base64",
        )
    })?;
    if bytes.len() > max_size {
        return Err(RemoteError::new(
            ErrorCode::ResourceExhausted,
            format!("stdin chunk exceeds the {max_stream_chunk_size}-byte limit"),
        ));
    }
    if BASE64.encode(&bytes) != data_base64 {
        return Err(RemoteError::new(
            ErrorCode::InvalidRequest,
            "stdin data_base64 is not valid canonical base64",
        ));
    }
    Ok(bytes)
}

#[derive(Debug)]
pub(super) struct OutputPipe<R> {
    reader: R,
    sequence: u64,
    sent: u64,
    limit: u64,
    pty_eio_is_eof: bool,
}

impl<R> OutputPipe<R> {
    pub(super) fn new(reader: R, limit: u64) -> Self {
        Self {
            reader,
            sequence: 0,
            sent: 0,
            limit,
            pty_eio_is_eof: false,
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn pty(reader: R, limit: u64) -> Self {
        Self {
            reader,
            sequence: 0,
            sent: 0,
            limit,
            pty_eio_is_eof: true,
        }
    }
}

pub(super) struct OutputPoll {
    pub(super) event: Option<EventKind>,
    pub(super) fault: bool,
}

pub(super) fn poll_output<R: io::Read>(
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
        Ok(0) => finish_output(output, execution_id, channel, false),
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
        Err(error) if pipe.pty_eio_is_eof && is_linux_pty_eof(&error) => {
            finish_output(output, execution_id, channel, false)
        }
        Err(_) => finish_output(output, execution_id, channel, true),
    }
}

fn finish_output<R>(
    output: &mut Option<OutputPipe<R>>,
    execution_id: &str,
    channel: StreamChannel,
    fault: bool,
) -> OutputPoll {
    let pipe = output
        .take()
        .expect("finish_output is called only for an active output pipe");
    OutputPoll {
        event: Some(stream_event(
            execution_id,
            channel,
            pipe.sequence,
            &[],
            true,
        )),
        fault,
    }
}

#[cfg(target_os = "linux")]
fn is_linux_pty_eof(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

#[cfg(not(target_os = "linux"))]
fn is_linux_pty_eof(_error: &io::Error) -> bool {
    false
}

pub(super) fn close_output<R>(
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

pub(super) fn stream_event(
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
