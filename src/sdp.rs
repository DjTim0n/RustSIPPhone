//! Построение SDP и разбор SDP собеседника (один аудио-поток G.711 + DTMF по RFC 4733).

use crate::g711::Codec;
use std::net::{IpAddr, SocketAddr};

/// Payload type для DTMF, который мы предлагаем сами.
pub const DEFAULT_DTMF_PT: u8 = 101;

/// Собирает SDP. `codecs` — в порядке предпочтения, `dtmf_pt` — номер для telephone-event.
pub fn build_sdp(
    local_ip: IpAddr,
    rtp_port: u16,
    session_id: u64,
    codecs: &[Codec],
    dtmf_pt: Option<u8>,
) -> String {
    let mut formats: Vec<String> = codecs.iter().map(|c| c.payload_type().to_string()).collect();
    if let Some(pt) = dtmf_pt {
        formats.push(pt.to_string());
    }
    let mut lines = vec![
        "v=0".to_string(),
        format!("o=rustphone {session_id} {session_id} IN IP4 {local_ip}"),
        "s=RustSIPPhone".to_string(),
        format!("c=IN IP4 {local_ip}"),
        "t=0 0".to_string(),
        format!("m=audio {rtp_port} RTP/AVP {}", formats.join(" ")),
    ];
    for codec in codecs {
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
    lines.push("a=sendrecv".to_string());
    let mut sdp = lines.join("\r\n");
    sdp.push_str("\r\n");
    sdp
}

pub fn build_offer(local_ip: IpAddr, rtp_port: u16, session_id: u64) -> String {
    build_sdp(
        local_ip,
        rtp_port,
        session_id,
        &[Codec::Pcmu, Codec::Pcma],
        Some(DEFAULT_DTMF_PT),
    )
}

#[derive(Debug, PartialEq, Eq)]
pub struct Remote {
    pub addr: SocketAddr,
    pub codec: Codec,
    pub dtmf_pt: Option<u8>,
}

/// Разбирает SDP собеседника (и ответ на наш оффер, и его оффер на входящем звонке).
pub fn parse_remote(sdp: &str) -> Result<Remote, String> {
    let mut session_ip: Option<IpAddr> = None;
    let mut media_ip: Option<IpAddr> = None;
    let mut media: Option<(u16, Vec<u8>)> = None;
    let mut dtmf_pt: Option<u8> = None;
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
                    .ok_or("некорректный порт в m=audio")?;
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

    let (port, formats) = media.ok_or("в SDP нет аудио-потока")?;
    if port == 0 {
        return Err("собеседник отклонил аудио (порт 0)".into());
    }
    let ip = media_ip.or(session_ip).ok_or("в SDP нет адреса (c=)")?;
    let codec = formats
        .iter()
        .find_map(|&pt| Codec::from_payload_type(pt))
        .ok_or("нет общего кодека: поддерживаются только PCMU и PCMA")?;
    Ok(Remote {
        addr: SocketAddr::new(ip, port),
        codec,
        dtmf_pt,
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
        let sdp = build_sdp("10.0.0.2".parse().unwrap(), 5004, 7, &[Codec::Pcma], Some(96));
        assert!(sdp.contains("m=audio 5004 RTP/AVP 8 96\r\n"));
        assert!(!sdp.contains("PCMU"));
        assert!(sdp.contains("a=rtpmap:96 telephone-event/8000\r\n"));
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
