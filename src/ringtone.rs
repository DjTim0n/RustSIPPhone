//! Сигнал входящего звонка. Синтезируется на лету и играет через cpal,
//! поэтому звучит одинаково на macOS, Windows и Linux и не требует звуковых файлов.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

pub struct Ringtone {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Ringtone {
    /// Начинает играть сигнал, пока объект не будет сброшен.
    pub fn start() -> Result<Ringtone, String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        // Поток cpal не `Send` на некоторых платформах, поэтому он живёт в отдельном потоке.
        let thread = std::thread::spawn(move || match build_stream() {
            Ok(stream) => {
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(err) => {
                let _ = ready_tx.send(Err(err));
            }
        });
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Ringtone {
                stop: Some(stop_tx),
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                let _ = thread.join();
                Err(err)
            }
            Err(_) => Err("поток сигнала неожиданно завершился".into()),
        }
    }
}

impl Drop for Ringtone {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn build_stream() -> Result<cpal::Stream, String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("не найден динамик")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("конфигурация динамика: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => stream_for::<f32>(&device, config),
        SampleFormat::I16 => stream_for::<i16>(&device, config),
        SampleFormat::U16 => stream_for::<u16>(&device, config),
        SampleFormat::I32 => stream_for::<i32>(&device, config),
        other => Err(format!("формат динамика {other:?} не поддерживается")),
    }?;
    stream.play().map_err(|e| format!("запуск динамика: {e}"))?;
    Ok(stream)
}

fn stream_for<T>(device: &cpal::Device, config: StreamConfig) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let mut synth = Synth::new(config.sample_rate);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for frame in data.chunks_mut(channels) {
                    frame.fill(T::from_sample(synth.next()));
                }
            },
            |err| eprintln!("ошибка динамика: {err}"),
            None,
        )
        .map_err(|e| format!("открытие динамика: {e}"))
}

/// Длина всего цикла «двойной звонок + пауза», секунды.
const CYCLE: f32 = 3.0;
/// Участки цикла, где звучит сигнал: (начало, конец).
const BURSTS: [(f32, f32); 2] = [(0.0, 0.4), (0.6, 1.0)];
const FADE: f32 = 0.02;
const VOLUME: f32 = 0.18;

struct Synth {
    rate: f32,
    frame: u64,
}

impl Synth {
    fn new(rate: u32) -> Self {
        Synth {
            rate: rate as f32,
            frame: 0,
        }
    }

    fn next(&mut self) -> f32 {
        let t = self.frame as f32 / self.rate;
        self.frame += 1;
        let phase = t % CYCLE;
        let envelope = BURSTS
            .iter()
            .filter(|(start, end)| phase >= *start && phase < *end)
            .map(|(start, end)| ((phase - start) / FADE).min((end - phase) / FADE).min(1.0))
            .next()
            .unwrap_or(0.0);
        if envelope == 0.0 {
            return 0.0;
        }
        let tau = std::f32::consts::TAU;
        let tone = (tau * 440.0 * t).sin() + (tau * 480.0 * t).sin();
        tone * 0.5 * envelope * VOLUME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rings_twice_then_pauses() {
        let mut synth = Synth::new(8000);
        let mut loud_frames = 0;
        let mut quiet_tail = true;
        for frame in 0..(CYCLE * 8000.0) as usize {
            let sample = synth.next();
            assert!(sample.abs() <= 1.0);
            if sample.abs() > 0.01 {
                loud_frames += 1;
            }
            if frame as f32 / 8000.0 > 1.05 && sample != 0.0 {
                quiet_tail = false;
            }
        }
        assert!(loud_frames > 3000, "сигнал должен звучать: {loud_frames}");
        assert!(quiet_tail, "после двух звонков должна быть тишина");
    }
}
