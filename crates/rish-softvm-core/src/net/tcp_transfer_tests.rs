use super::*;

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
    let frames = poll_until(
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
