//! Building SDP and parsing the other side's SDP (one G.711 audio stream + DTMF per RFC 4733).

use crate::g711::Codec;
use std::net::{IpAddr, SocketAddr};

/// Payload type for DTMF that we offer ourselves.
pub const DEFAULT_DTMF_PT: u8 = 101;

/// Which way audio flows in a session: how calls are put on hold (RFC 3264).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaDirection {
    #[default]
    SendRecv,
    SendOnly,
    RecvOnly,
    Inactive,
}

impl MediaDirection {
    fn attribute(self) -> &'static str {
        match self {
            MediaDirection::SendRecv => "sendrecv",
            MediaDirection::SendOnly => "sendonly",
            MediaDirection::RecvOnly => "recvonly",
            MediaDirection::Inactive => "inactive",
        }
    }

    fn from_attribute(text: &str) -> Option<MediaDirection> {
        match text {
            "sendrecv" => Some(MediaDirection::SendRecv),
            "sendonly" => Some(MediaDirection::SendOnly),
            "recvonly" => Some(MediaDirection::RecvOnly),
            "inactive" => Some(MediaDirection::Inactive),
            _ => None,
        }
    }

    /// What to answer when the other side offers `self`: the mirror image.
    pub fn answer(self) -> MediaDirection {
        match self {
            MediaDirection::SendRecv => MediaDirection::SendRecv,
            MediaDirection::SendOnly => MediaDirection::RecvOnly,
            MediaDirection::RecvOnly => MediaDirection::SendOnly,
            MediaDirection::Inactive => MediaDirection::Inactive,
        }
    }

    /// Whether the other side, offering `self`, is not receiving our audio (it put us on hold).
    pub fn puts_us_on_hold(self) -> bool {
        matches!(self, MediaDirection::SendOnly | MediaDirection::Inactive)
    }
}

/// Everything that goes into a session description of ours.
pub struct LocalSdp<'a> {
    pub ip: IpAddr,
    pub rtp_port: u16,
    pub session_id: u64,
    /// Must grow with every new description sent in the same session (RFC 3264).
    pub version: u64,
    /// In order of preference.
    pub codecs: &'a [Codec],
    /// Payload type for telephone-event, if tones are supported.
    pub dtmf_pt: Option<u8>,
    pub direction: MediaDirection,
}

pub fn build_sdp(sdp: &LocalSdp) -> String {
    let LocalSdp {
        ip,
        rtp_port,
        session_id,
        version,
        codecs,
        dtmf_pt,
        direction,
    } = sdp;
    let mut formats: Vec<String> = codecs
        .iter()
        .map(|c| c.payload_type().to_string())
        .collect();
    if let Some(pt) = dtmf_pt {
        formats.push(pt.to_string());
    }
    let mut lines = vec![
        "v=0".to_string(),
        format!("o=rustphone {session_id} {version} IN IP4 {ip}"),
        "s=RustSIPPhone".to_string(),
        format!("c=IN IP4 {ip}"),
        "t=0 0".to_string(),
        format!("m=audio {rtp_port} RTP/AVP {}", formats.join(" ")),
    ];
    for codec in *codecs {
        lines.push(match codec {
            Codec::Pcmu => "a=rtpmap:0 PCMU/8000".to_string(),
            Codec::Pcma => "a=rtpmap:8 PCMA/8000".to_string(),
        });
    }
    if let Some(pt) = dtmf_pt {
        lines.push(format!("a=rtpmap:{pt} telephone-event/8000"));
        lines.push(format!("a=fmtp:{pt} 0-15"));
    }
    lines.push("a=ptime:20".to_string());
    lines.push(format!("a={}", direction.attribute()));
    let mut text = lines.join("\r\n");
    text.push_str("\r\n");
    text
}

