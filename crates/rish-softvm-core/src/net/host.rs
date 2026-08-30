//! The per-connection host socket thread and the channel protocol it
//! speaks with the backend.
//!
//! One thread runs per outbound connection. The backend sends it guest
//! payload bytes over a bounded channel (a full channel holds the segment
//! and the guest retransmits); it sends host bytes and events back over a
//! second bounded channel (a full channel stops the thread from reading
//! the socket). The thread exits on channel disconnect, socket EOF, or a
//! socket error.
//!
//! Until the remote has produced its first response byte, the thread may
//! redial the same address up to [MAX_DIALS] times: a CDN edge that resets
//! or closes a freshly accepted connection (rate limiting, load shedding,
//! a transient middlebox reset) otherwise killed the guest's fetch
//! outright, because the backend had already answered the guest's SYN and
//! the FIN/RST reached the guest mid-handshake. A redial is invisible to
//! the guest -- its TCP state is terminated in the backend -- and the
//! pre-response guest flight (TLS ClientHello, HTTP request) is replayed
//! verbatim on the next dial. Once a response byte has been forwarded to
//! the guest the connection is no longer redial-eligible: replaying
//! mid-stream would corrupt the guest's view of the transfer.

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

/// How many dial attempts one connection gets before the remote has
/// produced a single response byte. Each attempt is bounded by
/// [CONNECT_TIMEOUT] plus [WRITE_TIMEOUT], so the redial budget never pins
/// the thread longer than the old single-dial worst case times
/// [MAX_DIALS].
const MAX_DIALS: usize = 3;

/// Cap on the guest payload replayed across redials. The pre-response
/// flight (TLS ClientHello, HTTP request) is a few KiB; a connection that
/// sent more than this without a response byte is no longer redial-eligible
/// and a death is reported as EOF, exactly as before.
const MAX_REPLAY_BYTES: usize = 64 * 1024;

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
/// channel half, the socket reaches EOF, or the socket errors. See the
/// module docs for the pre-response redial rule.
fn host_thread(
    remote: SocketAddr,
    guest_rx: Receiver<Vec<u8>>,
    host_tx: SyncSender<HostEvent>,
    connect_timeout: Duration,
) {
    let mut dials = 0_usize;
    // The Connected event is sent once, on the first successful dial: it
    // is what lets the backend answer the guest's SYN. Later dials reuse
    // the already-established guest side and send no event.
    let mut announced = false;
    // Guest payload written to a socket that died before the first
    // response byte: rewritten verbatim on the next dial.
    let mut replay = Vec::new();
    let mut replayable = true;
    loop {
        dials += 1;
        let mut stream = match TcpStream::connect_timeout(&remote, connect_timeout) {
            Ok(stream) => stream,
            Err(error) => {
                if dials < MAX_DIALS {
                    continue;
                }
                let _ = host_tx.send(HostEvent::ConnectFailed(error.to_string()));
                return;
            }
        };
        if !announced {
            let _ = host_tx.send(HostEvent::Connected);
            announced = true;
        }
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .ok();
        stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
        // Replay payload the previous dial never got to deliver.
        if !replay.is_empty() {
            use std::io::Write;
            if stream.write_all(&replay).is_err() {
                if dials < MAX_DIALS {
                    continue;
                }
                let _ = host_tx.send(HostEvent::Eof);
                return;
            }
        }
        let mut buffer = [0_u8; 16 * 1024];
        // True once the remote's response started flowing back to the
        // guest: from then on a socket death is a real EOF, not a reason
        // to redial.
        let mut answered = false;
        loop {
            match guest_rx.try_recv() {
                Ok(data) => {
                    use std::io::Write;
                    if stream.write_all(&data).is_err() {
                        if !answered
                            && replayable
                            && dials < MAX_DIALS
                            && replay.len() + data.len() <= MAX_REPLAY_BYTES
                        {
                            replay.extend_from_slice(&data);
                            break;
                        }
                        let _ = host_tx.send(HostEvent::Eof);
                        return;
                    }
                    if !answered && replayable {
                        if replay.len() + data.len() <= MAX_REPLAY_BYTES {
                            replay.extend_from_slice(&data);
                        } else {
                            replayable = false;
                            replay.clear();
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }
            use std::io::Read;
            match stream.read(&mut buffer) {
                Ok(0) => {
                    if !answered && replayable && dials < MAX_DIALS {
                        break;
                    }
                    let _ = host_tx.send(HostEvent::Eof);
                    return;
                }
                Ok(length) => {
                    answered = true;
                    replay.clear();
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
                    if !answered && replayable && dials < MAX_DIALS {
                        break;
                    }
                    let _ = host_tx.send(HostEvent::Eof);
                    return;
                }
            }
        }
    }
}
