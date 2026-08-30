//! TCP termination for the network backend.
//!
//! The backend proxies outbound guest connections: the guest's SYN opens a
//! host socket (on a dedicated thread), the guest's data flows into it, and
//! the host's data flows back as TCP segments carrying the remote host's
//! address. Sequence numbers, acknowledgements, windows, MSS clamping, and
//! the FIN handshake are translated between the guest and the host socket.
//!
//! What the state machine deliberately does NOT do (see the honest
//! inventory in the module docs of crate::net):
//!
//! - No out-of-order reassembly: a segment ahead of the expected sequence
//!   is ignored and the sender's retransmission closes the gap. Duplicate
//!   segments are re-acknowledged, not replayed.
//! - No delayed ACK, no fast retransmit, no SACK, no window scaling
//!   negotiation (the backend never sends the WS option, which disables
//!   scaling per RFC 7323), no zero-window probing, no PMTU discovery,
//!   no PAWS, no congestion control: the host kernel's TCP stack does all
//!   of that on the real network; our side is a store-and-forward bridge
//!   with one retransmission timer.
//! - One retransmission timer per connection (750 ms, 8 tries) resends the
//!   oldest unacknowledged segment; after the retry budget the connection
//!   is reset.
//! - Inbound (host-to-guest) connections are not supported: there is no
//!   listener. Only outbound connections the guest initiates exist.
//! - No half-close data beyond delivery: once the guest FINs, its data is
//!   no longer accepted, but host data keeps flowing until the host
//!   closes, then the backend FINs back.
//!
//! Backpressure: guest data crosses a bounded channel onto the host thread
//! (full channel holds the segment and the guest retransmits); host data
//! crosses a bounded channel off the host thread (full channel stops the
//! thread from reading the socket), and the interpreter thread never
//! blocks on either.

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

use super::host::{HostEvent, spawn_host_thread};
use super::ipv4::{Ipv4Packet, next_identification, wrap_tcp};
use super::tcp_segment::{
    FLAG_ACK, FLAG_FIN, FLAG_RST, FLAG_SYN, OUR_MSS, Segment, build_segment, parse_mss,
    parse_segment, segment_checksum_ok,
};
use super::{NetConfig, NetCounters, TCP_PENDING_CAP};

/// Window the backend advertises for guest-to-host data. The guest's
/// segments are drained into the host socket promptly, so a fixed generous
/// window is honest enough.
const OUR_WINDOW: u16 = 0xFFFF;

/// Retransmission timeout and retry budget.
const RETRANSMIT_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_RETRANSMITS: u32 = 8;

/// How long a half-closed connection (our FIN acknowledged, the guest's
/// FIN never seen) is kept before it is dropped.
const LINGER_TIMEOUT: Duration = Duration::from_secs(30);

/// One connection the backend terminates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnKey {
    /// Guest-side source port.
    pub local_port: u16,
    /// Remote host the guest dialed.
    pub remote_addr: [u8; 4],
    pub remote_port: u16,
}

/// One transmitted, not yet acknowledged segment (payload or SYN-ACK).
struct OutSeg {
    seq: u32,
    /// Payload bytes (empty for the SYN-ACK), kept verbatim for
    /// retransmission.
    bytes: Vec<u8>,
    last_sent: Instant,
}

struct TcpConn {
    key: ConnKey,
    /// True once the SYN-ACK is acknowledged: data flows.
    established: bool,
    /// Next sequence number expected from the guest.
    guest_next: u32,
    /// Latest window the guest advertised.
    guest_window: u32,
    /// Guest's MSS, clamped to ours.
    guest_mss: u16,
    our_isn: u32,
    /// Next sequence number we will assign.
    our_next: u32,
    /// First unacknowledged sequence number of our stream.
    our_una: u32,
    /// Host bytes not yet sent to the guest.
    pending: VecDeque<u8>,
    /// Segments sent, awaiting the guest's ACK.
    unacked: VecDeque<OutSeg>,
    retransmit_deadline: Option<Instant>,
    retransmits: u32,
    /// Guest data the host channel could not take yet (retried on poll).
    held_guest: Option<Vec<u8>>,
    guest_tx: SyncSender<Vec<u8>>,
    host_rx: Receiver<HostEvent>,
    fin_sent: bool,
    fin_acked: bool,
    fin_seq: u32,
    fin_pending: bool,
    guest_fin: bool,
    guest_fin_acked: bool,
    host_eof: bool,
    linger_deadline: Option<Instant>,
    closed: bool,
}