pub fn build_offer(local_ip: IpAddr, rtp_port: u16, session_id: u64) -> String {
    build_sdp(&LocalSdp {
        ip: local_ip,
        rtp_port,
        session_id,
        version: session_id,
        codecs: &[Codec::Pcmu, Codec::Pcma],
        dtmf_pt: Some(DEFAULT_DTMF_PT),
        direction: MediaDirection::SendRecv,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub addr: SocketAddr,
    pub codec: Codec,
    pub dtmf_pt: Option<u8>,
    pub direction: MediaDirection,
}

impl Remote {
    /// Whether the other side has put the call on hold: it stops receiving our audio, either by
    /// the direction attribute or by the old convention of an all-zero address.
    pub fn on_hold(&self) -> bool {
        self.direction.puts_us_on_hold() || self.addr.ip().is_unspecified()
    }
}

/// Parses the other side's SDP (both the answer to our offer and its offer on an incoming call).
pub fn parse_remote(sdp: &str) -> Result<Remote, String> {
    let mut session_ip: Option<IpAddr> = None;
    let mut media_ip: Option<IpAddr> = None;
    let mut media: Option<(u16, Vec<u8>)> = None;
    let mut dtmf_pt: Option<u8> = None;
    let mut session_direction = None;
    let mut media_direction = None;
    let mut in_audio = false;

    for line in sdp.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("m=") {
            in_audio = media.is_none() && rest.starts_with("audio ");
            if in_audio {
                let mut parts = rest.split_whitespace().skip(1);
                let port = parts
                    .next()
                    .and_then(|p| p.split('/').next())
                    .and_then(|p| p.parse::<u16>().ok())
                    .ok_or("invalid port in m=audio")?;
                let formats = parts.skip(1).filter_map(|f| f.parse::<u8>().ok()).collect();
                media = Some((port, formats));
            }
        } else if let Some(rest) = line.strip_prefix("c=") {
            let ip = rest
                .split_whitespace()
                .nth(2)
                .and_then(|a| a.split('/').next())
                .and_then(|a| a.parse::<IpAddr>().ok());
            if in_audio {
                media_ip = ip;
            } else if media.is_none() {
                session_ip = ip;
            }
        } else if let Some(attribute) = line
            .strip_prefix("a=")
            .and_then(MediaDirection::from_attribute)
        {
            // A direction in the audio section wins over one for the whole session.
            if in_audio {
                media_direction = Some(attribute);
            } else if media.is_none() {
                session_direction = Some(attribute);
            }
        } else if in_audio {
            if let Some(rest) = line.strip_prefix("a=rtpmap:") {
                let mut parts = rest.split_whitespace();
                let pt = parts.next().and_then(|p| p.parse::<u8>().ok());
                let name = parts.next().unwrap_or("").to_ascii_lowercase();
                if name.starts_with("telephone-event/8000") {
                    dtmf_pt = pt;
                }
            }
        }
    }

    let (port, formats) = media.ok_or("the SDP has no audio stream")?;
    if port == 0 {
        return Err("the other side rejected audio (port 0)".into());
    }
    let ip = media_ip
        .or(session_ip)
        .ok_or("the SDP has no address (c=)")?;
    let codec = formats
        .iter()
        .find_map(|&pt| Codec::from_payload_type(pt))
        .ok_or("no common codec: only PCMU and PCMA are supported")?;
    Ok(Remote {
        addr: SocketAddr::new(ip, port),
        codec,
        dtmf_pt,
        direction: media_direction.or(session_direction).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_contains_media_and_dtmf() {
        let sdp = build_offer("192.168.1.5".parse().unwrap(), 40000, 1);
        assert!(sdp.contains("m=audio 40000 RTP/AVP 0 8 101\r\n"));
        assert!(sdp.contains("c=IN IP4 192.168.1.5\r\n"));
        assert!(sdp.contains("a=rtpmap:101 telephone-event/8000\r\n"));
    }

    #[test]
    fn answer_lists_only_chosen_codec() {
        let sdp = build_sdp(&LocalSdp {
            ip: "10.0.0.2".parse().unwrap(),
            rtp_port: 5004,
            session_id: 7,
            version: 7,
            codecs: &[Codec::Pcma],
            dtmf_pt: Some(96),
            direction: MediaDirection::SendRecv,
        });
        assert!(sdp.contains("m=audio 5004 RTP/AVP 8 96\r\n"));
        assert!(!sdp.contains("PCMU"));
        assert!(sdp.contains("a=rtpmap:96 telephone-event/8000\r\n"));
    }

    #[test]
    fn hold_offer_says_sendonly_and_the_version_grows() {
        let hold = build_sdp(&LocalSdp {
            ip: "10.0.0.2".parse().unwrap(),
            rtp_port: 5004,
            session_id: 7,
            version: 9,
            codecs: &[Codec::Pcmu],
            dtmf_pt: None,
            direction: MediaDirection::SendOnly,
        });
        assert!(hold.contains("o=rustphone 7 9 IN IP4 10.0.0.2\r\n"));
        assert!(hold.contains("a=sendonly\r\n"));
        assert!(!hold.contains("a=sendrecv"));
    }

    #[test]
    fn directions_are_read_and_answered_by_mirroring() {
        let sdp = "c=IN IP4 1.1.1.1\nm=audio 5000 RTP/AVP 0\na=sendonly\n";
        let remote = parse_remote(sdp).unwrap();
        assert_eq!(remote.direction, MediaDirection::SendOnly);
        assert!(remote.on_hold(), "sendonly means they put us on hold");
        assert_eq!(remote.direction.answer(), MediaDirection::RecvOnly);
        assert_eq!(MediaDirection::Inactive.answer(), MediaDirection::Inactive);
        assert_eq!(MediaDirection::SendRecv.answer(), MediaDirection::SendRecv);
    }

    #[test]
    fn media_direction_beats_the_session_direction() {
        let sdp = "c=IN IP4 1.1.1.1\na=sendonly\nm=audio 5000 RTP/AVP 0\na=sendrecv\n";
        assert_eq!(
            parse_remote(sdp).unwrap().direction,
            MediaDirection::SendRecv
        );
    }

    #[test]
    fn a_zero_address_is_the_old_style_of_hold() {
        let sdp = "c=IN IP4 0.0.0.0\nm=audio 5000 RTP/AVP 0\n";
        assert!(parse_remote(sdp).unwrap().on_hold());
        let normal = "c=IN IP4 1.1.1.1\nm=audio 5000 RTP/AVP 0\na=recvonly\n";
        assert!(
            !parse_remote(normal).unwrap().on_hold(),
            "recvonly is not a hold"
        );
    }

    #[test]
    fn parses_asterisk_style_answer() {
        let sdp = "v=0\r\no=- 1 1 IN IP4 217.11.77.232\r\ns=Asterisk\r\nc=IN IP4 217.11.77.232\r\nt=0 0\r\n\
                   m=audio 14232 RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\n";
        let remote = parse_remote(sdp).unwrap();
        assert_eq!(remote.addr, "217.11.77.232:14232".parse().unwrap());
        assert_eq!(remote.codec, Codec::Pcma);
        assert_eq!(remote.dtmf_pt, Some(101));
    }

    #[test]
    fn dtmf_absent_when_not_offered() {
        let sdp = "c=IN IP4 1.1.1.1\nm=audio 5000 RTP/AVP 0\na=rtpmap:0 PCMU/8000\n";
        assert_eq!(parse_remote(sdp).unwrap().dtmf_pt, None);
    }

    #[test]
    fn media_level_address_wins() {
        let sdp = "v=0\nc=IN IP4 1.1.1.1\nm=audio 5000 RTP/AVP 0\nc=IN IP4 2.2.2.2\n";
        assert_eq!(
            parse_remote(sdp).unwrap().addr,
            "2.2.2.2:5000".parse().unwrap()
        );
    }

    #[test]
    fn rejects_unsupported_codec_and_disabled_media() {
        assert!(parse_remote("c=IN IP4 1.1.1.1\nm=audio 5000 RTP/AVP 18\n").is_err());
        assert!(parse_remote("c=IN IP4 1.1.1.1\nm=audio 0 RTP/AVP 0\n").is_err());
        assert!(parse_remote("v=0\n").is_err());
    }
}
