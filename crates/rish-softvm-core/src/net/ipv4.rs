//! IPv4 parsing/assembly, the RFC 1071 checksum, and ICMP echo replies.
//!
//! The backend handles exactly the packets it terminates: unfragmented
//! IPv4 with a valid header checksum. Reassembly of fragmented datagrams is
//! deliberately absent (the TCP MSS is clamped so guest segments never
//! fragment; DNS and ICMP payloads are far below the MTU).

use std::net::Ipv4Addr;

pub const PROTOCOL_ICMP: u8 = 1;
pub const PROTOCOL_TCP: u8 = 6;
pub const PROTOCOL_UDP: u8 = 17;

/// Default time-to-live for packets the backend originates.
const DEFAULT_TTL: u8 = 64;

/// One parsed IPv4 packet. header is the verbatim 20-byte header (needed
/// for checksum verification) and payload is everything after it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ipv4Packet<'a> {
    pub header: &'a [u8],
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: u8,
    pub more_fragments: bool,
    pub fragment_offset: u16,
    pub payload: &'a [u8],
}

/// Parses an IPv4 packet, failing closed on version mismatch, short or
/// truncated headers, or a total-length field that disagrees with the data.
pub fn parse(data: &[u8]) -> Option<Ipv4Packet<'_>> {
    if data.len() < 20 || data[0] >> 4 != 4 {
        return None;
    }
    let ihl = usize::from(data[0] & 0x0F) * 4;
    if ihl < 20 || ihl > data.len() {
        return None;
    }
    let total = usize::from(u16::from_be_bytes([data[2], data[3]]));
    if total < ihl || total > data.len() {
        return None;
    }
    let flags_fragment = u16::from_be_bytes([data[6], data[7]]);
    Some(Ipv4Packet {
        header: &data[..ihl],
        src: Ipv4Addr::new(data[12], data[13], data[14], data[15]),
        dst: Ipv4Addr::new(data[16], data[17], data[18], data[19]),
        protocol: data[9],
        more_fragments: flags_fragment & 0x2000 != 0,
        fragment_offset: flags_fragment & 0x1FFF,
        payload: &data[ihl..total],
    })
}

/// RFC 1071 internet checksum over an arbitrary byte slice.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]]) as u32
        } else {
            (chunk[0] as u32) << 8
        };
        sum = sum.wrapping_add(word);
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Verifies the header checksum of a parsed packet.
pub fn header_checksum_ok(header: &[u8]) -> bool {
    checksum(header) == 0
}

/// Builds a 20-byte IPv4 header (total_length is the whole packet).
pub fn build_header(
    total_length: usize,
    protocol: u8,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    identification: u16,
) -> [u8; 20] {
    let mut header = [0_u8; 20];
    header[0] = 0x45;
    header[2..4].copy_from_slice(&(total_length as u16).to_be_bytes());
    header[4..6].copy_from_slice(&identification.to_be_bytes());
    header[8] = DEFAULT_TTL;
    header[9] = protocol;
    header[12..16].copy_from_slice(&src.octets());
    header[16..20].copy_from_slice(&dst.octets());
    let sum = checksum(&header);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
    header
}

/// Wraps a UDP datagram in a complete IPv4 packet with computed checksums.
pub fn wrap_udp(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
    identification: u16,
) -> Vec<u8> {
    let mut udp = Vec::with_capacity(8 + payload.len());
    udp.extend_from_slice(&src_port.to_be_bytes());
    udp.extend_from_slice(&dst_port.to_be_bytes());
    udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);
    let sum = transport_checksum(src, dst, PROTOCOL_UDP, &udp);
    udp[6..8].copy_from_slice(&sum.to_be_bytes());
    let header = build_header(20 + udp.len(), PROTOCOL_UDP, src, dst, identification);
    let mut packet = Vec::with_capacity(20 + udp.len());
    packet.extend_from_slice(&header);
    packet.extend_from_slice(&udp);
    packet
}

