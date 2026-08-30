//! TCP segment wire format: parsing, assembly, checksum verification, and
//! the MSS option. Shared by the connection state machine (tcp) and its
//! tests.

use super::ipv4::{self, Ipv4Packet};

pub const FLAG_FIN: u8 = 0x01;
pub const FLAG_SYN: u8 = 0x02;
pub const FLAG_RST: u8 = 0x04;
pub const FLAG_ACK: u8 = 0x10;

/// Advertised MSS: the device MTU (1500) minus the IPv4/TCP headers.
pub const OUR_MSS: u16 = 1460;

/// One parsed TCP segment from the guest.
pub struct Segment<'a> {
    pub sport: u16,
    pub dport: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    /// Options between the fixed header and the payload.
    pub options: &'a [u8],
    pub payload: &'a [u8],
}

pub fn parse_segment(data: &[u8]) -> Option<Segment<'_>> {
    if data.len() < 20 {
        return None;
    }
    let data_offset = usize::from(data[12] >> 4) * 4;
    if data_offset < 20 || data_offset > data.len() {
        return None;
    }
    Some(Segment {
        sport: u16::from_be_bytes([data[0], data[1]]),
        dport: u16::from_be_bytes([data[2], data[3]]),
        seq: u32::from_be_bytes(data[4..8].try_into().unwrap()),
        ack: u32::from_be_bytes(data[8..12].try_into().unwrap()),
        flags: data[13],
        window: u16::from_be_bytes([data[14], data[15]]),
        options: &data[20..data_offset],
        payload: &data[data_offset..],
    })
}

/// Verifies the TCP checksum over the pseudo header the guest used.
pub fn segment_checksum_ok(packet: &Ipv4Packet<'_>, data: &[u8]) -> bool {
    let mut zeroed = data.to_vec();
    zeroed[16..18].copy_from_slice(&[0, 0]);
    ipv4::transport_checksum(packet.src, packet.dst, ipv4::PROTOCOL_TCP, &zeroed)
        == u16::from_be_bytes([data[16], data[17]])
}

/// Builds a TCP segment with the checksum field zeroed (the IPv4 wrapper
/// fills it).
#[allow(clippy::too_many_arguments)]
pub fn build_segment(
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    options: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    let data_offset = (5 + options.len() / 4) as u8;
    let mut segment = Vec::with_capacity(data_offset as usize * 4 + payload.len());
    segment.extend_from_slice(&sport.to_be_bytes());
    segment.extend_from_slice(&dport.to_be_bytes());
    segment.extend_from_slice(&seq.to_be_bytes());
    segment.extend_from_slice(&ack.to_be_bytes());
    segment.push(data_offset << 4);
    segment.push(flags);
    segment.extend_from_slice(&window.to_be_bytes());
    segment.extend_from_slice(&[0, 0]); // checksum placeholder
    segment.extend_from_slice(&[0, 0]); // urgent pointer
    segment.extend_from_slice(options);
    segment.extend_from_slice(payload);
    segment
}

/// The MSS option of a SYN, when present.
pub fn parse_mss(options: &[u8]) -> Option<u16> {
    let mut index = 0;
    while index < options.len() {
        match options[index] {
            0 => return None, // end of options
            1 => index += 1,  // no-op
            2 => {
                if index + 4 <= options.len() && options[index + 1] == 4 {
                    return Some(u16::from_be_bytes([options[index + 2], options[index + 3]]));
                }
                return None;
            }
            _kind => {
                let len = usize::from(options.get(index + 1).copied().unwrap_or(0));
                if len < 2 || index + len > options.len() {
                    return None;
                }
                index += len;
            }
        }
    }
    None
}
