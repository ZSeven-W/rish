use super::super::host::spawn_host_thread_with_timeout;
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

/// Builds a TcpConn detached from any host activity, for driving the
/// private state-machine functions directly.
fn fake_conn(key: ConnKey) -> TcpConn {
    let (guest_tx, guest_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(32);
    // Keep the receiver alive so guest_tx sends succeed (the conn's own
    // host thread is absent in these tests).
    std::mem::forget(guest_rx);
    let (_host_tx, host_rx) = std::sync::mpsc::sync_channel::<HostEvent>(32);
    TcpConn {
        key,
        established: true,
        guest_next: 1000,
        guest_window: 65535,
        guest_mss: OUR_MSS,
        our_isn: 0x0100_0000,
        our_next: 0x0100_0000,
        our_una: 0x0100_0000,
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
    }
}

#[test]
fn connection_creation_is_capped_and_excess_syns_get_a_reset() {
    // Regression: every SYN spawned a host thread with no limit, and a
    // hostile guest could exhaust host threads and sockets. Past the cap
    // the SYN must be refused with a RST, not opened.
    let remote = Ipv4Addr::new(127, 0, 0, 1);
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    let cap = 64; // MAX_CONNECTIONS
    for port in 0..cap {
        let syn = build_segment(40000 + port as u16, 9, 1000, 0, FLAG_SYN, 65535, &[], &[]);
        handle(
            &mut state,
            guest_tcp(&syn, remote),
            &mut counters,
            &mut frames,
        );
    }
    assert_eq!(counters.tcp_opened, cap as u64);
    assert_eq!(state.connections.len(), cap);
    // The 65th SYN is refused with a reset.
    let syn = build_segment(40000 + 64, 9, 1000, 0, FLAG_SYN, 65535, &[], &[]);
    handle(
        &mut state,
        guest_tcp(&syn, remote),
        &mut counters,
        &mut frames,
    );
    assert_eq!(counters.tcp_opened, cap as u64);
    assert_eq!(state.connections.len(), cap);
    assert_eq!(counters.tcp_resets, 1);
    assert!(frames.iter().any(|frame| {
        let parsed = ipv4::parse(frame).unwrap();
        parsed.payload[13] & (FLAG_RST | FLAG_ACK) == FLAG_RST | FLAG_ACK
    }));
    // Releasing the state tears every thread down (their connects to a
    // closed port fail fast).
    drop(state);
}

#[test]
fn a_connect_to_a_black_hole_fails_within_the_timeout() {
    // The connect is the one teardown path a guest RST or device reset
    // cannot interrupt: it runs on its own thread. The timeout is what
    // reclaims the thread and its socket. TEST-NET-3 is unroutable from a
    // normal host, so the connect can only end by timing out (or failing
    // fast with no route -- both bounded).
    let remote = SocketAddr::new(Ipv4Addr::new(203, 0, 113, 1).into(), 81);
    let (guest_tx, host_rx) = spawn_host_thread_with_timeout(remote, Duration::from_millis(200));
    let start = Instant::now();
    let outcome = host_rx.recv_timeout(Duration::from_secs(5));
    assert!(
        matches!(outcome, Ok(HostEvent::ConnectFailed(_))),
        "connect neither failed nor timed out: {outcome:?}",
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    // Dropping the channel half reclaims the thread.
    drop(guest_tx);
}

#[test]
fn a_reset_drops_every_connection_and_reclaims_the_host_socket() {
    // Device reset must tear down all backend connections: the channel
    // halves disconnect, the host thread exits, and its socket closes --
    // no stale connection or data survives into the next driver session.
    use std::io::Read;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let remote = Ipv4Addr::new(127, 0, 0, 1);
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    let syn = build_segment(40000, remote_port, 1000, 0, FLAG_SYN, 65535, &[], &[]);
    handle(
        &mut state,
        guest_tcp(&syn, remote),
        &mut counters,
        &mut frames,
    );
    assert_eq!(state.connections.len(), 1);
    // The host thread connects to the listener.
    let (mut host_stream, _) = listener.accept().unwrap();
    host_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    state.reset();
    assert!(state.connections.is_empty());
    // The thread observes the channel disconnect and exits, closing its
    // socket: the accepted side reads EOF promptly.
    let mut buffer = [0_u8; 16];
    assert_eq!(host_stream.read(&mut buffer).unwrap(), 0);
}

#[test]
fn isn_seeds_stay_distinct_when_the_clock_reads_before_1970() {
    // Regression: a host clock before 1970 used to collapse every seed to
    // one fixed constant, making initial sequence numbers predictable
    // across guests. The salt alone must keep seeds distinct.
    assert_ne!(isn_seed(None, 1), isn_seed(None, 2));
}

#[test]
fn a_host_burst_over_the_pending_cap_applies_backpressure_not_a_reset() {
    // Regression: host data arriving faster than one poll forwards it
    // pushed the pending buffer past its cap and the backend reset the
    // connection mid-transfer. The cap must act as a backpressure point:
    // stop draining events (the bounded channel then stalls the host
    // thread, which stops reading the socket), never as a kill switch.
    let key = ConnKey {
        local_port: 40000,
        remote_addr: [127, 0, 0, 1],
        remote_port: 80,
    };
    let mut conn = fake_conn(key);
    let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<HostEvent>(64);
    conn.host_rx = event_rx;
    // 33 x 16 KiB = 528 KiB queued before a single poll drains it.
    for _ in 0..33 {
        event_tx
            .send(HostEvent::Data(vec![0xAB_u8; 16 * 1024]))
            .unwrap();
    }
    let mut counters = NetCounters::default();
    drain_host_events(
        &mut conn,
        Instant::now(),
        &config(),
        &mut 0,
        &mut counters,
        &mut |_frame| {},
    );
    assert!(!conn.closed);
    assert_eq!(counters.tcp_resets, 0);
    // The buffer is bounded at the cap (plus at most one 16 KiB event) and
    // the remaining events stay queued for later polls.
    assert!(conn.pending.len() >= TCP_PENDING_CAP - 16 * 1024);
    assert!(conn.pending.len() <= TCP_PENDING_CAP + 16 * 1024);
    assert!(matches!(conn.host_rx.try_recv(), Ok(HostEvent::Data(_))));
}

#[test]
fn an_ack_outside_the_receive_window_is_ignored() {
    // Regression: an ACK past our_next was clamped to our_next and then
    // acknowledged data the guest never received; an ACK below our_una
    // could appear to advance. Only ACKs inside [our_una, our_next] may
    // touch the retransmission queue.
    let key = ConnKey {
        local_port: 40000,
        remote_addr: [127, 0, 0, 1],
        remote_port: 80,
    };
    let mut conn = fake_conn(key);
    conn.our_next = conn.our_una.wrapping_add(100);
    conn.unacked.push_back(OutSeg {
        seq: conn.our_una,
        bytes: vec![0_u8; 100],
        syn: false,
        last_sent: Instant::now(),
    });
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    // An ACK past our_next: outside the receive window, but close enough
    // for naive sequence comparison to treat it as progress. The old code
    // clamped it to our_next and confirmed data the guest never received.
    let beyond = build_segment(
        40000,
        80,
        conn.guest_next,
        conn.our_next.wrapping_add(0x1000),
        FLAG_ACK,
        65535,
        &[],
        &[],
    );
    let segment = parse_segment(&beyond).unwrap();
    process_segment(
        &mut conn,
        segment,
        &config(),
        &mut 0,
        &mut counters,
        &mut |frame| frames.push(frame),
    );
    assert_eq!(conn.unacked.len(), 1);
    assert_eq!(conn.our_una, 0x0100_0000);
    // A legitimate partial ACK inside the window advances our_una.
    let partial = build_segment(
        40000,
        80,
        conn.guest_next,
        conn.our_una.wrapping_add(50),
        FLAG_ACK,
        65535,
        &[],
        &[],
    );
    let segment = parse_segment(&partial).unwrap();
    process_segment(
        &mut conn,
        segment,
        &config(),
        &mut 0,
        &mut counters,
        &mut |frame| frames.push(frame),
    );
    assert_eq!(conn.unacked.len(), 1);
    assert_eq!(conn.our_una, 0x0100_0032);
    // The full ACK completes the segment.
    let full = build_segment(
        40000,
        80,
        conn.guest_next,
        conn.our_next,
        FLAG_ACK,
        65535,
        &[],
        &[],
    );
    let segment = parse_segment(&full).unwrap();
    process_segment(
        &mut conn,
        segment,
        &config(),
        &mut 0,
        &mut counters,
        &mut |frame| frames.push(frame),
    );
    assert!(conn.unacked.is_empty());
    assert_eq!(conn.our_una, conn.our_next);
}

#[test]
fn retransmitted_data_never_reuses_the_syn_flag() {
    // Regression: the retransmission path decided SYN vs data by comparing
    // the segment sequence against our_isn; after a 2^32 wrap a data
    // segment whose sequence landed on our_isn would retransmit as SYN-ACK.
    let key = ConnKey {
        local_port: 40000,
        remote_addr: [127, 0, 0, 1],
        remote_port: 80,
    };
    let mut conn = fake_conn(key);
    // Force the wrap scenario directly: data starts exactly at our_isn.
    conn.our_isn = 0;
    conn.our_una = 0;
    conn.our_next = 0;
    conn.pending.extend(vec![0x11_u8; 100]);
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    send_pending(&mut conn, Instant::now(), &config(), &mut 0, &mut |frame| {
        frames.push(frame)
    });
    frames.clear();
    conn.retransmit_deadline = Some(Instant::now());
    retransmit(
        &mut conn,
        Instant::now() + RETRANSMIT_TIMEOUT,
        &config(),
        &mut 0,
        &mut counters,
        &mut |frame| frames.push(frame),
    );
    let parsed = ipv4::parse(&frames[0]).unwrap();
    assert_eq!(parsed.payload[13] & FLAG_SYN, 0);
    assert_eq!(parsed.payload[13] & FLAG_ACK, FLAG_ACK);
    assert_eq!(&parsed.payload[20..], &vec![0x11_u8; 100][..]);
}

#[test]
fn retransmit_exhaustion_sends_the_rst_it_counts() {
    // Regression: when the retransmission budget ran out the backend
    // counted a reset and closed the connection without ever sending the
    // RST segment, leaving a lossy guest to wait out its own (much longer)
    // timeout in silence.
    let key = ConnKey {
        local_port: 40000,
        remote_addr: [127, 0, 0, 1],
        remote_port: 80,
    };
    let mut conn = fake_conn(key);
    conn.unacked.push_back(OutSeg {
        seq: conn.our_una,
        bytes: vec![0x22_u8; 100],
        syn: false,
        last_sent: Instant::now(),
    });
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    for _ in 0..=MAX_RETRANSMITS {
        conn.retransmit_deadline = Some(Instant::now());
        retransmit(
            &mut conn,
            Instant::now() + RETRANSMIT_TIMEOUT,
            &config(),
            &mut 0,
            &mut counters,
            &mut |frame| frames.push(frame),
        );
        if conn.closed {
            break;
        }
    }
    assert!(conn.closed);
    assert_eq!(counters.tcp_resets, 1);
    let parsed = ipv4::parse(frames.last().unwrap()).unwrap();
    assert_eq!(
        parsed.payload[13] & (FLAG_RST | FLAG_ACK),
        FLAG_RST | FLAG_ACK
    );
}

#[test]
fn a_payload_and_fin_in_one_segment_advances_the_fin_sequence() {
    // Regression: the FIN check compared the segment's sequence against
    // guest_next AFTER the payload had advanced it, so a segment carrying
    // payload+FIN was treated as a duplicate FIN and the close never
    // completed. The FIN lands at seq + payload_len.
    let key = ConnKey {
        local_port: 40000,
        remote_addr: [127, 0, 0, 1],
        remote_port: 80,
    };
    let mut conn = fake_conn(key);
    let segment = build_segment(40000, 80, 1000, 0, FLAG_ACK | FLAG_FIN, 65535, &[], b"data");
    let segment = parse_segment(&segment).unwrap();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();
    process_segment(
        &mut conn,
        segment,
        &config(),
        &mut 0,
        &mut counters,
        &mut |frame| frames.push(frame),
    );
    assert!(conn.guest_fin);
    assert!(conn.guest_fin_acked);
    assert_eq!(conn.guest_next, 1005);
    // The ACK covers the payload and the FIN.
    let ack_frame = ipv4::parse(&frames[0]).unwrap();
    assert_eq!(
        u32::from_be_bytes(ack_frame.payload[8..12].try_into().unwrap()),
        1005
    );
}

#[test]
fn a_transient_pre_response_close_is_redialed_instead_of_killing_the_fetch() {
    // Regression: a CDN edge that resets or closes a freshly accepted
    // connection (rate limiting, load shedding) used to surface as a FIN
    // right after the SYN-ACK, and apk reported "TLS: unspecified error".
    // Until the first response byte arrives the host thread must redial the
    // same address and replay the pre-response flight, so the guest sees
    // one unbroken connection and its data lands on the dial that stays.
    use std::io::{Read, Write};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let remote = Ipv4Addr::new(127, 0, 0, 1);
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();

    // Server side: close the first two connections without a single byte,
    // then serve the third and echo the payload back.
    std::thread::spawn(move || {
        for attempt in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            if attempt < 2 {
                drop(stream);
                continue;
            }
            let mut buffer = [0_u8; 64];
            let length = stream.read(&mut buffer).unwrap();
            stream.write_all(&buffer[..length]).unwrap();
            drop(stream);
        }
    });

    // Guest SYN, SYN-ACK, handshake ACK.
    let syn = build_segment(40000, remote_port, 1000, 0, FLAG_SYN, 65535, &[], &[]);
    handle(
        &mut state,
        guest_tcp(&syn, remote),
        &mut counters,
        &mut frames,
    );
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
    let our_isn = u32::from_be_bytes(syn_ack.payload[4..8].try_into().unwrap());
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

    // Guest data: the first two dials die before a response byte, so the
    // host thread must replay this flight on the third dial.
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

    // The echo arrives as a data segment from the remote address, with no
    // RST and no premature FIN in between.
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
        Duration::from_secs(10),
    );
    let reply = frames
        .iter()
        .find_map(|frame| {
            let parsed = ipv4::parse(frame).unwrap();
            (parsed.payload[13] & FLAG_SYN == 0 && !parsed.payload[20..].is_empty())
                .then_some(parsed)
        })
        .expect("echoed data segment");
    assert_eq!(reply.src, remote);
    assert_eq!(&reply.payload[20..], b"hello from guest");
    assert_eq!(counters.tcp_resets, 0);
    assert!(!frames.iter().any(|frame| {
        let parsed = ipv4::parse(frame).unwrap();
        parsed.protocol == ipv4::PROTOCOL_TCP && parsed.payload[13] & FLAG_FIN != 0
    }));
    drop(state);
}

