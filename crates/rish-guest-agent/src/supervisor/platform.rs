use std::io;
use std::process::{Command, ExitStatus};

use rish_guest_protocol::{ErrorCode, RemoteError};

use super::SIGKILL_NUMBER;

#[cfg(unix)]
pub(super) fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
pub(super) fn set_nonblocking(stream: &impl std::os::fd::AsFd) -> io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

    let flags = fcntl_getfl(stream).map_err(io::Error::from)?;
    fcntl_setfl(stream, flags | OFlags::NONBLOCK).map_err(io::Error::from)
}

#[cfg(not(unix))]
pub(super) fn set_nonblocking(_stream: &impl Sized) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native guest execution requires Unix nonblocking pipes",
    ))
}

#[cfg(unix)]
pub(super) fn validate_cancel_signal(signal: Option<i32>) -> Result<i32, RemoteError> {
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
pub(super) fn validate_cancel_signal(signal: Option<i32>) -> Result<i32, RemoteError> {
    match signal.unwrap_or(SIGKILL_NUMBER) {
        SIGKILL_NUMBER => Ok(SIGKILL_NUMBER),
        _ => Err(RemoteError::new(
            ErrorCode::UnsupportedOperation,
            "only forced cancellation is supported on this guest platform",
        )),
    }
}

#[cfg(unix)]
pub(super) fn signal_process_group(pid: u32, signal: i32) -> io::Result<()> {
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
pub(super) fn signal_process_group(_pid: u32, _signal: i32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process groups are unavailable",
    ))
}

#[cfg(unix)]
pub(super) fn platform_signal(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
pub(super) fn platform_signal(_status: &ExitStatus) -> Option<i32> {
    None
}