/// Sequence-number helpers over the 32-bit wrapping space (RFC 793).
fn seq_gt(a: u32, b: u32) -> bool {
    ((a.wrapping_sub(b)) as i32) > 0
}
fn seq_ge(a: u32, b: u32) -> bool {
    ((a.wrapping_sub(b)) as i32) >= 0
}
fn seq_lt(a: u32, b: u32) -> bool {
    ((a.wrapping_sub(b)) as i32) < 0
}

/// The connection table and the next initial sequence number.
pub struct TcpState {
    connections: HashMap<ConnKey, TcpConn>,
    next_isn: u32,
}

impl TcpState {
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.subsec_nanos())
            .unwrap_or(0x1234);
        Self {
            connections: HashMap::new(),
            next_isn: seed ^ 0x9E37_79B9,
        }
    }

    /// Handles one TCP segment the guest sent. A SYN opens a connection
    /// (spawning the host thread); everything else is matched to an
    /// existing connection, unknown connections get a RST.
    #[allow(clippy::too_many_arguments)]
    pub fn handle(
        &mut self,
        packet: Ipv4Packet<'_>,
        config: &NetConfig,
        ip_id: &mut u16,
        counters: &mut NetCounters,
        emit: &mut dyn FnMut(Vec<u8>),
    ) {
        let data = packet.payload;
        let Some(segment) = parse_segment(data) else {
            counters.dropped_bad_packet = counters.dropped_bad_packet.saturating_add(1);
            return;
        };
        if !segment_checksum_ok(&packet, data) {
            counters.dropped_bad_checksum = counters.dropped_bad_checksum.saturating_add(1);
            return;
        }
        if packet.src != config.guest_ip {
            // Only the configured guest may originate connections.
            counters.dropped_not_for_us = counters.dropped_not_for_us.saturating_add(1);
            return;
        }
        let key = ConnKey {
            local_port: segment.sport,
            remote_addr: packet.dst.octets(),
            remote_port: segment.dport,
        };
        if segment.flags & FLAG_SYN != 0 && segment.flags & FLAG_ACK == 0 {
            self.open(key, segment, counters);
            return;
        }
        if !self.connections.contains_key(&key) {
            if segment.flags & FLAG_RST == 0 {
                // Standard reset for a connection we do not know.
                let rst = build_segment(
                    segment.dport,
                    segment.sport,
                    segment.ack,
                    0,
                    FLAG_RST | FLAG_ACK,
                    0,
                    &[],
                    &[],
                );
                let remote = Ipv4Addr::from(key.remote_addr);
                emit(wrap_tcp(
                    remote,
                    config.guest_ip,
                    &rst,
                    next_identification(ip_id),
                ));
                counters.tcp_resets = counters.tcp_resets.saturating_add(1);
            }
            return;
        }
        let conn = self.connections.get_mut(&key).expect("checked above");
        process_segment(conn, segment, config, ip_id, counters, emit);
    }

    /// Opens a connection on the guest's SYN: spawns the host thread and
    /// records the guest's sequence space. The SYN-ACK is sent once the
    /// host connect completes (poll).
    fn open(&mut self, key: ConnKey, segment: Segment<'_>, counters: &mut NetCounters) {
        // A retransmitted SYN for a live connection is ignored; a SYN on a
        // lingering key replaces it.
        if self.connections.contains_key(&key) {
            return;
        }
        let (guest_tx, host_rx) = spawn_host_thread(SocketAddr::new(
            Ipv4Addr::from(key.remote_addr).into(),
            key.remote_port,
        ));
        let guest_mss = parse_mss(segment.options).unwrap_or(536).clamp(1, OUR_MSS);
        let our_isn = self.next_isn;
        self.next_isn = self.next_isn.wrapping_add(0x2F6B_35C5);
        self.connections.insert(
            key,
            TcpConn {
                key,
                established: false,
                guest_next: segment.seq.wrapping_add(1),
                guest_window: u32::from(segment.window),
                guest_mss,
                our_isn,
                our_next: our_isn.wrapping_add(1),
                // The SYN-ACK occupies sequence our_isn, so the first
                // unacknowledged sequence number starts AT our_isn; the
                // guest's acknowledgement of the handshake (our_isn + 1)
                // then advances past it and pops the SYN-ACK from the
                // retransmission queue.
                our_una: our_isn,
                pending: VecDeque::new(),
                unacked: VecDeque::new(),
                retransmit_deadline: None,
                retransmits: 0,
                held_guest: None,
                guest_tx,
                host_rx,
                fin_sent: false,
                fin_acked: false,
                fin_seq: 0,
                fin_pending: false,
                guest_fin: false,
                guest_fin_acked: false,
                host_eof: false,
                linger_deadline: None,
                closed: false,
            },
        );
        counters.tcp_opened = counters.tcp_opened.saturating_add(1);
    }

    /// Advances the backend: host events, held guest data, windowed sends,
    /// retransmission, FIN handshake, and connection teardown.
    #[allow(clippy::too_many_arguments)]
    pub fn poll(
        &mut self,
        now: Instant,
        config: &NetConfig,
        ip_id: &mut u16,
        counters: &mut NetCounters,
        emit: &mut dyn FnMut(Vec<u8>),
    ) {
        let mut closed: Vec<ConnKey> = Vec::new();
        for conn in self.connections.values_mut() {
            drain_host_events(conn, now, config, ip_id, counters, emit);
            if !conn.closed {
                retry_held(conn, config, ip_id, emit);
            }
            if !conn.closed {
                send_pending(conn, now, config, ip_id, emit);
            }
            if !conn.closed {
                retransmit(conn, now, config, ip_id, counters, emit);
            }
            if !conn.closed {
                maybe_send_fin(conn, now, config, ip_id, emit);
            }
            if conn.closed {
                closed.push(conn.key);
                continue;
            }
            // Teardown once both FINs are acknowledged, or after lingering.
            if conn.fin_sent && conn.fin_acked && conn.guest_fin && conn.guest_fin_acked {
                conn.closed = true;
                closed.push(conn.key);
            } else if conn.fin_acked && !conn.guest_fin {
                let deadline = *conn
                    .linger_deadline
                    .get_or_insert_with(|| now + LINGER_TIMEOUT);
                if now >= deadline {
                    conn.closed = true;
                    closed.push(conn.key);
                }
            }
        }
        for key in closed {
            self.connections.remove(&key);
            counters.tcp_closed = counters.tcp_closed.saturating_add(1);
        }
    }
}

