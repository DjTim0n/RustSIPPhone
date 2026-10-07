//! Микрофон и динамик через cpal. Снаружи звук всегда моно, 8 кГц, i16.
//!
//! cpal-потоки не `Send` на некоторых платформах, поэтому живут в отдельном потоке,
//! а с асинхронным миром общаются через канал (микрофон) и общую очередь (динамик).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub const SAMPLE_RATE: u32 = 8000;
/// Сколько миллисекунд накопить в очереди динамика перед началом воспроизведения.
const PREBUFFER_MS: usize = 60;
/// Максимальная задержка очереди воспроизведения; всё, что сверх, отбрасывается.
const MAX_QUEUE_MS: usize = 400;

pub type SpeakerQueue = Arc<Mutex<Playback>>;

pub struct AudioIo {
    /// Блоки моно-звука 8 кГц от микрофона (размер блока произвольный).
    pub mic: mpsc::Receiver<Vec<i16>>,
    /// Очередь, в которую складываются декодированные сэмплы собеседника.
    pub speaker: SpeakerQueue,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioIo {
    pub fn start() -> Result<AudioIo, String> {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(64);
        let speaker: SpeakerQueue = Arc::new(Mutex::new(Playback::default()));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

        let speaker_for_thread = speaker.clone();
        let thread = std::thread::spawn(move || {
            match build_streams(mic_tx, speaker_for_thread) {
                Ok(streams) => {
                    let _ = ready_tx.send(Ok(()));
                    // Ждём команды остановки (или закрытия канала), потом потоки сбросятся.
                    let _ = stop_rx.recv();
                    drop(streams);
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                }
            }
        });

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(AudioIo {
                mic: mic_rx,
                speaker,
                stop: Some(stop_tx),
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                let _ = thread.join();
                Err(err)
            }
            Err(_) => Err("аудио-поток неожиданно завершился".into()),
        }
    }
}

impl Drop for AudioIo {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Очередь воспроизведения с предварительной буферизацией.
#[derive(Default)]
pub struct Playback {
    queue: VecDeque<i16>,
    started: bool,
}

impl Playback {
    pub fn push(&mut self, samples: &[i16]) {
        self.queue.extend(samples.iter().copied());
        let max = MAX_QUEUE_MS * SAMPLE_RATE as usize / 1000;
        if self.queue.len() > max {
            let excess = self.queue.len() - max;
            self.queue.drain(..excess);
        }
    }

    fn pop(&mut self) -> Option<i16> {
        if !self.started {
            if self.queue.len() < PREBUFFER_MS * SAMPLE_RATE as usize / 1000 {
                return None;
            }
            self.started = true;
        }
        let sample = self.queue.pop_front();
        if sample.is_none() {
            // Очередь опустела: снова накопим небольшой запас, чтобы не «дребезжало».
            self.started = false;
        }
        sample
    }
}

struct Streams {
    _input: cpal::Stream,
    _output: cpal::Stream,
}

fn build_streams(mic_tx: mpsc::Sender<Vec<i16>>, speaker: SpeakerQueue) -> Result<Streams, String> {
    let host = cpal::default_host();
    let input_device = host
        .default_input_device()
        .ok_or("не найден микрофон (проверьте доступ к микрофону в настройках системы)")?;
    let output_device = host.default_output_device().ok_or("не найден динамик")?;

    let input = build_input(&input_device, mic_tx)?;
    let output = build_output(&output_device, speaker)?;
    input.play().map_err(|e| format!("запуск микрофона: {e}"))?;
    output.play().map_err(|e| format!("запуск динамика: {e}"))?;
    Ok(Streams {
        _input: input,
        _output: output,
    })
}

fn build_input(device: &cpal::Device, tx: mpsc::Sender<Vec<i16>>) -> Result<cpal::Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|e| format!("конфигурация микрофона: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    match format {
        SampleFormat::F32 => input_stream::<f32>(device, config, tx),
        SampleFormat::I16 => input_stream::<i16>(device, config, tx),
        SampleFormat::U16 => input_stream::<u16>(device, config, tx),
        SampleFormat::I32 => input_stream::<i32>(device, config, tx),
        other => Err(format!("формат микрофона {other:?} не поддерживается")),
    }
}

fn build_output(device: &cpal::Device, queue: SpeakerQueue) -> Result<cpal::Stream, String> {
    let supported = device
        .default_output_config()
        .map_err(|e| format!("конфигурация динамика: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    match format {
        SampleFormat::F32 => output_stream::<f32>(device, config, queue),
        SampleFormat::I16 => output_stream::<i16>(device, config, queue),
        SampleFormat::U16 => output_stream::<u16>(device, config, queue),
        SampleFormat::I32 => output_stream::<i32>(device, config, queue),
        other => Err(format!("формат динамика {other:?} не поддерживается")),
    }
}

fn input_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    tx: mpsc::Sender<Vec<i16>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let mut downsampler = Downsampler::new(config.sample_rate, SAMPLE_RATE);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut out = Vec::with_capacity(data.len() / channels / 4 + 1);
                for frame in data.chunks(channels) {
                    let mono =
                        frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32;
                    if let Some(sample) = downsampler.push(mono) {
                        out.push((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                    }
                }
                if !out.is_empty() {
                    // Если потребитель не успевает (звонок ещё не принят) — старый звук не копим.
                    let _ = tx.try_send(out);
                }
            },
            |err| eprintln!("ошибка микрофона: {err}"),
            None,
        )
        .map_err(|e| format!("открытие микрофона: {e}"))
}

fn output_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    queue: SpeakerQueue,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let mut upsampler = Upsampler::new(SAMPLE_RATE, config.sample_rate);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
                for frame in data.chunks_mut(channels) {
                    let sample = upsampler.next(|| queue.pop());
                    let value = T::from_sample(sample);
                    frame.fill(value);
                }
            },
            |err| eprintln!("ошибка динамика: {err}"),
            None,
        )
        .map_err(|e| format!("открытие динамика: {e}"))
}

