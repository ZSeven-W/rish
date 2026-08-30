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
pub enum HostEvent {
    Connected,
    ConnectFailed(String),
    Data(Vec<u8>),
    Eof,
}

/// Spawns the host thread for one connection, returning the channel ends
/// the backend keeps.
pub fn spawn_host_thread(remote: SocketAddr) -> (SyncSender<Vec<u8>>, Receiver<HostEvent>) {
    let (guest_tx, guest_rx) = sync_channel::<Vec<u8>>(32);
    let (host_tx, host_rx) = sync_channel::<HostEvent>(32);
    std::thread::spawn(move || host_thread(remote, guest_rx, host_tx));
    (guest_tx, host_rx)
}

/// Connect, then shuttle bytes until the backend drops its channel half,
/// the socket reaches EOF, or the socket errors.
fn host_thread(remote: SocketAddr, guest_rx: Receiver<Vec<u8>>, host_tx: SyncSender<HostEvent>) {
    let mut stream = match TcpStream::connect(remote) {
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
