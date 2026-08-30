//! UDP handling: DNS forwarding to the host resolver, and nothing else.
//!
//! The backend only terminates UDP on gateway:53 (DNS). Queries are
//! forwarded verbatim to the host's nameserver with the transaction id
//! rewritten so concurrent guest queries cannot collide; answers are mapped
//! back and relayed with recomputed checksums. Queries that time out are
//! dropped. Resolver datagrams shorter than the 12-byte DNS header are
//! dropped without touching the pending table; the pending table is capped
//! (MAX_PENDING_QUERIES) and the id allocator fails closed when the id
//! space is exhausted, so neither a hostile guest nor a hostile resolver
//! can wedge or exhaust the forwarder. No other UDP service exists: packets
//! to any other port are dropped and counted, and no ICMP port-unreachable
//! is generated.
//!
//! Truncated answers (TC bit) are relayed as-is; the guest resolver may
//! then retry over TCP, which this backend does not terminate (see the tcp
//! module). Typical A/AAAA answers fit well below the 512-byte limit.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use super::ipv4::{self, Ipv4Packet, next_identification, wrap_udp};
use super::{DNS_QUERY_TIMEOUT, NetConfig, NetCounters, host_nameserver};

/// One forwarded guest query awaiting the host resolver's answer.
struct PendingQuery {
    guest_id: u16,
    guest_port: u16,
    created: Instant,
}

/// Smallest datagram the forwarder accepts as a DNS answer: the fixed
/// 12-byte DNS header. Shorter datagrams from the resolver are dropped and
/// counted, never matched against the pending table (the transaction id
/// must come from bytes that actually arrived, not from stale buffer
/// contents).
const DNS_HEADER_BYTES: usize = 12;

/// Maximum unanswered queries kept at once. Queries past the cap are
/// dropped and counted instead of forwarded, so a hostile or broken guest
/// can never exhaust the id space or the host resolver.
const MAX_PENDING_QUERIES: usize = 1024;

/// The DNS forwarder: one host UDP socket and an id-rewrite table.
pub struct UdpState {
    socket: UdpSocket,
    upstream: Option<SocketAddr>,
    pending: HashMap<u16, PendingQuery>,
    next_id: u16,
}