/// Потоковое понижение частоты усреднением окна (грубо, но без сильного алиасинга).
struct Downsampler {
    ratio: f64,
    phase: f64,
    sum: f32,
    count: u32,
}

impl Downsampler {
    fn new(from: u32, to: u32) -> Self {
        Downsampler {
            ratio: from as f64 / to as f64,
            phase: 0.0,
            sum: 0.0,
            count: 0,
        }
    }

    fn push(&mut self, sample: f32) -> Option<f32> {
        self.sum += sample;
        self.count += 1;
        self.phase += 1.0;
        if self.phase >= self.ratio {
            let out = self.sum / self.count as f32;
            self.phase -= self.ratio;
            self.sum = 0.0;
            self.count = 0;
            Some(out)
        } else {
            None
        }
    }
}

/// Потоковое повышение частоты линейной интерполяцией.
struct Upsampler {
    step: f64,
    pos: f64,
    prev: f32,
    cur: f32,
}

impl Upsampler {
    fn new(from: u32, to: u32) -> Self {
        Upsampler {
            step: from as f64 / to as f64,
            pos: 0.0,
            prev: 0.0,
            cur: 0.0,
        }
    }

    fn next(&mut self, mut source: impl FnMut() -> Option<i16>) -> f32 {
        let out = self.prev + (self.cur - self.prev) * self.pos as f32;
        self.pos += self.step;
        while self.pos >= 1.0 {
            self.pos -= 1.0;
            self.prev = self.cur;
            self.cur = source().map_or(0.0, |s| s as f32 / i16::MAX as f32);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsampler_emits_expected_count() {
        let mut d = Downsampler::new(48000, 8000);
        let produced = (0..4800).filter_map(|_| d.push(0.5)).count();
        assert_eq!(produced, 800);
    }

    #[test]
    fn downsampler_non_integer_ratio() {
        let mut d = Downsampler::new(44100, 8000);
        let produced = (0..44100).filter_map(|_| d.push(0.0)).count();
        assert!((7999..=8001).contains(&produced), "{produced}");
    }

    #[test]
    fn upsampler_consumes_expected_count() {
        let mut u = Upsampler::new(8000, 48000);
        let mut consumed = 0;
        for _ in 0..48000 {
            u.next(|| {
                consumed += 1;
                Some(1000)
            });
        }
        assert!((7999..=8001).contains(&consumed), "{consumed}");
    }

    #[test]
    fn playback_prebuffers_then_drains() {
        let mut p = Playback::default();
        p.push(&[1; 100]);
        assert_eq!(p.pop(), None); // ещё копим запас
        p.push(&[1; 400]);
        assert_eq!(p.pop(), Some(1));
    }

    #[test]
    fn playback_caps_queue() {
        let mut p = Playback::default();
        p.push(&vec![0; 8000]);
        assert_eq!(p.queue.len(), MAX_QUEUE_MS * 8);
    }
}
