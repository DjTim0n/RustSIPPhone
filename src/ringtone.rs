//! Call tones: the ringtone of an incoming call and the ringback tone of an outgoing one. They are
//! synthesized on the fly and played through cpal, so they sound the same on macOS, Windows and
//! Linux and need no sound files.

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

/// Which tone to play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// An incoming call: a double ring, then a pause.
    Ring,
    /// An outgoing call while the other phone rings: one long beep every few seconds.
    Ringback,
}

impl Tone {
    /// Length of the whole sound-and-pause cycle, in seconds.
    fn cycle(self) -> f32 {
        match self {
            Tone::Ring => 3.0,
            Tone::Ringback => 5.0,
        }
    }

    /// Parts of the cycle where the tone sounds: (start, end).
    fn bursts(self) -> &'static [(f32, f32)] {
        match self {
            Tone::Ring => &[(0.0, 0.4), (0.6, 1.0)],
            Tone::Ringback => &[(0.0, 1.0)],
        }
    }

    fn frequencies(self) -> &'static [f32] {
        match self {
            Tone::Ring => &[440.0, 480.0],
            Tone::Ringback => &[425.0],
        }
    }
}

pub struct Ringtone {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Ringtone {
    /// Starts playing `tone` on the named speaker (the default if `None`) until the object is
    /// dropped.
    pub fn start(output_device: Option<&str>, tone: Tone) -> Result<Ringtone, String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let output_device = output_device.map(str::to_string);
        // The cpal stream is not `Send` on some platforms, so it lives on its own thread.
        let thread =
            std::thread::spawn(move || match build_stream(output_device.as_deref(), tone) {
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
            Err(_) => Err("the ringtone thread ended unexpectedly".into()),
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

fn build_stream(output_device: Option<&str>, tone: Tone) -> Result<cpal::Stream, String> {
    let device = crate::audio::pick_output(&cpal::default_host(), output_device)
        .ok_or("no speaker found")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("speaker configuration: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => stream_for::<f32>(&device, config, tone),
        SampleFormat::I16 => stream_for::<i16>(&device, config, tone),
        SampleFormat::U16 => stream_for::<u16>(&device, config, tone),
        SampleFormat::I32 => stream_for::<i32>(&device, config, tone),
        other => Err(format!("speaker sample format {other:?} is not supported")),
    }?;
    stream
        .play()
        .map_err(|e| format!("starting the speaker: {e}"))?;
    Ok(stream)
}

fn stream_for<T>(
    device: &cpal::Device,
    config: StreamConfig,
    tone: Tone,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let mut synth = Synth::new(config.sample_rate, tone);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for frame in data.chunks_mut(channels) {
                    frame.fill(T::from_sample(synth.next()));
                }
            },
            |err| eprintln!("speaker error: {err}"),
            None,
        )
        .map_err(|e| format!("opening the speaker: {e}"))
}

const FADE: f32 = 0.02;
const VOLUME: f32 = 0.18;

struct Synth {
    rate: f32,
    frame: u64,
    tone: Tone,
}

impl Synth {
    fn new(rate: u32, tone: Tone) -> Self {
        Synth {
            rate: rate as f32,
            frame: 0,
            tone,
        }
    }

    fn next(&mut self) -> f32 {
        let t = self.frame as f32 / self.rate;
        self.frame += 1;
        let phase = t % self.tone.cycle();
        let envelope = self
            .tone
            .bursts()
            .iter()
            .filter(|(start, end)| phase >= *start && phase < *end)
            .map(|(start, end)| ((phase - start) / FADE).min((end - phase) / FADE).min(1.0))
            .next()
            .unwrap_or(0.0);
        if envelope == 0.0 {
            return 0.0;
        }
        let tau = std::f32::consts::TAU;
        let frequencies = self.tone.frequencies();
        let sum: f32 = frequencies.iter().map(|f| (tau * f * t).sin()).sum();
        // The average of the tones, so every tone has the same peak level.
        sum / frequencies.len() as f32 * envelope * VOLUME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud_spans(tone: Tone) -> Vec<(f32, f32)> {
        let mut synth = Synth::new(8000, tone);
        let mut spans: Vec<(f32, f32)> = Vec::new();
        for frame in 0..(tone.cycle() * 8000.0) as usize {
            let sample = synth.next();
            assert!(sample.abs() <= 1.0, "no clipping");
            let t = frame as f32 / 8000.0;
            if sample.abs() > 0.01 {
                match spans.last_mut() {
                    Some(last) if t - last.1 < 0.05 => last.1 = t,
                    _ => spans.push((t, t)),
                }
            }
        }
        spans
    }

    #[test]
    fn ring_is_a_double_ring_then_a_pause() {
        let spans = loud_spans(Tone::Ring);
        assert_eq!(spans.len(), 2, "two rings: {spans:?}");
        assert!(
            spans[1].1 < 1.05,
            "silence for the rest of the cycle: {spans:?}"
        );
    }

    #[test]
    fn ringback_is_one_long_beep_then_a_pause() {
        let spans = loud_spans(Tone::Ringback);
        assert_eq!(spans.len(), 1, "one beep: {spans:?}");
        assert!(
            spans[0].1 - spans[0].0 > 0.9,
            "about a second long: {spans:?}"
        );
        assert!(
            spans[0].1 < 1.05,
            "silence for the other four seconds: {spans:?}"
        );
    }
}
