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
    /// The other side moved its audio to this address (a new session description in the call).
    pub retarget: mpsc::UnboundedReceiver<SocketAddr>,
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
    let MediaControl {
        muted,
        dtmf,
        retarget,
    } = control;
    let ((), received) = tokio::join!(
        send_loop(
            &socket,
            codec,
            dtmf_pt,
            &mut audio.mic,
            muted,
            dtmf,
            remote_rx,
            &stop
        ),
        receive_loop(&socket, codec, speaker, remote_tx, retarget, filter, &stop),
    );
    // Closing the devices waits for the audio thread, which must not hold up the async threads.
    let _ = tokio::task::spawn_blocking(move || drop(audio)).await;
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

/// The payload of the `n`-th packet of a key press (RFC 4733): the key, the end flag and how
/// long the key has been held so far. The last packets repeat the final one unchanged, so that
/// losing one of them does not lose the end of the key press.
fn dtmf_payload(event: u8, n: u16) -> [u8; 4] {
    let first_end = DTMF_PACKETS - DTMF_END_PACKETS;
    let end = n >= first_end;
    let duration = (n.min(first_end) + 1) * FRAME_SAMPLES as u16;
    [
        event,
        (end as u8) << 7 | 10,
        (duration >> 8) as u8,
        duration as u8,
    ]
}