/// Applies one non-SYN segment to its connection.
#[allow(clippy::too_many_arguments)]
fn process_segment(
    conn: &mut TcpConn,
    segment: Segment<'_>,
    config: &NetConfig,
    ip_id: &mut u16,
    counters: &mut NetCounters,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    // RST in the receive window closes the connection immediately.
    if segment.flags & FLAG_RST != 0 {
        if segment.seq == conn.guest_next || segment.seq == conn.guest_next.wrapping_sub(1) {
            conn.closed = true;
            counters.tcp_resets = counters.tcp_resets.saturating_add(1);
        }
        return;
    }
    if segment.flags & FLAG_ACK != 0 {
        conn.guest_window = u32::from(segment.window);
        // Clamp the acknowledgement to what we actually sent.
        let mut ack = segment.ack;
        if seq_gt(ack, conn.our_next) {
            ack = conn.our_next;
        }
        if seq_gt(ack, conn.our_una) {
            while let Some(front) = conn.unacked.front() {
                let end = front.seq.wrapping_add(front.bytes.len() as u32);
                if seq_ge(ack, end) {
                    conn.unacked.pop_front();
                } else {
                    break;
                }
            }
            conn.our_una = ack;
            conn.retransmits = 0;
            if conn.unacked.is_empty() {
                conn.retransmit_deadline = None;
            }
        }
        if !conn.established && ack == conn.our_isn.wrapping_add(1) {
            conn.established = true;
        }
        if conn.fin_sent && seq_ge(ack, conn.our_next) {
            conn.fin_acked = true;
        }
    }
    let mut ack_needed = false;
    if !segment.payload.is_empty() {
        if segment.seq == conn.guest_next && conn.established && !conn.guest_fin {
            // Accept and forward the payload; a full host channel holds
            // the segment without acknowledging it, so the guest's own
            // retransmission timer retries.
            match conn.guest_tx.try_send(segment.payload.to_vec()) {
                Ok(()) => {
                    conn.guest_next = conn.guest_next.wrapping_add(segment.payload.len() as u32);
                    ack_needed = true;
                }
                Err(TrySendError::Full(_)) => {
                    conn.held_guest = Some(segment.payload.to_vec());
                }
                Err(TrySendError::Disconnected(_)) => {
                    conn.closed = true;
                    counters.tcp_resets = counters.tcp_resets.saturating_add(1);
                    return;
                }
            }
        } else if seq_lt(segment.seq, conn.guest_next) {
            // Duplicate data: re-acknowledge so the guest stops.
            ack_needed = true;
        }
        // Segments ahead of the expected sequence are ignored: no
        // out-of-order buffering; the guest retransmits.
    }
    if segment.flags & FLAG_FIN != 0 {
        if segment.seq == conn.guest_next {
            conn.guest_next = conn.guest_next.wrapping_add(1);
            conn.guest_fin = true;
            ack_needed = true;
        } else if seq_lt(segment.seq, conn.guest_next) {
            // Duplicate FIN: re-acknowledge.
            ack_needed = true;
        }
    }
    if ack_needed {
        if conn.guest_fin && !conn.guest_fin_acked {
            conn.guest_fin_acked = true;
        }
        send_ack(conn, config, ip_id, emit);
    }
    if conn.guest_fin && conn.guest_fin_acked && !conn.fin_sent {
        conn.fin_pending = true;
    }
}

