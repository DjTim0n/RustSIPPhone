//! Minimal RTP packet implementation (RFC 3550): 12-byte header + payload.

pub const RTP_VERSION: u8 = 2;
const HEADER_LEN: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtpHeader {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

pub fn build_packet(header: &RtpHeader, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER_LEN + payload.len());
    packet.push(RTP_VERSION << 6);
    packet.push((header.marker as u8) << 7 | (header.payload_type & 0x7F));
    packet.extend_from_slice(&header.sequence.to_be_bytes());
    packet.extend_from_slice(&header.timestamp.to_be_bytes());
    packet.extend_from_slice(&header.ssrc.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

/// Parses a packet. Handles CSRC, header extension and padding.
/// Returns `None` if this is not RTP version 2 or the packet is truncated.
pub fn parse_packet(buf: &[u8]) -> Option<(RtpHeader, &[u8])> {
    if buf.len() < HEADER_LEN || buf[0] >> 6 != RTP_VERSION {
        return None;
    }
    let has_padding = buf[0] & 0x20 != 0;
    let has_extension = buf[0] & 0x10 != 0;
    let csrc_count = (buf[0] & 0x0F) as usize;
    let header = RtpHeader {
        marker: buf[1] & 0x80 != 0,
        payload_type: buf[1] & 0x7F,
        sequence: u16::from_be_bytes([buf[2], buf[3]]),
        timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
    };

    let mut offset = HEADER_LEN + csrc_count * 4;
    if has_extension {
        if buf.len() < offset + 4 {
            return None;
        }
        let words = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]) as usize;
        offset += 4 + words * 4;
    }
    let mut end = buf.len();
    if has_padding {
        end = end.checked_sub(*buf.last()? as usize)?;
    }
    if offset > end {
        return None;
    }
    Some((header, &buf[offset..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_parse() {
        let header = RtpHeader {
            payload_type: 8,
            marker: true,
            sequence: 513,
            timestamp: 160_000,
            ssrc: 0xDEADBEEF,
        };
        let packet = build_packet(&header, &[1, 2, 3]);
        assert_eq!(packet.len(), 15);
        let (parsed, payload) = parse_packet(&packet).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(payload, &[1, 2, 3]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_packet(&[0; 4]).is_none());
        assert!(parse_packet(&[0x40; 20]).is_none());
    }

    #[test]
    fn skips_csrc_and_extension() {
        let mut packet = vec![RTP_VERSION << 6 | 0x10 | 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        packet.extend_from_slice(&[0, 0, 0, 7]); // one CSRC
        packet.extend_from_slice(&[0xBE, 0xDE, 0, 1, 9, 9, 9, 9]); // extension of 1 word
        packet.extend_from_slice(&[42, 43]);
        let (_, payload) = parse_packet(&packet).unwrap();
        assert_eq!(payload, &[42, 43]);
    }
}