#[allow(clippy::too_many_arguments)]
async fn send_loop(
    socket: &UdpSocket,
    codec: Codec,
    dtmf_pt: Option<u8>,
    mic: &mut mpsc::Receiver<Vec<i16>>,
    muted: Arc<AtomicBool>,
    mut dtmf_keys: mpsc::UnboundedReceiver<char>,
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
            Some(digit) = dtmf_keys.recv() => {
                if let (Some(event), Some(_)) = (dtmf_event_code(digit), dtmf_pt) {
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
            if muted.load(Ordering::Relaxed) {
                frame.fill(0);
            }

            if dtmf.is_none()
                && let Some(event) = digits.pop_front()
            {
                dtmf = Some(DtmfState {
                    event,
                    start_timestamp: timestamp,
                    packets_sent: 0,
                });
            }

            let (header, payload) = match (&mut dtmf, dtmf_pt) {
                // While a digit is being sent, event packets replace the audio.
                (Some(state), Some(pt)) => {
                    let n = state.packets_sent;
                    let payload = dtmf_payload(state.event, n).to_vec();
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
    mut retarget: mpsc::UnboundedReceiver<SocketAddr>,
    mut filter: PeerFilter,
    stop: &CancellationToken,
) -> u64 {
    let mut buf = [0u8; 2048];
    let mut received = 0u64;
    let mut started = Instant::now();
    // The address the other side's session description names.
    let mut announced = *remote.borrow();

    loop {
        let (len, source) = tokio::select! {
            _ = stop.cancelled() => break,
            Some(addr) = retarget.recv() => {
                if addr == announced {
                    continue;
                }
                // The other side named a new address: send there and learn its stream afresh.
                announced = addr;
                filter.retarget(addr.ip());
                started = Instant::now();
                let _ = remote.send(addr);
                continue;
            }
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
                // (a NAT usually rewrites only the port). It changes only when the other side
                // sends a new session description.
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

/// The stream the call listens to.
struct Pinned {
    source: SocketAddr,
    ssrc: u32,
    last_sequence: u16,
    /// When its last packet arrived, on the clock `check` is given.
    last_seen: Duration,
}

/// How long the stream the call listens to must have been silent before another one from the
/// same address may replace it. A burst of forged packets squeezed in between two real ones
/// (20 ms apart) therefore cannot take the call over.
const STREAM_QUIET: Duration = Duration::from_millis(300);

/// A packet this far behind the newest one is late or repeated and is skipped; one further
/// behind means the sender started counting anew.
const MAX_LATE_PACKETS: i16 = 50;

/// How many packets in a row `source` has now sent with this SSRC, each following the last.
fn streak(candidate: &mut Option<Candidate>, source: SocketAddr, header: &RtpHeader) -> u32 {
    match candidate {
        Some(c)
            if c.source == source
                && c.ssrc == header.ssrc
                && header.sequence == c.last_sequence.wrapping_add(1) =>
        {
            c.last_sequence = header.sequence;
            c.count += 1;
            c.count
        }
        _ => {
            *candidate = Some(Candidate {
                source,
                ssrc: header.ssrc,
                last_sequence: header.sequence,
                count: 1,
            });
            1
        }
    }
}

/// Decides which incoming RTP packets are allowed to reach the speaker.
///
/// Plain RTP carries no authentication, so we limit what a third party can do:
/// * packets are accepted only from the IPs we negotiated with (the SDP address
///   and the signalling server, which usually hosts the media relay);
/// * the first accepted packet pins source address and SSRC. The address stays until the other
///   side sends a new session description; a new SSRC from that address takes over only with
///   a short, strictly sequential burst after the old stream has gone quiet (a station starts
///   a new stream when it puts the call on hold, plays music or connects another party);
/// * repeated and late packets are skipped, so audio is never played out of order;
/// * only if the SDP address is private (the peer is behind NAT) do we learn another
///   address, and then only from a short, strictly sequential burst early in the call.
///
/// This does not stop an attacker who can spoof a trusted IP; that needs SRTP.
struct PeerFilter {
    trusted: Vec<IpAddr>,
    allow_nat_learning: bool,
    pinned: Option<Pinned>,
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

    /// The other side moved its audio to `sdp_ip`: forget the old stream and learn the new one.
    fn retarget(&mut self, sdp_ip: IpAddr) {
        self.trusted[0] = sdp_ip;
        self.allow_nat_learning = !is_publicly_routable(sdp_ip);
        self.pinned = None;
        self.candidate = None;
    }

    fn pin(&mut self, source: SocketAddr, header: &RtpHeader, now: Duration) {
        self.pinned = Some(Pinned {
            source,
            ssrc: header.ssrc,
            last_sequence: header.sequence,
            last_seen: now,
        });
        self.candidate = None;
    }

    fn check(&mut self, source: SocketAddr, header: &RtpHeader, elapsed: Duration) -> Verdict {
        if let Some(pinned) = &mut self.pinned {
            if source != pinned.source {
                return Verdict::Drop;
            }
            if header.ssrc == pinned.ssrc {
                // The stream we listen to is alive: nothing else gets to replace it.
                self.candidate = None;
                pinned.last_seen = elapsed;
                let ahead = header.sequence.wrapping_sub(pinned.last_sequence) as i16;
                if ahead <= 0 && ahead > -MAX_LATE_PACKETS {
                    return Verdict::Drop;
                }
                pinned.last_sequence = header.sequence;
                return Verdict::Accept { latch: false };
            }
            if elapsed.saturating_sub(pinned.last_seen) < STREAM_QUIET {
                self.candidate = None;
                return Verdict::Drop;
            }
            if streak(&mut self.candidate, source, header) >= NAT_LEARN_PACKETS {
                self.pin(source, header, elapsed);
                return Verdict::Accept { latch: false };
            }
            return Verdict::Drop;
        }
        if self.trusted.contains(&source.ip()) {
            self.pin(source, header, elapsed);
            return Verdict::Accept { latch: true };
        }
        if !self.allow_nat_learning || elapsed > NAT_LEARN_WINDOW {
            return Verdict::Drop;
        }
        if streak(&mut self.candidate, source, header) >= NAT_LEARN_PACKETS {
            self.trusted.push(source.ip());
            self.pin(source, header, elapsed);
            return Verdict::Accept { latch: true };
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

    fn pinned_filter() -> PeerFilter {
        let mut f = PeerFilter::new(
            "203.0.113.5".parse().unwrap(),
            &["203.0.113.9".parse().unwrap()],
        );
        f.check(addr("203.0.113.5:4000"), &header(7, 100), Duration::ZERO);
        f
    }

    /// A station starts a new stream (hold music, another party after a transfer) from the same
    /// address. The call must keep its sound.
    #[test]
    fn a_new_stream_from_the_same_address_takes_over() {
        let mut f = pinned_filter();
        let src = addr("203.0.113.5:4000");
        let t = Duration::from_secs(60);
        for seq in 500..504 {
            assert_eq!(f.check(src, &header(8, seq), t), Verdict::Drop);
        }
        assert_eq!(
            f.check(src, &header(8, 504), t),
            Verdict::Accept { latch: false }
        );
        assert_eq!(
            f.check(src, &header(8, 505), t),
            Verdict::Accept { latch: false }
        );
        assert_eq!(
            f.check(src, &header(7, 101), t),
            Verdict::Drop,
            "the old one"
        );
    }

    /// Forged packets sent in one go, right after a real one, must not take the call over
    /// however many of them there are.
    #[test]
    fn a_burst_cannot_replace_a_stream_that_was_just_heard() {
        let mut f = pinned_filter();
        let src = addr("203.0.113.5:4000");
        let t = Duration::from_secs(60);
        assert_eq!(
            f.check(src, &header(7, 101), t),
            Verdict::Accept { latch: false }
        );
        for seq in 500..600 {
            let soon = t + Duration::from_millis(19);
            assert_eq!(f.check(src, &header(8, seq), soon), Verdict::Drop);
        }
        assert_eq!(
            f.check(src, &header(7, 102), t + Duration::from_millis(20)),
            Verdict::Accept { latch: false }
        );
    }

    #[test]
    fn a_live_stream_is_not_replaced() {
        let mut f = pinned_filter();
        let src = addr("203.0.113.5:4000");
        let t = Duration::from_secs(60);
        // Packets of another stream arrive in between the real ones: they never add up.
        for i in 0..20u16 {
            assert_eq!(f.check(src, &header(8, 500 + i), t), Verdict::Drop);
            assert_eq!(
                f.check(src, &header(7, 101 + i), t),
                Verdict::Accept { latch: false }
            );
        }
    }

    #[test]
    fn repeated_and_late_packets_are_skipped() {
        let mut f = pinned_filter();
        let src = addr("203.0.113.5:4000");
        let t = Duration::ZERO;
        assert_eq!(
            f.check(src, &header(7, 102), t),
            Verdict::Accept { latch: false }
        );
        assert_eq!(f.check(src, &header(7, 102), t), Verdict::Drop, "repeated");
        assert_eq!(f.check(src, &header(7, 101), t), Verdict::Drop, "late");
        assert_eq!(
            f.check(src, &header(7, 103), t),
            Verdict::Accept { latch: false }
        );
        // The counter wraps around, and a sender may start it anew.
        let mut wrap = PeerFilter::new("203.0.113.5".parse().unwrap(), &[]);
        wrap.check(src, &header(7, u16::MAX), t);
        assert_eq!(
            wrap.check(src, &header(7, 0), t),
            Verdict::Accept { latch: false }
        );
        assert_eq!(
            wrap.check(src, &header(7, 30000), t),
            Verdict::Accept { latch: false }
        );
    }

    #[test]
    fn a_new_address_from_the_other_side_is_learned_afresh() {
        let mut f = pinned_filter();
        f.retarget("203.0.113.77".parse().unwrap());
        assert_eq!(
            f.check(addr("203.0.113.77:6000"), &header(9, 1), Duration::ZERO),
            Verdict::Accept { latch: true }
        );
        assert_eq!(
            f.check(addr("203.0.113.5:4000"), &header(7, 101), Duration::ZERO),
            Verdict::Drop,
            "the old address is no longer listened to"
        );
    }

    #[test]
    fn the_end_of_a_key_press_is_repeated_unchanged() {
        let packets: Vec<[u8; 4]> = (0..DTMF_PACKETS).map(|n| dtmf_payload(5, n)).collect();
        assert_eq!(packets[0], [5, 10, 0, 160]);
        assert_eq!(packets[6][1] & 0x80, 0, "still held");
        let end = packets[7];
        assert_eq!(end[1] & 0x80, 0x80, "the end flag");
        assert_eq!(u16::from_be_bytes([end[2], end[3]]), 8 * 160);
        assert_eq!(packets[8], end);
        assert_eq!(packets[9], end);
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