fn send_ack(conn: &TcpConn, config: &NetConfig, ip_id: &mut u16, emit: &mut dyn FnMut(Vec<u8>)) {
    let segment = build_segment(
        conn.key.remote_port,
        conn.key.local_port,
        conn.our_next,
        conn.guest_next,
        FLAG_ACK,
        OUR_WINDOW,
        &[],
        &[],
    );
    let remote = Ipv4Addr::from(conn.key.remote_addr);
    emit(wrap_tcp(
        remote,
        config.guest_ip,
        &segment,
        next_identification(ip_id),
    ));
}

#[allow(clippy::too_many_arguments)]
fn drain_host_events(
    conn: &mut TcpConn,
    now: Instant,
    config: &NetConfig,
    ip_id: &mut u16,
    counters: &mut NetCounters,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    loop {
        let event = match conn.host_rx.try_recv() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                // The host thread died without an event: treat as EOF.
                conn.host_eof = true;
                conn.fin_pending = true;
                return;
            }
        };
        match event {
            HostEvent::Connected => {
                if !conn.established && conn.unacked.is_empty() {
                    send_syn_ack(conn, now, config, ip_id, emit);
                }
            }
            HostEvent::ConnectFailed(_message) => {
                let segment = build_segment(
                    conn.key.remote_port,
                    conn.key.local_port,
                    conn.our_isn,
                    conn.guest_next,
                    FLAG_RST | FLAG_ACK,
                    0,
                    &[],
                    &[],
                );
                let remote = Ipv4Addr::from(conn.key.remote_addr);
                emit(wrap_tcp(
                    remote,
                    config.guest_ip,
                    &segment,
                    next_identification(ip_id),
                ));
                conn.closed = true;
                counters.tcp_resets = counters.tcp_resets.saturating_add(1);
            }
            HostEvent::Data(bytes) => {
                if conn.pending.len() + bytes.len() > TCP_PENDING_CAP {
                    // Fail closed on our own buffer limit: reset the
                    // connection rather than grow without bound.
                    conn.closed = true;
                    counters.tcp_resets = counters.tcp_resets.saturating_add(1);
                    return;
                }
                conn.pending.extend(bytes);
            }
            HostEvent::Eof => {
                conn.host_eof = true;
                conn.fin_pending = true;
            }
        }
    }
}

fn send_syn_ack(
    conn: &mut TcpConn,
    now: Instant,
    config: &NetConfig,
    ip_id: &mut u16,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    let segment = build_segment(
        conn.key.remote_port,
        conn.key.local_port,
        conn.our_isn,
        conn.guest_next,
        FLAG_SYN | FLAG_ACK,
        OUR_WINDOW,
        &[2, 4, (OUR_MSS >> 8) as u8, OUR_MSS as u8],
        &[],
    );
    let remote = Ipv4Addr::from(conn.key.remote_addr);
    emit(wrap_tcp(
        remote,
        config.guest_ip,
        &segment,
        next_identification(ip_id),
    ));
    conn.unacked.push_back(OutSeg {
        seq: conn.our_isn,
        bytes: Vec::new(),
        last_sent: now,
    });
    conn.retransmit_deadline = Some(now + RETRANSMIT_TIMEOUT);
}

fn retry_held(
    conn: &mut TcpConn,
    config: &NetConfig,
    ip_id: &mut u16,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    let Some(held) = conn.held_guest.take() else {
        return;
    };
    match conn.guest_tx.try_send(held.clone()) {
        Ok(()) => {
            conn.guest_next = conn.guest_next.wrapping_add(held.len() as u32);
            send_ack(conn, config, ip_id, emit);
        }
        Err(TrySendError::Full(_)) => conn.held_guest = Some(held),
        Err(TrySendError::Disconnected(_)) => conn.closed = true,
    }
}

