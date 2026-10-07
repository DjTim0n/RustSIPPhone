//! RTP-сессия: микрофон -> G.711 -> UDP и UDP -> G.711 -> динамик, плюс DTMF (RFC 4733).

use crate::audio::{AudioIo, SpeakerQueue};
use crate::g711::Codec;
use crate::rtp::{self, RtpHeader};
use crate::sdp::Remote;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// 20 мс при 8 кГц.
const FRAME_SAMPLES: usize = 160;
/// Сколько 20-мс пакетов занимает одна цифра (200 мс); последние три — с флагом конца.
const DTMF_PACKETS: u16 = 10;
const DTMF_END_PACKETS: u16 = 3;

#[derive(Debug, Default)]
pub struct MediaStats {
    pub received: u64,
}

/// То, чем ядро управляет разговором во время звонка.
pub struct MediaControl {
    pub muted: Arc<AtomicBool>,
    pub dtmf: mpsc::UnboundedReceiver<char>,
}

/// Крутит медиа, пока не сработает `stop`. Владеет аудиоустройствами: после выхода они закрыты.
pub async fn run(
    socket: Arc<UdpSocket>,
    remote: Remote,
    mut audio: AudioIo,
    control: MediaControl,
    stop: CancellationToken,
) -> MediaStats {
    // Звук, накопленный за время дозвона, не нужен.
    while audio.mic.try_recv().is_ok() {}

    let (remote_tx, remote_rx) = watch::channel(remote.addr);
    let speaker = audio.speaker.clone();
    let codec = remote.codec;
    let dtmf_pt = remote.dtmf_pt;

    let ((), received) = tokio::join!(
        send_loop(&socket, codec, dtmf_pt, &mut audio.mic, control, remote_rx, &stop),
        receive_loop(&socket, codec, speaker, remote_tx, &stop),
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
                // Пока идёт цифра, вместо звука шлём пакеты события.
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
                        marker = true; // после события звук начинается «заново»
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
    stop: &CancellationToken,
) -> u64 {
    let mut buf = [0u8; 2048];
    let mut received = 0u64;
    let mut peer_ssrc: Option<u32> = None;

    loop {
        let (len, source) = tokio::select! {
            _ = stop.cancelled() => break,
            result = socket.recv_from(&mut buf) => match result {
                Ok(pair) => pair,
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
            },
        };
        let Some((header, payload)) = rtp::parse_packet(&buf[..len]) else {
            continue;
        };
        if header.payload_type != codec.payload_type() {
            continue; // DTMF, comfort noise и прочее нам не нужно
        }
        // Симметричный RTP: если звук идёт с другого адреса (NAT), отвечаем туда же.
        // Привязываемся только к тому SSRC, что пришёл первым, чтобы посторонний пакет не перехватил поток.
        let ssrc = *peer_ssrc.get_or_insert(header.ssrc);
        if header.ssrc != ssrc {
            continue;
        }
        if *remote.borrow() != source {
            let _ = remote.send(source);
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
}
