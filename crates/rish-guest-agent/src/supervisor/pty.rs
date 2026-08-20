#[cfg(target_os = "linux")]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::fd::OwnedFd;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use rish_guest_protocol::{ErrorCode, RemoteError};
    use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
    use rustix::termios::{Winsize, tcsetpgrp, tcsetwinsize};

    use crate::supervisor::set_nonblocking;

    const DEFAULT_ROWS: u16 = 24;
    const DEFAULT_COLUMNS: u16 = 80;

    #[derive(Debug)]
    pub(in crate::supervisor) struct PreparedPty {
        pub(in crate::supervisor) reader: File,
        pub(in crate::supervisor) writer: Option<File>,
        pub(in crate::supervisor) control: PtyControl,
    }

    #[derive(Debug)]
    pub(in crate::supervisor) struct PtyControl(File);

    impl PtyControl {
        pub(in crate::supervisor) fn resize(&self, rows: u16, columns: u16) -> io::Result<()> {
            tcsetwinsize(
                &self.0,
                Winsize {
                    ws_row: rows,
                    ws_col: columns,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                },
            )
            .map_err(io::Error::from)
        }
    }

    pub(in crate::supervisor) fn prepare(
        command: &mut Command,
        attach_stdin: bool,
    ) -> Result<PreparedPty, RemoteError> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)
            .map_err(|error| pty_io_error("open /dev/ptmx", error))?;
        grantpt(&master).map_err(|error| pty_io_error("grant PTY slave", error))?;
        unlockpt(&master).map_err(|error| pty_io_error("unlock PTY slave", error))?;
        let slave = ioctl_tiocgptpeer(
            &master,
            OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
        )
        .map_err(|error| pty_io_error("open PTY slave", error))?;

        tcsetwinsize(
            &master,
            Winsize {
                ws_row: DEFAULT_ROWS,
                ws_col: DEFAULT_COLUMNS,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .map_err(|error| pty_io_error("set initial PTY size", error))?;

        let master = File::from(master);
        set_nonblocking(&master).map_err(|error| pty_io_error("configure PTY master", error))?;
        let reader = master
            .try_clone()
            .map_err(|error| pty_io_error("clone PTY reader", error))?;
        let writer = attach_stdin
            .then(|| master.try_clone())
            .transpose()
            .map_err(|error| pty_io_error("clone PTY writer", error))?;
        let control = PtyControl(master);

        configure_child_stdio(command, slave, attach_stdin)?;
        Ok(PreparedPty {
            reader,
            writer,
            control,
        })
    }

    fn configure_child_stdio(
        command: &mut Command,
        slave: OwnedFd,
        attach_stdin: bool,
    ) -> Result<(), RemoteError> {
        let slave = File::from(slave);
        command.stdin(if attach_stdin {
            Stdio::from(
                slave
                    .try_clone()
                    .map_err(|error| pty_io_error("clone PTY stdin", error))?,
            )
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::from(
            slave
                .try_clone()
                .map_err(|error| pty_io_error("clone PTY stdout", error))?,
        ));
        command.stderr(Stdio::from(
            slave
                .try_clone()
                .map_err(|error| pty_io_error("clone PTY stderr", error))?,
        ));

        // SAFETY: the closure runs after fork and before exec. It performs only
        // direct session/terminal syscalls on an already-open descriptor.
        unsafe {
            command.pre_exec(move || {
                let process_group = rustix::process::setsid().map_err(io::Error::from)?;
                rustix::process::ioctl_tiocsctty(&slave).map_err(io::Error::from)?;
                tcsetpgrp(&slave, process_group).map_err(io::Error::from)
            });
        }
        Ok(())
    }

    fn pty_io_error(action: &str, error: impl std::fmt::Display) -> RemoteError {
        RemoteError::new(ErrorCode::Io, format!("failed to {action}: {error}"))
    }
}

#[cfg(target_os = "linux")]
pub(super) use platform::{PreparedPty, PtyControl, prepare};