fn send_pending(
    conn: &mut TcpConn,
    now: Instant,
    config: &NetConfig,
    ip_id: &mut u16,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    const MAX_UNACKED_SEGMENTS: usize = 64;
    while !conn.pending.is_empty() && conn.unacked.len() < MAX_UNACKED_SEGMENTS {
        let in_flight = u64::from(conn.our_next.wrapping_sub(conn.our_una));
        let window = u64::from(conn.guest_window);
        if in_flight >= window {
            break;
        }
        let take = conn
            .pending
            .len()
            .min(usize::from(conn.guest_mss))
            .min((window - in_flight) as usize);
        if take == 0 {
            break;
        }
        let bytes: Vec<u8> = conn.pending.drain(..take).collect();
        let segment = build_segment(
            conn.key.remote_port,
            conn.key.local_port,
            conn.our_next,
            conn.guest_next,
            FLAG_ACK,
            OUR_WINDOW,
            &[],
            &bytes,
        );
        let remote = Ipv4Addr::from(conn.key.remote_addr);
        emit(wrap_tcp(
            remote,
            config.guest_ip,
            &segment,
            next_identification(ip_id),
        ));
        conn.unacked.push_back(OutSeg {
            seq: conn.our_next,
            bytes: bytes.clone(),
            last_sent: now,
        });
        conn.our_next = conn.our_next.wrapping_add(bytes.len() as u32);
        conn.retransmit_deadline = Some(now + RETRANSMIT_TIMEOUT);
    }
}

#[allow(clippy::too_many_arguments)]
fn retransmit(
    conn: &mut TcpConn,
    now: Instant,
    config: &NetConfig,
    ip_id: &mut u16,
    counters: &mut NetCounters,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    let Some(deadline) = conn.retransmit_deadline else {
        return;
    };
    if now < deadline {
        return;
    }
    conn.retransmits = conn.retransmits.saturating_add(1);
    if conn.retransmits > MAX_RETRANSMITS {
        conn.closed = true;
        counters.tcp_resets = counters.tcp_resets.saturating_add(1);
        return;
    }
    let remote = Ipv4Addr::from(conn.key.remote_addr);
    if !conn.unacked.is_empty() {
        // Resend every unacknowledged segment (bounded by the 64-segment
        // cap): under burst loss, resending only the oldest would cost
        // one retransmission timeout per lost segment and stall the
        // connection long enough for the remote to give up. Sequence
        // numbers make the resent data idempotent for the guest.
        for front in conn.unacked.iter_mut() {
            let segment = build_segment(
                conn.key.remote_port,
                conn.key.local_port,
                front.seq,
                conn.guest_next,
                if front.seq == conn.our_isn {
                    FLAG_SYN | FLAG_ACK
                } else {
                    FLAG_ACK
                },
                OUR_WINDOW,
                if front.seq == conn.our_isn {
                    &[2, 4, (OUR_MSS >> 8) as u8, OUR_MSS as u8]
                } else {
                    &[]
                },
                &front.bytes,
            );
            emit(wrap_tcp(
                remote,
                config.guest_ip,
                &segment,
                next_identification(ip_id),
            ));
            front.last_sent = now;
        }
    } else if conn.fin_sent && !conn.fin_acked {
        let segment = build_segment(
            conn.key.remote_port,
            conn.key.local_port,
            conn.fin_seq,
            conn.guest_next,
            FLAG_FIN | FLAG_ACK,
            OUR_WINDOW,
            &[],
            &[],
        );
        emit(wrap_tcp(
            remote,
            config.guest_ip,
            &segment,
            next_identification(ip_id),
        ));
    } else {
        conn.retransmit_deadline = None;
        return;
    }
    conn.retransmit_deadline = Some(now + RETRANSMIT_TIMEOUT);
    counters.tcp_retransmits = counters.tcp_retransmits.saturating_add(1);
}

fn maybe_send_fin(
    conn: &mut TcpConn,
    now: Instant,
    config: &NetConfig,
    ip_id: &mut u16,
    emit: &mut dyn FnMut(Vec<u8>),
) {
    if conn.fin_sent || !conn.fin_pending {
        return;
    }
    if !conn.pending.is_empty() || !conn.unacked.is_empty() {
        // Wait for our data to be delivered and acknowledged first.
        return;
    }
    let segment = build_segment(
        conn.key.remote_port,
        conn.key.local_port,
        conn.our_next,
        conn.guest_next,
        FLAG_FIN | FLAG_ACK,
        OUR_WINDOW,
        &[],
        &[],
    );
    let remote = Ipv4Addr::from(conn.key.remote_addr);
    emit(wrap_tcp(
        remote,
        config.guest_ip,
        &segment,
        next_identification(ip_id),
    ));
    conn.fin_seq = conn.our_next;
    conn.our_next = conn.our_next.wrapping_add(1);
    conn.fin_sent = true;
    conn.retransmit_deadline = Some(now + RETRANSMIT_TIMEOUT);
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;
