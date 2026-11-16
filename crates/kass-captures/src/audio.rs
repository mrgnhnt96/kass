//! A capture's recording: its waveform, and playing it on the default output.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A recording as mono samples.
#[derive(Clone, Debug)]
pub struct Clip {
    samples: Arc<Vec<f32>>,
    rate: u32,
}

impl Clip {
    pub fn duration_ms(&self) -> u64 {
        if self.rate == 0 {
            return 0;
        }
        self.samples.len() as u64 * 1000 / self.rate as u64
    }
}

/// Reads a WAV (what the server stores) into mono samples.
pub fn decode_wav(bytes: &[u8]) -> Result<Clip, String> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    let samples = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok(Clip {
        samples: Arc::new(samples),
        rate: spec.sample_rate,
    })
}

/// `bars` peak heights from 0 to 1, normalized to the loudest.
pub fn peaks(clip: &Clip, bars: usize) -> Vec<f32> {
    if bars == 0 || clip.samples.is_empty() {
        return vec![0.; bars];
    }
    let per_bar = clip.samples.len().div_ceil(bars).max(1);
    let mut peaks: Vec<f32> = clip
        .samples
        .chunks(per_bar)
        .map(|chunk| chunk.iter().fold(0f32, |peak, s| peak.max(s.abs())))
        .collect();
    peaks.resize(bars, 0.);
    let loudest = peaks.iter().copied().fold(0f32, f32::max);
    if loudest > 0. {
        for peak in &mut peaks {
            *peak /= loudest;
        }
    }
    peaks
}

struct Shared {
    /// Position in the clip's samples, as f64 bits.
    position: AtomicU64,
    playing: AtomicBool,
}

impl Shared {
    fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::Relaxed))
    }

    fn set_position(&self, position: f64) {
        self.position.store(position.to_bits(), Ordering::Relaxed);
    }
}

/// A clip on the default output device. Dropping it stops the sound.
pub struct Player {
    clip: Clip,
    shared: Arc<Shared>,
    stream: Option<cpal::Stream>,
}

impl Player {
    pub fn new(clip: Clip) -> Self {
        Self {
            clip,
            shared: Arc::new(Shared {
                position: AtomicU64::new(0f64.to_bits()),
                playing: AtomicBool::new(false),
            }),
            stream: None,
        }
    }

    pub fn clip(&self) -> &Clip {
        &self.clip
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Where playback is, from 0 to 1.
    pub fn progress(&self) -> f32 {
        let len = self.clip.samples.len();
        if len == 0 {
            return 0.;
        }
        (self.shared.position() / len as f64).clamp(0., 1.) as f32
    }

    pub fn position_ms(&self) -> u64 {
        if self.clip.rate == 0 {
            return 0;
        }
        (self.shared.position() * 1000. / self.clip.rate as f64) as u64
    }

    /// Jumps to `fraction` (0 to 1) of the clip.
    pub fn seek(&self, fraction: f32) {
        let len = self.clip.samples.len() as f64;
        self.shared
            .set_position(len * fraction.clamp(0., 1.) as f64);
    }

    pub fn play(&mut self) -> Result<(), String> {
        if self.stream.is_none() {
            self.stream = Some(self.open()?);
        }
        self.shared.playing.store(true, Ordering::Relaxed);
        if let Some(stream) = &self.stream {
            stream.play().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn pause(&mut self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        if let Some(stream) = &self.stream {
            stream.pause().ok();
        }
    }

    fn open(&self) -> Result<cpal::Stream, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| "no output device".to_string())?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        match format {
            cpal::SampleFormat::F32 => self.build::<f32>(&device, &config),
            cpal::SampleFormat::I16 => self.build::<i16>(&device, &config),
            cpal::SampleFormat::U16 => self.build::<u16>(&device, &config),
            cpal::SampleFormat::I32 => self.build::<i32>(&device, &config),
            other => Err(format!("unsupported output format {other}")),
        }
    }

    fn build<T: SizedSample + FromSample<f32>>(
        &self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
    ) -> Result<cpal::Stream, String> {
        let channels = config.channels.max(1) as usize;
        let step = self.clip.rate as f64 / config.sample_rate.0.max(1) as f64;
        let samples = self.clip.samples.clone();
        let shared = self.shared.clone();
        device
            .build_output_stream(
                config,
                move |out: &mut [T], _| {
                    let playing = shared.playing.load(Ordering::Relaxed);
                    let mut position = shared.position();
                    for frame in out.chunks_mut(channels) {
                        let value = if playing {
                            let at = position as usize;
                            match samples.get(at) {
                                Some(&sample) => {
                                    position += step;
                                    sample
                                }
                                None => 0.,
                            }
                        } else {
                            0.
                        };
                        frame.fill(T::from_sample(value));
                    }
                    if playing && position as usize >= samples.len() {
                        // Back to the start at the end, ready to play again.
                        shared.playing.store(false, Ordering::Relaxed);
                        position = 0.;
                    }
                    shared.set_position(position);
                },
                |error| eprintln!("kass-captures: playback failed: {error}"),
                None,
            )
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[i16], channels: u16) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut bytes, spec).unwrap();
        for &sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        bytes.into_inner()
    }

    #[test]
    fn a_wav_decodes_to_mono() {
        let clip = decode_wav(&wav(&[16_384, -16_384, 0, 0], 2)).unwrap();
        assert_eq!(clip.samples.len(), 2);
        assert!(clip.samples[0].abs() < 1e-6);
        assert!(decode_wav(b"not a wav").is_err());
    }

    #[test]
    fn peaks_are_normalized_to_the_loudest() {
        let samples: Vec<i16> = (0..16_000)
            .map(|i| if i < 8_000 { 1_000 } else { 4_000 })
            .collect();
        let clip = decode_wav(&wav(&samples, 1)).unwrap();
        assert_eq!(clip.duration_ms(), 1000);
        let peaks = peaks(&clip, 4);
        assert_eq!(peaks.len(), 4);
        assert!((peaks[3] - 1.).abs() < 1e-6);
        assert!((peaks[0] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn seeking_moves_the_position() {
        let clip = decode_wav(&wav(&vec![0; 16_000], 1)).unwrap();
        let player = Player::new(clip);
        player.seek(0.5);
        assert_eq!(player.position_ms(), 500);
        assert!((player.progress() - 0.5).abs() < 1e-6);
        assert!(!player.is_playing());
    }
}