impl UdpState {
    pub fn new() -> std::io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            upstream: host_nameserver(),
            pending: HashMap::new(),
            next_id: 0,
        })
    }

    /// Handles one UDP datagram the guest sent. Only DNS queries to the
    /// gateway are serviced; everything else is dropped with a counter.
    pub fn handle(
        &mut self,
        packet: Ipv4Packet<'_>,
        config: &NetConfig,
        counters: &mut NetCounters,
    ) {
        let payload = packet.payload;
        if payload.len() < 8 {
            counters.dropped_bad_packet = counters.dropped_bad_packet.saturating_add(1);
            return;
        }
        let src_port = u16::from_be_bytes([payload[0], payload[1]]);
        let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
        if packet.dst != config.gateway_ip || dst_port != 53 {
            counters.dropped_other_udp = counters.dropped_other_udp.saturating_add(1);
            return;
        }
        // Verify the UDP checksum (zero means "not computed", which
        // some stacks send; accept it).
        let mut zeroed = payload.to_vec();
        zeroed[6..8].copy_from_slice(&[0, 0]);
        let expected =
            ipv4::transport_checksum(packet.src, packet.dst, ipv4::PROTOCOL_UDP, &zeroed);
        let carried = u16::from_be_bytes([payload[6], payload[7]]);
        if carried != 0 && carried != expected {
            counters.dropped_bad_checksum = counters.dropped_bad_checksum.saturating_add(1);
            return;
        }
        // The UDP length field must agree with the datagram the IP layer
        // actually delivered; a lying length fails closed before any field
        // is trusted.
        let udp_len = u16::from_be_bytes([payload[4], payload[5]]);
        if usize::from(udp_len) != payload.len() {
            counters.dropped_bad_packet = counters.dropped_bad_packet.saturating_add(1);
            return;
        }
        let query = &payload[8..];
        let Some(upstream) = self.upstream else {
            // No host resolver: DNS is unavailable and the guest's own
            // resolver timeout will surface it.
            counters.dns_drops = counters.dns_drops.saturating_add(1);
            return;
        };
        if query.len() < 12 {
            counters.dropped_bad_packet = counters.dropped_bad_packet.saturating_add(1);
            return;
        }
        // Sweep expired queries before allocating, so ids a dead resolver
        // is holding are freed first -- and the table never exceeds the cap.
        self.expire_pending(Instant::now(), counters);
        if self.pending.len() >= MAX_PENDING_QUERIES {
            counters.dns_drops = counters.dns_drops.saturating_add(1);
            return;
        }
        // Rewrite the transaction id so every outstanding guest query has a
        // unique upstream id.
        let guest_id = u16::from_be_bytes([query[0], query[1]]);
        let Some(upstream_id) = self.allocate_id() else {
            // Every id is held by a live query: fail closed and count it
            // instead of spinning the interpreter thread forever.
            counters.dns_drops = counters.dns_drops.saturating_add(1);
            return;
        };
        let mut forwarded = query.to_vec();
        forwarded[0..2].copy_from_slice(&upstream_id.to_be_bytes());
        if self.socket.send_to(&forwarded, upstream).is_err() {
            self.pending.remove(&upstream_id);
            counters.dns_drops = counters.dns_drops.saturating_add(1);
            return;
        }
        self.pending.insert(
            upstream_id,
            PendingQuery {
                guest_id,
                guest_port: src_port,
                created: Instant::now(),
            },
        );
        counters.dns_queries = counters.dns_queries.saturating_add(1);
    }

    /// Tears down every pending query and rewinds the id cursor. Called on
    /// device reset: queries from the previous driver session must not be
    /// answered into the next one.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.next_id = 0;
    }

    /// Collects resolver answers and relays them to the guest, and expires
    /// queries the resolver never answered.
    #[allow(clippy::too_many_arguments)]
    pub fn poll(
        &mut self,
        now: Instant,
        config: &NetConfig,
        ip_id: &mut u16,
        counters: &mut NetCounters,
        emit: &mut dyn FnMut(Vec<u8>),
    ) {
        let mut buffer = [0_u8; 4096];
        loop {
            let (length, from) = match self.socket.recv_from(&mut buffer) {
                Ok((length, from)) => (length, from),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            };
            if Some(from) != self.upstream {
                continue;
            }
            // A DNS answer is at least the 12-byte header. Anything shorter
            // is garbage from the wire (or a hostile resolver) and must
            // never be matched against pending queries or indexed by the
            // transaction id: the id comes from the bytes that actually
            // arrived, not from stale buffer contents.
            if length < DNS_HEADER_BYTES {
                counters.dns_drops = counters.dns_drops.saturating_add(1);
                continue;
            }
            let Some(pending) = self
                .pending
                .remove(&u16::from_be_bytes([buffer[0], buffer[1]]))
            else {
                counters.dns_drops = counters.dns_drops.saturating_add(1);
                continue;
            };
            let mut answer = buffer[..length].to_vec();
            answer[0..2].copy_from_slice(&pending.guest_id.to_be_bytes());
            let packet = wrap_udp(
                config.gateway_ip,
                config.guest_ip,
                53,
                pending.guest_port,
                &answer,
                next_identification(ip_id),
            );
            emit(packet);
            counters.dns_answers = counters.dns_answers.saturating_add(1);
        }
        self.expire_pending(now, counters);
    }

    /// Drops queries the resolver never answered within the timeout.
    fn expire_pending(&mut self, now: Instant, counters: &mut NetCounters) {
        self.pending.retain(|_, pending| {
            let keep = now.duration_since(pending.created) < DNS_QUERY_TIMEOUT;
            if !keep {
                counters.dns_drops = counters.dns_drops.saturating_add(1);
            }
            keep
        });
    }

    /// A transaction id no query currently uses, or None when the whole
    /// 16-bit space is occupied by live queries. The caller sweeps expired
    /// queries first and the pending cap keeps the table far below the id
    /// space, so None means the table is full: the query is dropped and
    /// counted, never spun on (the old code looped forever, and the sweep
    /// runs on the very thread it would have wedged).
    fn allocate_id(&mut self) -> Option<u16> {
        let start = self.next_id;
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if !self.pending.contains_key(&id) {
                return Some(id);
            }
            if self.next_id == start {
                return None;
            }
        }
    }
}

