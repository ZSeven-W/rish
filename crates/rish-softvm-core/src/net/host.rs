//! The per-connection host socket thread and the channel protocol it
//! speaks with the backend.
//!
//! One thread runs per outbound connection. The backend sends it guest
//! payload bytes over a bounded channel (a full channel holds the segment
//! and the guest retransmits); it sends host bytes and events back over a
//! second bounded channel (a full channel stops the thread from reading
//! the socket). The thread exits on channel disconnect, socket EOF, or a
//! socket error.

use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::Duration;

/// Events the host thread hands back over the bounded channel.
#[derive(Debug)]
pub enum HostEvent {
    Connected,
    ConnectFailed(String),
    Data(Vec<u8>),
    Eof,
}

/// Bound on the blocking connect. A host that never answers the SYN must
/// not pin a thread forever: a guest RST or a device reset cannot interrupt
/// a connect that is already in flight, so the timeout is what reclaims the
/// thread and its socket on teardown.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on a blocking socket write. A host that stops reading deadlocks
/// the thread in write_all once the socket buffer fills; the timeout turns
/// that into a clean EOF (and with the read timeout and the channel
/// disconnect, every teardown path exits promptly).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawns the host thread for one connection with the production connect
/// bound, returning the channel ends the backend keeps.
pub fn spawn_host_thread(remote: SocketAddr) -> (SyncSender<Vec<u8>>, Receiver<HostEvent>) {
    spawn_host_thread_with_timeout(remote, CONNECT_TIMEOUT)
}

/// Spawns the host thread with an explicit connect bound (tests use a
/// short one; production uses [CONNECT_TIMEOUT]).
pub fn spawn_host_thread_with_timeout(
    remote: SocketAddr,
    connect_timeout: Duration,
) -> (SyncSender<Vec<u8>>, Receiver<HostEvent>) {
    let (guest_tx, guest_rx) = sync_channel::<Vec<u8>>(32);
    let (host_tx, host_rx) = sync_channel::<HostEvent>(32);
    std::thread::spawn(move || host_thread(remote, guest_rx, host_tx, connect_timeout));
    (guest_tx, host_rx)
}

/// Connect (bounded), then shuttle bytes until the backend drops its
/// channel half, the socket reaches EOF, or the socket errors.
fn host_thread(
    remote: SocketAddr,
    guest_rx: Receiver<Vec<u8>>,
    host_tx: SyncSender<HostEvent>,
    connect_timeout: Duration,
) {
    let mut stream = match TcpStream::connect_timeout(&remote, connect_timeout) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = host_tx.send(HostEvent::ConnectFailed(error.to_string()));
            return;
        }
    };
    let _ = host_tx.send(HostEvent::Connected);
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok();
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match guest_rx.try_recv() {
            Ok(data) => {
                use std::io::Write;
                if stream.write_all(&data).is_err() {
                    let _ = host_tx.send(HostEvent::Eof);
                    return;
                }
            }
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        use std::io::Read;
        match stream.read(&mut buffer) {
            Ok(0) => {
                let _ = host_tx.send(HostEvent::Eof);
                return;
            }
            Ok(length) => {
                if host_tx
                    .send(HostEvent::Data(buffer[..length].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut,
                ) => {}
            Err(_) => {
                let _ = host_tx.send(HostEvent::Eof);
                return;
            }
        }
    }
}
