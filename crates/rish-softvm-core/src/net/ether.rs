//! Ethernet framing and ARP handling for the network backend.
//!
//! Only the two ethertypes the backend terminates are parsed; everything
//! else is reported to the dispatcher for counting.

use std::net::Ipv4Addr;

const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

/// Splits a frame into destination MAC, raw ethertype, and payload.
/// Frames shorter than the 14-byte header fail closed; the dispatcher
/// counts unknown ethertypes itself.
pub fn parse(frame: &[u8]) -> Option<([u8; 6], u16, &[u8])> {
    if frame.len() < 14 {
        return None;
    }
    let mut dst = [0_u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    Some((dst, ethertype, &frame[14..]))
}

/// Whether a frame addressed to dst is addressed to us: the guest's own
/// MAC, the gateway MAC the backend answers for, or broadcast.
pub fn for_us(dst: [u8; 6], mac: [u8; 6], gateway_mac: [u8; 6]) -> bool {
    dst == mac || dst == gateway_mac || dst == BROADCAST_MAC
}

/// Builds an Ethernet frame: dst, src (our MAC), raw ethertype, payload.
pub fn frame(dst: [u8; 6], src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + payload.len());
    out.extend_from_slice(&dst);
    out.extend_from_slice(&src);
    out.extend_from_slice(&ethertype.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Parses an ARP request over Ethernet/IPv4 for the sender identity and the
/// target protocol address. Fails closed on truncated packets, wrong
/// hardware/protocol types or lengths, and non-request operations.
pub fn parse_arp_request(payload: &[u8]) -> Option<([u8; 6], Ipv4Addr, Ipv4Addr)> {
    if payload.len() < 28 {
        return None;
    }
    if u16::from_be_bytes([payload[0], payload[1]]) != 1 {
        return None; // hardware type: Ethernet
    }
    if u16::from_be_bytes([payload[2], payload[3]]) != 0x0800 {
        return None; // protocol type: IPv4
    }
    if payload[4] != 6 || payload[5] != 4 {
        return None; // MAC and IPv4 address lengths
    }
    if u16::from_be_bytes([payload[6], payload[7]]) != 1 {
        return None; // operation: request
    }
    let mut sender_mac = [0_u8; 6];
    sender_mac.copy_from_slice(&payload[8..14]);
    let sender_ip = Ipv4Addr::new(payload[14], payload[15], payload[16], payload[17]);
    let target_ip = Ipv4Addr::new(payload[24], payload[25], payload[26], payload[27]);
    Some((sender_mac, sender_ip, target_ip))
}

/// Builds an ARP reply: the gateway MAC owns the gateway address, answered
/// to the requestor. Returns a complete Ethernet frame with the gateway MAC
/// as both the Ethernet source and the ARP sender.
pub fn build_arp_reply(
    mac: [u8; 6],
    gateway_ip: Ipv4Addr,
    sender_mac: [u8; 6],
    sender_ip: Ipv4Addr,
) -> Option<Vec<u8>> {
    let mut arp = Vec::with_capacity(28);
    arp.extend_from_slice(&[0x00, 0x01]); // Ethernet
    arp.extend_from_slice(&[0x08, 0x00]); // IPv4
    arp.push(6);
    arp.push(4);
    arp.extend_from_slice(&[0x00, 0x02]); // reply
    arp.extend_from_slice(&mac);
    arp.extend_from_slice(&gateway_ip.octets());
    arp.extend_from_slice(&sender_mac);
    arp.extend_from_slice(&sender_ip.octets());
    Some(frame(sender_mac, mac, 0x0806, &arp))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    const GATEWAY_MAC: [u8; 6] = [0x52, 0x55, 0x0A, 0x00, 0x02, 0x02];
    const GUEST_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    const GW: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
    const GUEST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);

    fn arp_request_for(target: Ipv4Addr) -> Vec<u8> {
        let mut arp = Vec::new();
        arp.extend_from_slice(&[0x00, 0x01, 0x08, 0x00, 6, 4, 0x00, 0x01]);
        arp.extend_from_slice(&GUEST_MAC);
        arp.extend_from_slice(&GUEST.octets());
        arp.extend_from_slice(&[0_u8; 6]);
        arp.extend_from_slice(&target.octets());
        frame(MAC, GUEST_MAC, 0x0806, &arp)
    }

    #[test]
    fn parses_an_arp_request_and_builds_the_reply() {
        let request = arp_request_for(GW);
        let (dst, ethertype, payload) = parse(&request).unwrap();
        assert_eq!(ethertype, 0x0806);
        assert_eq!(dst, MAC);
        let (sender_mac, sender_ip, target) = parse_arp_request(payload).unwrap();
        assert_eq!(sender_mac, GUEST_MAC);
        assert_eq!(sender_ip, GUEST);
        assert_eq!(target, GW);

        let reply = build_arp_reply(GATEWAY_MAC, GW, sender_mac, sender_ip).unwrap();
        let (dst, ethertype, payload) = parse(&reply).unwrap();
        assert_eq!(dst, GUEST_MAC);
        assert_eq!(ethertype, 0x0806);
        assert_eq!(&payload[0..8], &[0x00, 0x01, 0x08, 0x00, 6, 4, 0x00, 0x02]);
        // The Ethernet source and the ARP sender are the gateway MAC.
        assert_eq!(&reply[6..12], &GATEWAY_MAC);
        assert_eq!(&payload[8..14], &GATEWAY_MAC);
        assert_eq!(&payload[14..18], &GW.octets());
        assert_eq!(&payload[18..24], &GUEST_MAC);
        assert_eq!(&payload[24..28], &GUEST.octets());
    }

    #[test]
    fn drops_malformed_arp() {
        assert!(parse_arp_request(&[]).is_none());
        let mut bogus = arp_request_for(GW);
        bogus[14 + 7] = 2; // operation: reply, not request
        let (_, _, payload) = parse(&bogus).unwrap();
        assert!(parse_arp_request(payload).is_none());
    }

    #[test]
    fn frame_filtering_accepts_broadcast_own_and_gateway_macs() {
        assert!(for_us(BROADCAST_MAC, MAC, GATEWAY_MAC));
        assert!(for_us(MAC, MAC, GATEWAY_MAC));
        assert!(for_us(GATEWAY_MAC, MAC, GATEWAY_MAC));
        assert!(!for_us(GUEST_MAC, MAC, GATEWAY_MAC));
        assert!(parse(&[0_u8; 13]).is_none());
        let unknown = frame(MAC, GUEST_MAC, 0x0800, &[]);
        let mut unknown = unknown;
        unknown[12] = 0x86; // IPv6 ethertype
        unknown[13] = 0xDD;
        let (_, ethertype, _) = parse(&unknown).unwrap();
        assert_eq!(ethertype, 0x86DD);
    }
}
