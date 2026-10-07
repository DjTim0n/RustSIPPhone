//! RTP session: microphone -> G.711 -> UDP and UDP -> G.711 -> speaker, plus DTMF (RFC 4733).

use crate::audio::{AudioIo, SpeakerQueue};
use crate::g711::Codec;
use crate::rtp::{self, RtpHeader};
use crate::sdp::Remote;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// 20 ms at 8 kHz.
const FRAME_SAMPLES: usize = 160;
/// How many 20 ms packets one digit takes (200 ms); the last three carry the end flag.
const DTMF_PACKETS: u16 = 10;
const DTMF_END_PACKETS: u16 = 3;

#[derive(Debug, Default)]
pub struct MediaStats {
    pub received: u64,
}

/// What the core uses to steer a call while it is in progress.
pub struct MediaControl {
    pub muted: Arc<AtomicBool>,
    pub dtmf: mpsc::UnboundedReceiver<char>,
}

/// Runs media until `stop` fires. Owns the audio devices: they are closed when this returns.
pub async fn run(
    socket: Arc<UdpSocket>,
    remote: Remote,
    mut audio: AudioIo,
    control: MediaControl,
    signalling_ips: Vec<IpAddr>,
    stop: CancellationToken,
) -> MediaStats {
    // Audio captured while dialling is not needed.
    while audio.mic.try_recv().is_ok() {}

    let (remote_tx, remote_rx) = watch::channel(remote.addr);
    let speaker = audio.speaker.clone();
    let codec = remote.codec;
    let dtmf_pt = remote.dtmf_pt;

    let filter = PeerFilter::new(remote.addr.ip(), &signalling_ips);
    let ((), received) = tokio::join!(
        send_loop(
            &socket,
            codec,
            dtmf_pt,
            &mut audio.mic,
            control,
            remote_rx,
            &stop
        ),
        receive_loop(&socket, codec, speaker, remote_tx, filter, &stop),
    );
    MediaStats { received }
}

struct DtmfState {
    event: u8,
    start_timestamp: u32,
    packets_sent: u16,
}

fn dtmf_event_code(digit: char) -> Option<u8> {
    match digit {
        '0'..='9' => Some(digit as u8 - b'0'),
        '*' => Some(10),
        '#' => Some(11),
        'A'..='D' => Some(digit as u8 - b'A' + 12),
        'a'..='d' => Some(digit as u8 - b'a' + 12),
        _ => None,
    }
}

async fn send_loop(
    socket: &UdpSocket,
    codec: Codec,
    dtmf_pt: Option<u8>,
    mic: &mut mpsc::Receiver<Vec<i16>>,
    mut control: MediaControl,
    remote: watch::Receiver<SocketAddr>,
    stop: &CancellationToken,
) {
    let mut rng = XorShift::seeded();
    let ssrc = rng.next() as u32;
    let mut sequence = rng.next() as u16;
    let mut timestamp = rng.next() as u32;
    let mut marker = true;
    let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);
    let mut digits: std::collections::VecDeque<u8> = Default::default();
    let mut dtmf: Option<DtmfState> = None;

    loop {
        let chunk = tokio::select! {
            _ = stop.cancelled() => break,
            digit = control.dtmf.recv() => {
                if let (Some(event), Some(_)) = (digit.and_then(dtmf_event_code), dtmf_pt) {
                    digits.push_back(event);
                }
                continue;
            }
            chunk = mic.recv() => match chunk {
                Some(chunk) => chunk,
                None => break,
            },
        };
        pending.extend_from_slice(&chunk);
        while pending.len() >= FRAME_SAMPLES {
            let mut frame: Vec<i16> = pending.drain(..FRAME_SAMPLES).collect();
            if control.muted.load(Ordering::Relaxed) {
                frame.fill(0);
            }

            if dtmf.is_none() {
                if let Some(event) = digits.pop_front() {
                    dtmf = Some(DtmfState {
                        event,
                        start_timestamp: timestamp,
                        packets_sent: 0,
                    });
                }
            }

            let (header, payload) = match (&mut dtmf, dtmf_pt) {
                // While a digit is being sent, event packets replace the audio.
                (Some(state), Some(pt)) => {
                    let n = state.packets_sent;
                    let end = n + DTMF_END_PACKETS >= DTMF_PACKETS;
                    let duration = (n + 1) * FRAME_SAMPLES as u16;
                    let payload = vec![
                        state.event,
                        (end as u8) << 7 | 10,
                        (duration >> 8) as u8,
                        duration as u8,
                    ];
                    let header = RtpHeader {
                        payload_type: pt,
                        marker: n == 0,
                        sequence,
                        timestamp: state.start_timestamp,
                        ssrc,
                    };
                    state.packets_sent += 1;
                    if state.packets_sent >= DTMF_PACKETS {
                        dtmf = None;
                        marker = true; // audio starts "afresh" after the event
                    }
                    (header, payload)
                }
                _ => {
                    let header = RtpHeader {
                        payload_type: codec.payload_type(),
                        marker,
                        sequence,
                        timestamp,
                        ssrc,
                    };
                    marker = false;
                    (header, codec.encode(&frame))
                }
            };

            let destination = *remote.borrow();
            let _ = socket
                .send_to(&rtp::build_packet(&header, &payload), destination)
                .await;
            sequence = sequence.wrapping_add(1);
            timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
        }
    }
}

