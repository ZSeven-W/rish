//! Host-side user-mode network backend (slirp style) for the virtio-net
//! device.
//!
//! The backend terminates the guest's Ethernet frames in ordinary host
//! sockets. It speaks a deliberately small protocol surface:
//!
//! - ARP: answers requests for the gateway address.
//! - IPv4: forwards only unfragmented packets with a valid header checksum.
//! - ICMP: answers echo requests addressed to the gateway (so the guest can
//!   ping 10.0.2.2 to prove the link).
//! - UDP: forwards DNS queries addressed to the gateway port 53 to the host
//!   resolver and relays the answers back.
//! - TCP: proxies outbound connections from the guest to real hosts over
//!   host sockets, translating the guest's TCP stream (see the tcp module
//!   for what the state machine does and does not implement).
//!
//! Everything else fails closed: frames are dropped and counted, never
//! guessed at. The guest is configured statically (see the guest init):
//! 10.0.2.15/24 with gateway 10.0.2.2, DNS at the gateway address.
//!
//! The backend is driven by the device poll on the interpreter's device
//! tick. Outbound host sockets run on dedicated host threads that hand data
//! back through bounded channels, so the interpreter thread never blocks on
//! a host socket.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

mod ether;
mod host;
mod ipv4;
mod tcp;
mod tcp_segment;
mod udp;

use ether::for_us;
use tcp::TcpState;
use udp::UdpState;

/// Transport the virtio-net device speaks to. The device owns the
/// virtqueues and the register file; the backend owns everything above
/// Ethernet.
pub trait NetBackend: Send {
    /// The fixed MAC the device reports in its config space. The backend
    /// also uses it to filter inbound frames.
    fn mac(&self) -> [u8; 6];
    /// Handles one Ethernet frame the guest transmitted. Frames the backend
    /// answers are queued internally, not returned here.
    fn enqueue(&mut self, frame: &[u8]);
    /// Advances host-side state: socket readiness, TCP timers, and pending
    /// replies. Frames destined for the guest are appended to output.
    fn poll(&mut self, output: &mut Vec<Vec<u8>>);
    /// Protocol counters, for tests and diagnostics.
    fn counters(&self) -> NetCounters;
    /// Tears down every connection, pending DNS query, and queued frame.
    /// Called when the device resets (status 0): stale host-side state must
    /// not survive into the next driver session. The default no-op keeps
    /// minimal backends simple.
    fn reset(&mut self) {}
}

/// Fixed addressing for the emulated network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetConfig {
    /// The guest interface MAC (the device config-space MAC).
    pub mac: [u8; 6],
    /// The MAC the backend answers for on the emulated wire (the slirp
    /// convention QEMU uses for its gateway).
    pub gateway_mac: [u8; 6],
    pub guest_ip: Ipv4Addr,
    pub gateway_ip: Ipv4Addr,
    /// IP payload MTU; frames carrying more are dropped by the device.
    pub mtu: usize,
}

impl NetConfig {
    /// The slirp-conventional defaults the provider attaches: guest
    /// 10.0.2.15 behind gateway 10.0.2.2, QEMU's classic virtio-net MAC.
    #[must_use]
    pub fn slirp_defaults() -> Self {
        Self {
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            gateway_mac: [0x52, 0x55, 0x0A, 0x00, 0x02, 0x02],
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            gateway_ip: Ipv4Addr::new(10, 0, 2, 2),
            mtu: 1500,
        }
    }
}

/// Monotonic protocol counters. Every dropped packet class is counted so a
/// misbehaving guest is visible in diagnostics instead of silent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NetCounters {
    pub arp_replies: u64,
    pub icmp_echo_replies: u64,
    pub dns_queries: u64,
    pub dns_answers: u64,
    pub dns_drops: u64,
    pub tcp_opened: u64,
    pub tcp_closed: u64,
    pub tcp_resets: u64,
    pub tcp_retransmits: u64,
    pub dropped_unknown_ethertype: u64,
    pub dropped_not_for_us: u64,
    pub dropped_not_ipv4: u64,
    pub dropped_fragmented: u64,
    pub dropped_bad_checksum: u64,
    pub dropped_other_icmp: u64,
    pub dropped_other_udp: u64,
    pub dropped_bad_packet: u64,
}

/// Bytes the backend buffers per direction of one TCP connection before it
/// starts shedding load (host-side buffer cap) or stops reading the host
/// socket (channel backpressure).
const TCP_PENDING_CAP: usize = 256 * 1024;

/// The slirp-style backend itself.
pub struct SlirpNetBackend {
    config: NetConfig,
    counters: NetCounters,
    /// Frames ready for the guest, drained by the device poll.
    output: VecDeque<Vec<u8>>,
    /// IPv4 identification counter.
    ip_id: u16,
    udp: UdpState,
    tcp: TcpState,
}

impl SlirpNetBackend {
    /// Builds the backend: binds the host DNS socket and reads the host
    /// resolver address from /etc/resolv.conf. DNS silently degrades (every
    /// query dropped, counted) when the host exposes no resolver; TCP still
    /// works by IP.
    pub fn new(config: NetConfig) -> Result<Self, std::io::Error> {
        Ok(Self {
            config,
            counters: NetCounters::default(),
            output: VecDeque::new(),
            ip_id: 0,
            udp: UdpState::new()?,
            tcp: TcpState::new(),
        })
    }

