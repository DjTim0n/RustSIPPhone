//! Incoming-call ringtone. It is synthesized on the fly and played through cpal,
//! so it sounds the same on macOS, Windows and Linux and needs no sound files.

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

pub struct Ringtone {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Ringtone {
    /// Starts playing the tone on the named speaker (the default if `None`) until the object is
    /// dropped.
    pub fn start(output_device: Option<&str>) -> Result<Ringtone, String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let output_device = output_device.map(str::to_string);
        // The cpal stream is not `Send` on some platforms, so it lives on its own thread.
        let thread = std::thread::spawn(move || match build_stream(output_device.as_deref()) {
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

fn build_stream(output_device: Option<&str>) -> Result<cpal::Stream, String> {
    let device = crate::audio::pick_output(&cpal::default_host(), output_device)
        .ok_or("no speaker found")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("speaker configuration: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => stream_for::<f32>(&device, config),
        SampleFormat::I16 => stream_for::<i16>(&device, config),
        SampleFormat::U16 => stream_for::<u16>(&device, config),
        SampleFormat::I32 => stream_for::<i32>(&device, config),
        other => Err(format!("speaker sample format {other:?} is not supported")),
    }?;
    stream
        .play()
        .map_err(|e| format!("starting the speaker: {e}"))?;
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
            |err| eprintln!("speaker error: {err}"),
            None,
        )
        .map_err(|e| format!("opening the speaker: {e}"))
}

/// Length of the whole "double ring + pause" cycle, in seconds.
const CYCLE: f32 = 3.0;
/// Parts of the cycle where the tone sounds: (start, end).
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
        assert!(
            loud_frames > 3000,
            "the tone should be audible: {loud_frames}"
        );
        assert!(quiet_tail, "there should be silence after the two rings");
    }
}