async fn receive_loop(
    socket: &UdpSocket,
    codec: Codec,
    speaker: SpeakerQueue,
    remote: watch::Sender<SocketAddr>,
    mut filter: PeerFilter,
    stop: &CancellationToken,
) -> u64 {
    let mut buf = [0u8; 2048];
    let mut received = 0u64;
    let started = Instant::now();

    loop {
        let (len, source) = tokio::select! {
            _ = stop.cancelled() => break,
            result = socket.recv_from(&mut buf) => match result {
                Ok(pair) => pair,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            },
        };
        let Some((header, payload)) = rtp::parse_packet(&buf[..len]) else {
            continue;
        };
        if header.payload_type != codec.payload_type() {
            continue; // DTMF, comfort noise, etc. are not played
        }
        match filter.check(source, &header, started.elapsed()) {
            Verdict::Drop => continue,
            Verdict::Accept { latch } => {
                // Symmetric RTP: the first accepted packet decides where our audio goes
                // (a NAT usually rewrites only the port). It never changes afterwards.
                if latch && *remote.borrow() != source {
                    let _ = remote.send(source);
                }
            }
        }
        received += 1;
        let samples = codec.decode(payload);
        speaker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(&samples);
    }
    received
}

/// How long after the call starts we may learn the peer's address from the media itself
/// when the SDP advertised an unroutable (private) address.
const NAT_LEARN_WINDOW: Duration = Duration::from_secs(10);
/// Consecutive in-sequence packets required before trusting an unexpected source.
const NAT_LEARN_PACKETS: u32 = 5;

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Drop,
    /// `latch` is true only for the very first accepted packet.
    Accept {
        latch: bool,
    },
}

struct Candidate {
    source: SocketAddr,
    ssrc: u32,
    last_sequence: u16,
    count: u32,
}

/// Decides which incoming RTP packets are allowed to reach the speaker.
///
/// Plain RTP carries no authentication, so we limit what a third party can do:
/// * packets are accepted only from the IPs we negotiated with (the SDP address
///   and the signalling server, which usually hosts the media relay);
/// * the first accepted packet pins source address and SSRC for the rest of the call;
/// * only if the SDP address is private (the peer is behind NAT) do we learn another
///   address, and then only from a short, strictly sequential burst early in the call.
///
/// This does not stop an attacker who can spoof a trusted IP; that needs SRTP.
struct PeerFilter {
    trusted: Vec<IpAddr>,
    allow_nat_learning: bool,
    pinned: Option<(SocketAddr, u32)>,
    candidate: Option<Candidate>,
}

impl PeerFilter {
    fn new(sdp_ip: IpAddr, signalling_ips: &[IpAddr]) -> Self {
        let mut trusted = vec![sdp_ip];
        trusted.extend_from_slice(signalling_ips);
        PeerFilter {
            trusted,
            allow_nat_learning: !is_publicly_routable(sdp_ip),
            pinned: None,
            candidate: None,
        }
    }

    fn check(&mut self, source: SocketAddr, header: &RtpHeader, elapsed: Duration) -> Verdict {
        if let Some((addr, ssrc)) = self.pinned {
            return if source == addr && header.ssrc == ssrc {
                Verdict::Accept { latch: false }
            } else {
                Verdict::Drop
            };
        }
        if self.trusted.contains(&source.ip()) {
            self.pinned = Some((source, header.ssrc));
            return Verdict::Accept { latch: true };
        }
        if !self.allow_nat_learning || elapsed > NAT_LEARN_WINDOW {
            return Verdict::Drop;
        }
        let continues = matches!(&self.candidate, Some(c)
            if c.source == source
                && c.ssrc == header.ssrc
                && header.sequence == c.last_sequence.wrapping_add(1));
        if continues {
            let candidate = self.candidate.as_mut().expect("checked above");
            candidate.last_sequence = header.sequence;
            candidate.count += 1;
            if candidate.count >= NAT_LEARN_PACKETS {
                self.trusted.push(source.ip());
                self.pinned = Some((source, header.ssrc));
                return Verdict::Accept { latch: true };
            }
        } else {
            self.candidate = Some(Candidate {
                source,
                ssrc: header.ssrc,
                last_sequence: header.sequence,
                count: 1,
            });
        }
        Verdict::Drop
    }
}

