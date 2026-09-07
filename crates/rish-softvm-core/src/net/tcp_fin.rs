use super::*;

pub(super) fn maybe_send_fin(
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