    /// Wraps an IPv4 packet in an Ethernet frame (gateway MAC to guest MAC)
    /// and queues it for the guest, dropping the oldest frame first under
    /// memory pressure so a stuck guest cannot grow the queue.
    fn emit(&mut self, packet: Vec<u8>) {
        self.emit_frame(ether::frame(
            self.config.mac,
            self.config.gateway_mac,
            0x0800,
            &packet,
        ));
    }

    /// Queues one already-framed Ethernet frame (used by the ARP handler).
    fn emit_frame(&mut self, frame: Vec<u8>) {
        const OUTPUT_CAP: usize = 1024;
        if self.output.len() >= OUTPUT_CAP {
            self.output.pop_front();
            self.counters.dropped_bad_packet = self.counters.dropped_bad_packet.saturating_add(1);
        }
        self.output.push_back(frame);
    }

    fn handle_arp(&mut self, payload: &[u8]) {
        let Some((sender_mac, sender_ip, target_ip)) = ether::parse_arp_request(payload) else {
            self.counters.dropped_bad_packet = self.counters.dropped_bad_packet.saturating_add(1);
            return;
        };
        if target_ip != self.config.gateway_ip {
            // The backend only owns the gateway address; everything else is
            // not ours to answer.
            return;
        }
        if let Some(reply) = ether::build_arp_reply(
            self.config.gateway_mac,
            self.config.gateway_ip,
            sender_mac,
            sender_ip,
        ) {
            self.emit_frame(reply);
            self.counters.arp_replies = self.counters.arp_replies.saturating_add(1);
        }
    }

    fn handle_ipv4(&mut self, payload: &[u8]) {
        let Some(packet) = ipv4::parse(payload) else {
            self.counters.dropped_bad_packet = self.counters.dropped_bad_packet.saturating_add(1);
            return;
        };
        if packet.more_fragments || packet.fragment_offset != 0 {
            self.counters.dropped_fragmented = self.counters.dropped_fragmented.saturating_add(1);
            return;
        }
        if !ipv4::header_checksum_ok(packet.header) {
            self.counters.dropped_bad_checksum =
                self.counters.dropped_bad_checksum.saturating_add(1);
            return;
        }
        match packet.protocol {
            ipv4::PROTOCOL_ICMP => {
                if packet.dst != self.config.gateway_ip {
                    self.counters.dropped_other_icmp =
                        self.counters.dropped_other_icmp.saturating_add(1);
                    return;
                }
                if let Some(reply) = ipv4::icmp_echo_reply(packet, &mut self.ip_id) {
                    self.emit(reply);
                    self.counters.icmp_echo_replies =
                        self.counters.icmp_echo_replies.saturating_add(1);
                } else {
                    self.counters.dropped_other_icmp =
                        self.counters.dropped_other_icmp.saturating_add(1);
                }
            }
            ipv4::PROTOCOL_UDP => {
                self.udp.handle(packet, &self.config, &mut self.counters);
            }
            ipv4::PROTOCOL_TCP => {
                let mut frames = Vec::new();
                self.tcp.handle(
                    packet,
                    &self.config,
                    &mut self.ip_id,
                    &mut self.counters,
                    &mut |frame| frames.push(frame),
                );
                for frame in frames {
                    self.emit(frame);
                }
            }
            _ => {
                self.counters.dropped_not_ipv4 = self.counters.dropped_not_ipv4.saturating_add(1);
            }
        }
    }
}

impl NetBackend for SlirpNetBackend {
    fn mac(&self) -> [u8; 6] {
        self.config.mac
    }

    fn enqueue(&mut self, frame: &[u8]) {
        let Some((dst, ethertype, payload)) = ether::parse(frame) else {
            self.counters.dropped_bad_packet = self.counters.dropped_bad_packet.saturating_add(1);
            return;
        };
        if !for_us(dst, self.config.mac, self.config.gateway_mac) {
            self.counters.dropped_not_for_us = self.counters.dropped_not_for_us.saturating_add(1);
            return;
        }
        match ethertype {
            0x0806 => self.handle_arp(payload),
            0x0800 => self.handle_ipv4(payload),
            _ => {
                self.counters.dropped_unknown_ethertype =
                    self.counters.dropped_unknown_ethertype.saturating_add(1);
            }
        }
    }

    fn poll(&mut self, output: &mut Vec<Vec<u8>>) {
        let now = Instant::now();
        let mut frames = Vec::new();
        self.udp.poll(
            now,
            &self.config,
            &mut self.ip_id,
            &mut self.counters,
            &mut |frame| frames.push(frame),
        );
        self.tcp.poll(
            now,
            &self.config,
            &mut self.ip_id,
            &mut self.counters,
            &mut |frame| frames.push(frame),
        );
        for frame in frames {
            self.emit(frame);
        }
        while let Some(frame) = self.output.pop_front() {
            output.push(frame);
        }
    }

    fn counters(&self) -> NetCounters {
        self.counters
    }
}

/// Host resolver address for the DNS forwarder, read once at construction.
/// Returns the first nameserver /etc/resolv.conf lists.
pub fn host_nameserver() -> Option<std::net::SocketAddr> {
    let contents = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        if fields.next()? == "nameserver" {
            let address = fields.next()?.parse().ok()?;
            return Some(std::net::SocketAddr::new(address, 53));
        }
    }
    None
}

/// Idle timeout for unanswered DNS queries.
const DNS_QUERY_TIMEOUT: Duration = Duration::from_secs(30);