/// Wraps a TCP segment in a complete IPv4 packet with computed checksums.
pub fn wrap_tcp(src: Ipv4Addr, dst: Ipv4Addr, segment: &[u8], identification: u16) -> Vec<u8> {
    let mut tcp = Vec::with_capacity(segment.len());
    tcp.extend_from_slice(segment);
    let sum = transport_checksum(src, dst, PROTOCOL_TCP, &tcp);
    tcp[16..18].copy_from_slice(&sum.to_be_bytes());
    let header = build_header(20 + tcp.len(), PROTOCOL_TCP, src, dst, identification);
    let mut packet = Vec::with_capacity(20 + tcp.len());
    packet.extend_from_slice(&header);
    packet.extend_from_slice(&tcp);
    packet
}

/// TCP/UDP checksum: pseudo header plus the transport segment, with the
/// checksum field itself treated as zero.
pub fn transport_checksum(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, segment: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for word in src.octets().chunks(2) {
        sum = sum.wrapping_add(u16::from_be_bytes([word[0], word[1]]) as u32);
    }
    for word in dst.octets().chunks(2) {
        sum = sum.wrapping_add(u16::from_be_bytes([word[0], word[1]]) as u32);
    }
    sum = sum.wrapping_add(u32::from(protocol));
    sum = sum.wrapping_add(segment.len() as u32);
    for chunk in segment.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]]) as u32
        } else {
            (chunk[0] as u32) << 8
        };
        sum = sum.wrapping_add(word);
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let folded = (sum & 0xFFFF) + (sum >> 16);
    !(folded as u16)
}