fn is_publicly_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified())
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || first & 0xfe00 == 0xfc00 // unique local
                || first & 0xffc0 == 0xfe80) // link local
        }
    }
}

struct XorShift(u64);

impl XorShift {
    fn seeded() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        XorShift(nanos | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtmf_codes() {
        assert_eq!(dtmf_event_code('0'), Some(0));
        assert_eq!(dtmf_event_code('9'), Some(9));
        assert_eq!(dtmf_event_code('*'), Some(10));
        assert_eq!(dtmf_event_code('#'), Some(11));
        assert_eq!(dtmf_event_code('b'), Some(13));
        assert_eq!(dtmf_event_code('x'), None);
    }

    fn header(ssrc: u32, sequence: u16) -> RtpHeader {
        RtpHeader {
            payload_type: 0,
            marker: false,
            sequence,
            timestamp: 0,
            ssrc,
        }
    }

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn pins_first_trusted_source_and_ssrc() {
        let mut f = PeerFilter::new(
            "203.0.113.5".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        let t = Duration::ZERO;
        assert_eq!(
            f.check(addr("203.0.113.5:4000"), &header(7, 1), t),
            Verdict::Accept { latch: true }
        );
        assert_eq!(
            f.check(addr("203.0.113.5:4000"), &header(7, 2), t),
            Verdict::Accept { latch: false }
        );
        // Same IP, other port or other SSRC: refused after pinning.
        assert_eq!(
            f.check(addr("203.0.113.5:4002"), &header(7, 3), t),
            Verdict::Drop
        );
        assert_eq!(
            f.check(addr("203.0.113.5:4000"), &header(8, 3), t),
            Verdict::Drop
        );
    }

    #[test]
    fn stranger_cannot_pin_before_the_real_peer() {
        let mut f = PeerFilter::new(
            "203.0.113.5".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        for seq in 0..50 {
            assert_eq!(
                f.check(
                    addr("198.51.100.66:9999"),
                    &header(666, seq),
                    Duration::ZERO
                ),
                Verdict::Drop
            );
        }
        assert_eq!(
            f.check(addr("203.0.113.5:4000"), &header(7, 1), Duration::ZERO),
            Verdict::Accept { latch: true }
        );
    }

    #[test]
    fn signalling_server_is_trusted() {
        let mut f = PeerFilter::new(
            "203.0.113.5".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        assert_eq!(
            f.check(addr("203.0.113.9:20000"), &header(1, 1), Duration::ZERO),
            Verdict::Accept { latch: true }
        );
    }

    #[test]
    fn nat_learning_needs_a_sequential_burst() {
        // SDP says 192.168.1.20, but the audio really comes from a public address.
        let mut f = PeerFilter::new(
            "192.168.1.20".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        let src = addr("198.51.100.7:5004");
        for seq in 10..14 {
            assert_eq!(
                f.check(src, &header(3, seq), Duration::from_secs(1)),
                Verdict::Drop
            );
        }
        assert_eq!(
            f.check(src, &header(3, 14), Duration::from_secs(1)),
            Verdict::Accept { latch: true }
        );
    }

    #[test]
    fn nat_learning_rejects_gaps_late_and_public_sdp() {
        let src = addr("198.51.100.7:5004");
        let mut gap = PeerFilter::new(
            "10.0.0.2".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        for seq in [1u16, 2, 4, 5, 6, 8, 9] {
            assert_eq!(
                gap.check(src, &header(3, seq), Duration::from_secs(1)),
                Verdict::Drop
            );
        }
        let mut late = PeerFilter::new(
            "10.0.0.2".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        for seq in 0..20 {
            assert_eq!(
                late.check(src, &header(3, seq), Duration::from_secs(30)),
                Verdict::Drop
            );
        }
        // A publicly routable SDP address means "no NAT": never learn a different IP.
        let mut public = PeerFilter::new(
            "203.0.113.5".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        for seq in 0..20 {
            assert_eq!(
                public.check(src, &header(3, seq), Duration::from_secs(1)),
                Verdict::Drop
            );
        }
    }
}