#[test]
fn a_megabyte_transfer_drains_through_the_pending_cap_without_a_reset_or_stall() {
    // Regression guard for the backpressure path: a host burst well past
    // TCP_PENDING_CAP must stall the event drain (backpressure), not reset
    // the connection (the old kill switch) and not deadlock the resume.
    // The host streams a megabyte while the test acknowledges every
    // segment it receives; the whole payload must arrive, with no RST.
    use std::io::Write;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let remote = Ipv4Addr::new(127, 0, 0, 1);
    let mut state = TcpState::new();
    let mut counters = NetCounters::default();
    let mut frames = Vec::new();

    let total: usize = 1024 * 1024;
    std::thread::spawn(move || {
        let (mut host, _) = listener.accept().unwrap();
        host.set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let chunk = vec![0x6B_u8; 16 * 1024];
        let mut sent = 0_usize;
        while sent < total {
            let take = (total - sent).min(chunk.len());
            host.write_all(&chunk[..take]).unwrap();
            sent += take;
        }
        drop(host);
    });

    // Handshake.
    let syn = build_segment(40000, remote_port, 1000, 0, FLAG_SYN, 65535, &[], &[]);
    handle(
        &mut state,
        guest_tcp(&syn, remote),
        &mut counters,
        &mut frames,
    );
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
    let our_isn = u32::from_be_bytes(syn_ack.payload[4..8].try_into().unwrap());
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

    // Transfer loop: poll the backend, acknowledge every segment it
    // emitted (exactly the receive-then-ack cycle a guest performs), and
    // collect the payload. The host thread's 64+ events (1 MiB) must cross
    // the 256 KiB pending cap several times over. The host can start
    // streaming before the handshake ACK is processed, so the frames the
    // SYN-ACK wait collected flow through the same loop instead of being
    // cleared.
    let mut received = 0_usize;
    let mut fin_seen = false;
    let mut ip_id = 0_u16;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fin_seen && Instant::now() < deadline {
        state.poll(
            Instant::now(),
            &config(),
            &mut ip_id,
            &mut counters,
            &mut |frame| frames.push(frame),
        );
        // Take the batch out so the handle() calls below may append the
        // backend's replies to the frame list without aliasing the loop.
        let batch = std::mem::take(&mut frames);
        if batch.is_empty() {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        for frame in &batch {
            let parsed = ipv4::parse(frame).unwrap();
            if parsed.protocol != ipv4::PROTOCOL_TCP {
                continue;
            }
            let payload = parsed.payload;
            let flags = payload[13];
            if flags & FLAG_SYN != 0 {
                // The SYN-ACK collected by the wait above: already acked.
                continue;
            }
            let seq = u32::from_be_bytes(payload[4..8].try_into().unwrap());
            let data_offset = usize::from(payload[12] >> 4) * 4;
            let data_len = payload.len() - data_offset;
            if flags & FLAG_FIN != 0 {
                fin_seen = true;
            }
            let ack_num = seq
                .wrapping_add(data_len as u32)
                .wrapping_add(u32::from(flags & FLAG_FIN != 0));
            let ack_seg =
                build_segment(40000, remote_port, 1001, ack_num, FLAG_ACK, 65535, &[], &[]);
            handle(
                &mut state,
                guest_tcp(&ack_seg, remote),
                &mut counters,
                &mut frames,
            );
            received += data_len;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        fin_seen,
        "FIN never arrived: received {received} bytes, counters {counters:?}"
    );
    assert_eq!(received, total, "transfer lost or duplicated data");
    assert_eq!(counters.tcp_resets, 0, "transfer was reset: {counters:?}");

    // Guest FIN completes the close promptly (no leaked connection).
    let fin = build_segment(
        40000,
        remote_port,
        1001,
        our_isn,
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