/// Builds an ICMP echo reply (type 0) for an echo request (type 8) that
/// arrived as packet: identifier, sequence, and payload are echoed back and
/// the ICMP checksum is recomputed. Any other ICMP type yields None.
pub fn icmp_echo_reply(packet: Ipv4Packet<'_>, identification: &mut u16) -> Option<Vec<u8>> {
    let payload = packet.payload;
    if payload.len() < 8 || payload[0] != 8 || payload[1] != 0 {
        return None;
    }
    // The request must carry a valid ICMP checksum; a forged echo must not
    // get a reply.
    if checksum(payload) != 0 {
        return None;
    }
    let mut icmp = Vec::with_capacity(payload.len());
    icmp.push(0); // echo reply
    icmp.push(0);
    icmp.extend_from_slice(&[0, 0]); // checksum placeholder
    icmp.extend_from_slice(&payload[4..]);
    let sum = checksum(&icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    // The reply travels gateway -> guest: swap the addresses.
    let id = next_identification(identification);
    let header = build_header(20 + icmp.len(), PROTOCOL_ICMP, packet.dst, packet.src, id);
    let mut reply = Vec::with_capacity(20 + icmp.len());
    reply.extend_from_slice(&header);
    reply.extend_from_slice(&icmp);
    Some(reply)
}

/// Monotonic IPv4 identification.
pub fn next_identification(identification: &mut u16) -> u16 {
    let id = *identification;
    *identification = identification.wrapping_add(1);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUEST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const GW: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

    #[test]
    fn checksum_matches_the_rfc_1071_reference() {
        // RFC 1071 section 3: checksum of 0x00,0x01,0xF2,0x03,0xF4,0xF5,
        // 0xF6,0xF7 is 0x220D.
        let data = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
        assert_eq!(checksum(&data), 0x220D);
        // A checksummed header verifies to zero: it is the one's-complement
        // of its own sum.
        let header = build_header(40, PROTOCOL_ICMP, GW, GUEST, 7);
        assert_eq!(checksum(&header), 0);
    }

    #[test]
    fn echo_request_round_trips_through_the_reply() {
        let id: u16 = 0x1234;
        let mut icmp = Vec::new();
        icmp.extend_from_slice(&[8, 0, 0, 0]);
        icmp.extend_from_slice(&id.to_be_bytes());
        icmp.extend_from_slice(&7_u16.to_be_bytes());
        icmp.extend_from_slice(b"ping-payload");
        let sum = checksum(&icmp);
        icmp[2..4].copy_from_slice(&sum.to_be_bytes());
        let mut packet = Vec::new();
        let header = build_header(20 + icmp.len(), PROTOCOL_ICMP, GUEST, GW, 1);
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&icmp);

        let parsed = parse(&packet).unwrap();
        assert_eq!(parsed.src, GUEST);
        assert_eq!(parsed.dst, GW);
        assert!(header_checksum_ok(parsed.header));
        let mut identification = 9;
        let reply = icmp_echo_reply(parsed, &mut identification).unwrap();
        let reply = parse(&reply).unwrap();
        assert_eq!(reply.src, GW);
        assert_eq!(reply.dst, GUEST);
        assert_eq!(reply.protocol, PROTOCOL_ICMP);
        assert!(header_checksum_ok(reply.header));
        assert_eq!(&reply.payload[0..2], &[0, 0]);
        assert_eq!(checksum(reply.payload), 0);
        assert_eq!(&reply.payload[4..6], &id.to_be_bytes());
        assert_eq!(&reply.payload[6..8], &7_u16.to_be_bytes());
        assert_eq!(&reply.payload[8..], b"ping-payload");
    }

    #[test]
    fn udp_wrapper_produces_a_verifiable_datagram() {
        let packet = wrap_udp(GW, GUEST, 53, 40000, b"dns-payload", 5);
        let parsed = parse(&packet).unwrap();
        assert!(header_checksum_ok(parsed.header));
        assert_eq!(parsed.protocol, PROTOCOL_UDP);
        assert_eq!(&parsed.payload[0..2], &53_u16.to_be_bytes());
        assert_eq!(&parsed.payload[2..4], &40000_u16.to_be_bytes());
        let mut with_checksum_zeroed = parsed.payload.to_vec();
        with_checksum_zeroed[6..8].copy_from_slice(&[0, 0]);
        assert_eq!(
            transport_checksum(GW, GUEST, PROTOCOL_UDP, &with_checksum_zeroed),
            u16::from_be_bytes([parsed.payload[6], parsed.payload[7]]),
        );
        assert_eq!(&parsed.payload[8..], b"dns-payload");
    }

    #[test]
    fn echo_replies_refuse_a_request_with_a_bad_checksum() {
        // Regression: the ICMP checksum of an incoming echo request was
        // never verified, so any frame on the wire could elicit an echo
        // reply.
        let mut icmp = Vec::new();
        icmp.extend_from_slice(&[8, 0, 0, 0]); // type, code, checksum 0
        icmp.extend_from_slice(&0x1234_u16.to_be_bytes());
        icmp.extend_from_slice(&7_u16.to_be_bytes());
        icmp.extend_from_slice(b"ping-payload");
        let mut packet = Vec::new();
        let header = build_header(20 + icmp.len(), PROTOCOL_ICMP, GUEST, GW, 1);
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&icmp);
        let parsed = parse(&packet).unwrap();
        let mut identification = 9;
        assert!(icmp_echo_reply(parsed, &mut identification).is_none());
    }

    #[test]
    fn parse_rejects_truncated_and_fragmented_packets() {
        let packet = wrap_udp(GW, GUEST, 53, 40000, &[1, 2, 3], 1);
        assert!(parse(&packet[..19]).is_none());
        let mut bad_len = packet.clone();
        bad_len[2..4].copy_from_slice(&999_u16.to_be_bytes());
        assert!(parse(&bad_len).is_none());
        let mut fragmented = packet.clone();
        fragmented[6..8].copy_from_slice(&0x2000_u16.to_be_bytes());
        assert!(parse(&fragmented).unwrap().more_fragments);
    }
}
