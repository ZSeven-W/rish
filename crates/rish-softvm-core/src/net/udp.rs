//! UDP handling: DNS forwarding to the host resolver, and nothing else.
//!
//! The backend only terminates UDP on gateway:53 (DNS). Queries are
//! forwarded verbatim to the host's nameserver with the transaction id
//! rewritten so concurrent guest queries cannot collide; answers are mapped
//! back and relayed with recomputed checksums. Queries that time out are
//! dropped. No other UDP service exists: packets to any other port are
//! dropped and counted, and no ICMP port-unreachable is generated.
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
        // Rewrite the transaction id so every outstanding guest query has a
        // unique upstream id.
        let guest_id = u16::from_be_bytes([query[0], query[1]]);
        let upstream_id = self.allocate_id();
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
        // Expire unanswered queries.
        self.pending.retain(|_, pending| {
            let keep = now.duration_since(pending.created) < DNS_QUERY_TIMEOUT;
            if !keep {
                counters.dns_drops = counters.dns_drops.saturating_add(1);
            }
            keep
        });
    }

    /// A transaction id no query currently uses.
    fn allocate_id(&mut self) -> u16 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if !self.pending.contains_key(&id) {
                return id;
            }
        }
    }
}

/// The DNS forwarder tests drive against a local loopback "resolver": a
/// UDP socket bound in the test acts as the upstream nameserver.
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, UdpSocket};
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
}
