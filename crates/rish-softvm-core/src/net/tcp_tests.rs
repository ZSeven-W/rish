use super::super::ipv4;
use super::*;
use std::net::TcpListener;
use std::thread;

fn config() -> NetConfig {
    NetConfig {
        mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
        gateway_mac: [0x52, 0x55, 0x0A, 0x00, 0x02, 0x02],
        guest_ip: Ipv4Addr::new(10, 0, 2, 15),
        gateway_ip: Ipv4Addr::new(10, 0, 2, 2),
        mtu: 1500,
    }
}

/// Builds the guest-side IPv4/TCP packet for a segment.
fn guest_tcp(segment: &[u8], remote: Ipv4Addr) -> Vec<u8> {
    wrap_tcp(Ipv4Addr::new(10, 0, 2, 15), remote, segment, 7)
}

/// Handles one guest packet into state, appending emitted frames.
fn handle(
    state: &mut TcpState,
    packet: Vec<u8>,
    counters: &mut NetCounters,
    frames: &mut Vec<Vec<u8>>,
) {
    let mut ip_id = 0;
    state.handle(
        ipv4::parse(&packet).unwrap(),
        &config(),
        &mut ip_id,
        counters,
        &mut |frame| frames.push(frame),
    );
}

/// Drives the backend until the condition holds or the timeout expires.
fn poll_until<F: FnMut(&mut Vec<Vec<u8>>, &NetCounters) -> bool>(
    state: &mut TcpState,
    counters: &mut NetCounters,
    mut condition: F,
    timeout: Duration,
) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut ip_id = 0;
    let start = Instant::now();
    loop {
        state.poll(
            Instant::now(),
            &config(),
            &mut ip_id,
            counters,
            &mut |frame| frames.push(frame),
        );
        if condition(&mut frames, counters) {
            return frames;
        }
        if start.elapsed() > timeout {
            return frames;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn segments_round_trip_and_options_parse() {
    let segment = build_segment(
        1234,
        80,
        0xDEAD_BEEF,
        0x0102_0304,
        FLAG_SYN,
        65535,
        &[2, 4, 0x05, 0xB4],
        b"payload",
    );
    let parsed = parse_segment(&segment).unwrap();
    assert_eq!(parsed.sport, 1234);
    assert_eq!(parsed.dport, 80);
    assert_eq!(parsed.seq, 0xDEAD_BEEF);
    assert_eq!(parsed.flags, FLAG_SYN);
    assert_eq!(parsed.payload, b"payload");
    assert_eq!(parse_mss(parsed.options), Some(1460));
    assert!(parse_segment(&segment[..19]).is_none());
    // A window-scale option (kind 3, len 3) is skipped before the MSS.
    assert_eq!(parse_mss(&[3, 3, 7, 2, 4, 0x05, 0xB4]), Some(1460));
}

#[test]
fn a_corrupt_checksum_is_dropped_and_counted() {
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    let segment = build_segment(40000, 80, 100, 0, FLAG_SYN, 65535, &[], &[]);
    let mut packet = guest_tcp(&segment, Ipv4Addr::new(93, 184, 215, 14));
    packet[20 + 4] ^= 0xFF; // corrupt a header byte: checksum mismatch
    handle(&mut state, packet, &mut counters, &mut frames);
    assert_eq!(counters.dropped_bad_checksum, 1);
    assert_eq!(counters.tcp_opened, 0);
}

#[test]
fn unknown_connections_get_a_reset() {
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    let segment = build_segment(40000, 80, 100, 0, FLAG_ACK, 65535, &[], &[]);
    handle(
        &mut state,
        guest_tcp(&segment, Ipv4Addr::new(93, 184, 215, 14)),
        &mut counters,
        &mut frames,
    );
    assert_eq!(counters.tcp_resets, 1);
    let reply = ipv4::parse(&frames[0]).unwrap();
    assert_eq!(reply.protocol, ipv4::PROTOCOL_TCP);
    assert_eq!(
        reply.payload[13] & (FLAG_RST | FLAG_ACK),
        FLAG_RST | FLAG_ACK,
    );
}

#[test]
fn full_connection_over_host_loopback() {
    use std::io::{Read, Write};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let remote = Ipv4Addr::new(127, 0, 0, 1);
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();

    // Guest SYN.
    let syn = build_segment(40000, remote_port, 1000, 0, FLAG_SYN, 65535, &[], &[]);
    handle(
        &mut state,
        guest_tcp(&syn, remote),
        &mut counters,
        &mut frames,
    );
    assert_eq!(counters.tcp_opened, 1);

    // The host thread connects and the backend answers SYN-ACK from the
    // remote address.
    let mut frames = poll_until(
        &mut state,
        &mut counters,
        |frames, _counters| {
            frames.iter().any(|frame| {
                let parsed = ipv4::parse(frame).unwrap();
                parsed.payload[13] & FLAG_SYN != 0
            })
        },
        Duration::from_secs(5),
    );
    let syn_ack = frames
        .iter()
        .find_map(|frame| {
            let parsed = ipv4::parse(frame).unwrap();
            (parsed.payload[13] & FLAG_SYN != 0).then_some(parsed)
        })
        .expect("SYN-ACK");
    assert_eq!(syn_ack.src, remote);
    assert_eq!(&syn_ack.payload[0..2], &remote_port.to_be_bytes());
    let our_isn = u32::from_be_bytes(syn_ack.payload[4..8].try_into().unwrap());

    // Accept the host-side connection.
    let (mut host, _) = listener.accept().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // Guest ACK of the SYN-ACK.
    let ack = build_segment(
        40000,
        remote_port,
        1001,
        our_isn.wrapping_add(1),
        FLAG_ACK,
        65535,
        &[],
        &[],
    );
    handle(
        &mut state,
        guest_tcp(&ack, remote),
        &mut counters,
        &mut frames,
    );
    // The handshake acknowledgement pops the SYN-ACK from the retransmit
    // queue: the timer must be disarmed (regression: it used to retransmit
    // the SYN-ACK forever and reset the connection).
    {
        let key = ConnKey {
            local_port: 40000,
            remote_addr: remote.octets(),
            remote_port,
        };
        let conn = state.connections.get(&key).unwrap();
        assert!(conn.established);
        assert!(conn.unacked.is_empty());
        assert!(conn.retransmit_deadline.is_none());
    }

    // Guest data reaches the host socket.
    let data = build_segment(
        40000,
        remote_port,
        1001,
        our_isn.wrapping_add(1),
        FLAG_ACK | 0x08,
        65535,
        &[],
        b"hello from guest",
    );
    handle(
        &mut state,
        guest_tcp(&data, remote),
        &mut counters,
        &mut frames,
    );
    let mut buffer = [0_u8; 64];
    let length = host.read(&mut buffer).unwrap();
    assert_eq!(&buffer[..length], b"hello from guest");

    // Host data reaches the guest as a segment from the remote address.
    host.write_all(b"reply from host").unwrap();
    let mut frames = poll_until(
        &mut state,
        &mut counters,
        |frames, _counters| {
            frames.iter().any(|frame| {
                let parsed = ipv4::parse(frame).unwrap();
                parsed.protocol == ipv4::PROTOCOL_TCP
                    && parsed.payload[13] & FLAG_SYN == 0
                    && !parsed.payload[20..].is_empty()
            })
        },
        Duration::from_secs(5),
    );
    let reply = frames
        .iter()
        .find_map(|frame| {
            let parsed = ipv4::parse(frame).unwrap();
            (parsed.payload[13] & FLAG_SYN == 0 && !parsed.payload[20..].is_empty())
                .then_some(parsed)
        })
        .expect("host data segment");
    assert_eq!(reply.src, remote);
    assert_eq!(&reply.payload[20..], b"reply from host");
    let data_seq = u32::from_be_bytes(reply.payload[4..8].try_into().unwrap());

    // Guest acknowledges the data; then the host closes and the FIN
    // handshake completes.
    let ack = build_segment(
        40000,
        remote_port,
        1017, // 1001 + len("hello from guest") = 16
        data_seq.wrapping_add(15),
        FLAG_ACK,
        65535,
        &[],
        &[],
    );
    handle(
        &mut state,
        guest_tcp(&ack, remote),
        &mut counters,
        &mut frames,
    );
    drop(host); // host closes: EOF travels to the backend
    let mut frames = poll_until(
        &mut state,
        &mut counters,
        |frames, _counters| {
            frames.iter().any(|frame| {
                let parsed = ipv4::parse(frame).unwrap();
                parsed.payload[13] & FLAG_FIN != 0
            })
        },
        Duration::from_secs(5),
    );
    assert!(!frames.is_empty());

    // Guest FIN + ACK of our FIN closes the connection.
    let fin = build_segment(
        40000,
        remote_port,
        1017,
        data_seq.wrapping_add(16),
        FLAG_FIN | FLAG_ACK,
        65535,
        &[],
        &[],
    );
    handle(
        &mut state,
        guest_tcp(&fin, remote),
        &mut counters,
        &mut frames,
    );
    let _frames = poll_until(
        &mut state,
        &mut counters,
        |_frames, counters| counters.tcp_closed == 1,
        Duration::from_secs(5),
    );
    assert_eq!(counters.tcp_closed, 1);
}