/// The DNS forwarder tests drive against a local loopback "resolver": a
/// UDP socket bound in the test acts as the upstream nameserver.
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::time::Duration;

    fn config() -> NetConfig {
        NetConfig {
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            gateway_mac: [0x52, 0x55, 0x0A, 0x00, 0x02, 0x02],
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            gateway_ip: Ipv4Addr::new(10, 0, 2, 2),
            mtu: 1500,
        }
    }

    /// A minimal DNS query for the given name and id.
    fn dns_query(id: u16, name: &str) -> Vec<u8> {
        let mut query = Vec::new();
        query.extend_from_slice(&id.to_be_bytes());
        query.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.push(0);
        query.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // A IN
        query
    }

    /// Wraps a DNS payload in the IPv4/UDP envelope the guest would send.
    fn guest_udp(payload: &[u8], guest_port: u16) -> Vec<u8> {
        wrap_udp(
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 2),
            guest_port,
            53,
            payload,
            3,
        )
    }

    #[test]
    fn forwards_a_query_and_relays_the_answer_with_the_guest_id() {
        // The test's own socket plays the host resolver.
        let resolver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let mut state = UdpState::new().unwrap();
        state.upstream = Some(resolver.local_addr().unwrap());
        let config = config();
        let mut counters = NetCounters::default();
        let mut frames = Vec::new();
        let mut ip_id = 0;

        state.handle(
            ipv4::parse(&guest_udp(&dns_query(0xBEEF, "example.com"), 41000)).unwrap(),
            &config,
            &mut counters,
        );
        assert_eq!(counters.dns_queries, 1);

        // The resolver receives the query with a rewritten id.
        let mut buffer = [0_u8; 512];
        let (length, forwarder_addr) = resolver.recv_from(&mut buffer).unwrap();
        let upstream_id = u16::from_be_bytes([buffer[0], buffer[1]]);
        assert_ne!(upstream_id, 0xBEEF);
        assert_eq!(&buffer[12..length], &dns_query(0xBEEF, "example.com")[12..]);

        // Answer: same id, a minimal A record response.
        let mut answer = Vec::new();
        answer.extend_from_slice(&upstream_id.to_be_bytes());
        answer.extend_from_slice(&[0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 0]);
        answer.extend_from_slice(&dns_query(0, "example.com")[12..]);
        answer.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 60, 0, 4]);
        answer.extend_from_slice(&[93, 184, 215, 14]);
        // Reply to the forwarder's socket (the address the query came from),
        // playing the upstream nameserver.
        resolver.send_to(&answer, forwarder_addr).unwrap();

        // The loopback datagram may take a poll or two to arrive.
        let start = Instant::now();
        while counters.dns_answers == 0 && start.elapsed() < Duration::from_secs(5) {
            state.poll(
                Instant::now(),
                &config,
                &mut ip_id,
                &mut counters,
                &mut |frame| frames.push(frame),
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(counters.dns_answers, 1);
        let relayed = frames.last().unwrap();
        let parsed = ipv4::parse(relayed).unwrap();
        assert_eq!(parsed.src, config.gateway_ip);
        assert_eq!(parsed.dst, config.guest_ip);
        assert_eq!(&parsed.payload[0..2], &53_u16.to_be_bytes());
        assert_eq!(&parsed.payload[2..4], &41000_u16.to_be_bytes());
        // The answer carries the guest's original transaction id.
        assert_eq!(&parsed.payload[8..10], &0xBEEF_u16.to_be_bytes());
    }

    #[test]
    fn drops_udp_that_is_not_dns_to_the_gateway() {
        let mut state = UdpState::new().unwrap();
        let config = config();
        let mut counters = NetCounters::default();
        let frames: Vec<Vec<u8>> = Vec::new();
        // UDP to the gateway, but not port 53.
        let packet = guest_udp(&dns_query(1, "example.com"), 41000);
        let mut not_dns = packet.clone();
        // Rewrite the destination port from 53 to 80 inside the UDP header
        // (which starts after the 20-byte IPv4 header).
        not_dns[22..24].copy_from_slice(&80_u16.to_be_bytes());
        state.handle(ipv4::parse(&not_dns).unwrap(), &config, &mut counters);
        assert_eq!(counters.dropped_other_udp, 1);
        assert!(frames.is_empty());
    }

    /// Polls until the drop counter reached the target (or the budget ran
    /// out).
    fn poll_until_dropped(
        state: &mut UdpState,
        config: &NetConfig,
        counters: &mut NetCounters,
        target: u64,
    ) {
        let mut ip_id = 0;
        let start = Instant::now();
        while counters.dns_drops < target && start.elapsed() < Duration::from_secs(5) {
            state.poll(
                Instant::now(),
                config,
                &mut ip_id,
                counters,
                &mut |_frame| {},
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_short_resolver_datagram_is_dropped_instead_of_panicking() {
        // Regression: the transaction id was read from the 4096-byte
        // receive buffer without looking at the datagram length, and the
        // guest-id rewrite then indexed answer[0..2] on a possibly empty
        // vec. A zero-length (or 1-byte) datagram from the configured
        // resolver with a pending id-0 query panicked the host.
        let resolver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let mut state = UdpState::new().unwrap();
        state.upstream = Some(resolver.local_addr().unwrap());
        let config = config();
        let mut counters = NetCounters::default();
        // One pending query whose upstream id is 0: exactly the value the
        // stale-buffer read would match.
        state.pending.insert(
            0,
            PendingQuery {
                guest_id: 0xBEEF,
                guest_port: 41000,
                created: Instant::now(),
            },
        );
        // The forwarder socket binds 0.0.0.0; the resolver reaches it on
        // loopback (the way the real upstream nameserver would).
        let forwarder_addr = SocketAddr::new(
            Ipv4Addr::LOCALHOST.into(),
            state.socket.local_addr().unwrap().port(),
        );
        // A zero-length datagram from the expected upstream address.
        resolver.send_to(&[], forwarder_addr).unwrap();
        poll_until_dropped(&mut state, &config, &mut counters, 1);
        assert_eq!(counters.dns_drops, 1);
        assert_eq!(counters.dns_answers, 0);
        // The pending entry must survive the garbage datagram.
        assert_eq!(state.pending.len(), 1);

        // A 1-byte datagram is equally short and equally dropped.
        resolver.send_to(&[0_u8], forwarder_addr).unwrap();
        let before = counters.dns_drops;
        poll_until_dropped(&mut state, &config, &mut counters, before + 1);
        assert_eq!(counters.dns_drops, before + 1);
        assert_eq!(state.pending.len(), 1);

        // A well-formed answer for the same id still completes the query:
        // the guard rejects short datagrams without harming real traffic.
        let mut answer = Vec::new();
        answer.extend_from_slice(&0_u16.to_be_bytes());
        answer.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        answer.extend_from_slice(&dns_query(0, "example.com")[12..]);
        answer.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        answer.extend_from_slice(&[93, 184, 215, 14]);
        resolver.send_to(&answer, forwarder_addr).unwrap();
        let mut ip_id = 0;
        let mut frames = Vec::new();
        let start = Instant::now();
        while counters.dns_answers == 0 && start.elapsed() < Duration::from_secs(5) {
            state.poll(
                Instant::now(),
                &config,
                &mut ip_id,
                &mut counters,
                &mut |frame| frames.push(frame),
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(counters.dns_answers, 1);
        assert!(state.pending.is_empty());
    }

    #[test]
    fn id_exhaustion_fails_closed_instead_of_hanging() {
        // Regression: with all 65536 ids occupied, one more query spun the
        // allocator forever -- and the expiry sweep runs on the same
        // interpreter thread, so it could never run. The query must be
        // dropped and counted instead of deadlocking.
        let resolver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let mut state = UdpState::new().unwrap();
        state.upstream = Some(resolver.local_addr().unwrap());
        let config = config();
        let mut counters = NetCounters::default();
        for id in 0..=u16::MAX {
            state.pending.insert(
                id,
                PendingQuery {
                    guest_id: id,
                    guest_port: 1,
                    created: Instant::now(),
                },
            );
        }
        state.handle(
            ipv4::parse(&guest_udp(&dns_query(0x1234, "example.com"), 41000)).unwrap(),
            &config,
            &mut counters,
        );
        assert_eq!(counters.dns_drops, 1);
        assert_eq!(counters.dns_queries, 0);
    }

    #[test]
    fn a_query_beyond_the_pending_cap_is_dropped_not_forwarded() {
        // The pending-query table has an explicit cap; queries past it fail
        // closed (dropped, counted) instead of exhausting host state.
        let resolver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        resolver.set_nonblocking(true).unwrap();
        let mut state = UdpState::new().unwrap();
        state.upstream = Some(resolver.local_addr().unwrap());
        let config = config();
        let mut counters = NetCounters::default();
        let cap = 1024; // MAX_PENDING_QUERIES
        for id in 0..cap {
            state.pending.insert(
                id as u16,
                PendingQuery {
                    guest_id: id as u16,
                    guest_port: 40000,
                    created: Instant::now(),
                },
            );
        }
        state.handle(
            ipv4::parse(&guest_udp(&dns_query(0x1234, "example.com"), 41000)).unwrap(),
            &config,
            &mut counters,
        );
        assert_eq!(counters.dns_drops, 1);
        assert_eq!(counters.dns_queries, 0);
        assert_eq!(state.pending.len(), cap);
        // Nothing was forwarded: the resolver socket has no datagram.
        let mut buffer = [0_u8; 64];
        assert!(matches!(
            resolver.recv_from(&mut buffer),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn a_udp_datagram_with_a_lying_length_field_is_dropped() {
        // Regression: the UDP length field was never validated, so a
        // datagram whose header lies about its own length sailed through
        // every check and was forwarded.
        let resolver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        resolver.set_nonblocking(true).unwrap();
        let mut state = UdpState::new().unwrap();
        state.upstream = Some(resolver.local_addr().unwrap());
        let config = config();
        let mut counters = NetCounters::default();
        let mut packet = guest_udp(&dns_query(1, "example.com"), 41000);
        // The UDP header starts after the 20-byte IPv4 header: patch the
        // length field to claim header-only, then recompute the UDP
        // checksum so ONLY the length check can reject the datagram.
        packet[24..26].copy_from_slice(&8_u16.to_be_bytes());
        let mut udp = packet[20..].to_vec();
        udp[6..8].copy_from_slice(&[0, 0]);
        let sum = ipv4::transport_checksum(
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 2),
            ipv4::PROTOCOL_UDP,
            &udp,
        );
        packet[26..28].copy_from_slice(&sum.to_be_bytes());
        state.handle(ipv4::parse(&packet).unwrap(), &config, &mut counters);
        assert_eq!(counters.dropped_bad_packet, 1);
        assert_eq!(counters.dns_queries, 0);
        // Nothing was forwarded: the resolver socket has no datagram.
        let mut buffer = [0_u8; 64];
        assert!(matches!(
            resolver.recv_from(&mut buffer),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn expired_queries_are_swept_before_allocating_an_id() {
        // Regression: with all 65536 ids held by expired queries, one more
        // query had to succeed after the sweep freed them; instead the
        // allocator spun forever on the only thread that could sweep.
        let resolver = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let mut state = UdpState::new().unwrap();
        state.upstream = Some(resolver.local_addr().unwrap());
        let config = config();
        let mut counters = NetCounters::default();
        let stale = Instant::now() - Duration::from_secs(31);
        for id in 0..=u16::MAX {
            state.pending.insert(
                id,
                PendingQuery {
                    guest_id: id,
                    guest_port: 1,
                    created: stale,
                },
            );
        }
        state.handle(
            ipv4::parse(&guest_udp(&dns_query(0x1234, "example.com"), 41000)).unwrap(),
            &config,
            &mut counters,
        );
        // Every expired entry was dropped and counted, one id was freed,
        // and the new query went out.
        assert_eq!(counters.dns_drops, 65536);
        assert_eq!(counters.dns_queries, 1);
        assert_eq!(state.pending.len(), 1);
    }
}
