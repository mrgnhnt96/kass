//! Pure audio helpers for native dictation: downmix, framing, device and
//! sample-rate choice, WAV encoding and per-take metrics.

use std::collections::VecDeque;
use std::time::Duration;

/// Frame length streamed to the server.
pub const FRAME_DURATION: Duration = Duration::from_millis(100);
/// The server accepts 16–48 kHz.
pub const MIN_SAMPLE_RATE: u32 = 16_000;
pub const MAX_SAMPLE_RATE: u32 = 48_000;
/// Prefix that marks a saved `input_device_id` as a native device, so it is
/// never mistaken for a WebKit media device id (and vice versa).
pub const NATIVE_DEVICE_PREFIX: &str = "native:";

/// Convert a float sample in [-1, 1] to s16, clamping out-of-range input.
pub fn f32_to_i16(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

/// Average interleaved channels into mono s16, calling `emit` per frame.
/// Runs on the realtime audio thread: no allocation.
#[inline]
pub fn downmix<T>(data: &[T], channels: usize, mut emit: impl FnMut(i16))
where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    let channels = channels.max(1);
    for frame in data.chunks_exact(channels) {
        let mut sum = 0.0f32;
        for &sample in frame {
            sum += sample.to_sample::<f32>();
        }
        emit(f32_to_i16(sum / channels as f32));
    }
}

/// Groups mono samples into fixed-size frames.
pub struct Framer {
    frame_samples: usize,
    buffer: Vec<i16>,
}

impl Framer {
    pub fn new(sample_rate: u32) -> Self {
        let frame_samples =
            ((sample_rate as u128 * FRAME_DURATION.as_millis()) / 1000).max(1) as usize;
        Self {
            frame_samples,
            buffer: Vec::with_capacity(frame_samples),
        }
    }

    /// Append samples; returns every complete frame.
    pub fn push(&mut self, samples: &[i16]) -> Vec<Vec<i16>> {
        let mut frames = Vec::new();
        let mut rest = samples;
        while !rest.is_empty() {
            let take = (self.frame_samples - self.buffer.len()).min(rest.len());
            self.buffer.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.buffer.len() == self.frame_samples {
                frames.push(std::mem::replace(
                    &mut self.buffer,
                    Vec::with_capacity(self.frame_samples),
                ));
            }
        }
        frames
    }

    /// The final partial frame, if any.
    pub fn flush(&mut self) -> Option<Vec<i16>> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buffer))
        }
    }
}

/// Streaming linear-interpolation resampler for mono s16. A take that starts
/// on the built-in microphone and moves to a Bluetooth headset keeps the
/// built-in rate: the stream's rate is fixed when it starts, and the headset
/// (16–24 kHz) is upsampled to it.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Next output position, in input samples after `prev`.
    position: f64,
    prev: Option<i16>,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate.max(1) as f64,
            position: 0.0,
            prev: None,
        }
    }

    /// Resample `input`, appending to `out`.
    pub fn push(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for &current in input {
            let Some(prev) = self.prev else {
                self.prev = Some(current);
                continue;
            };
            while self.position < 1.0 {
                let value = prev as f64 + (current as f64 - prev as f64) * self.position;
                out.push(value.round() as i16);
                self.position += self.step;
            }
            self.position -= 1.0;
            self.prev = Some(current);
        }
    }
}

/// Spots digital silence: runs of exact-zero samples. A live microphone never
/// delivers them, since noise keeps every sample off zero. A Bluetooth
/// headset that is switching to its microphone profile (or has moved to
/// another device) delivers a few samples, then exact zeros for half a
/// second or more.
pub struct DigitalSilence {
    run: usize,
    limit: usize,
}

impl DigitalSilence {
    /// The shortest run that counts.
    pub const RUN: Duration = Duration::from_millis(1);

    pub fn new(sample_rate: u32) -> Self {
        Self {
            run: 0,
            limit: ((sample_rate as u128 * Self::RUN.as_micros()) / 1_000_000).max(2) as usize,
        }
    }

    /// Whether `samples` (continuing the previous chunk) hold a run of zeros
    /// at least [`Self::RUN`] long.
    pub fn push(&mut self, samples: &[i16]) -> bool {
        let mut found = false;
        for &sample in samples {
            if sample == 0 {
                self.run += 1;
                found |= self.run >= self.limit;
            } else {
                self.run = 0;
            }
        }
        found
    }
}

/// How long a microphone has been quiet: every chunk since within
/// [`Self::MARGIN_DB`] of the quietest chunk heard so far (its room noise).
/// The first [`Self::STARTUP`] doesn't count: a built-in microphone opens
/// near-silent, 30 dB under its room noise, and then nothing was quiet.
pub struct Pause {
    sample_rate: u32,
    floor_db: f32,
    quiet: u64,
    seen: u64,
}

impl Pause {
    /// Speech from across the desk is still 20 dB over a built-in
    /// microphone's room noise.
    pub const MARGIN_DB: f32 = 10.0;
    pub const STARTUP: Duration = Duration::from_millis(100);

    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            floor_db: f32::INFINITY,
            quiet: 0,
            seen: 0,
        }
    }

    /// Account for the next chunk (about 10 ms).
    pub fn push(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        self.seen += samples.len() as u64;
        if (self.seen as u128) * 1_000_000 <= self.sample_rate as u128 * Self::STARTUP.as_micros() {
            return;
        }
        let power = samples.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / samples.len() as f32;
        let level = 10.0 * (power + 1.0).log10();
        self.floor_db = self.floor_db.min(level);
        if level <= self.floor_db + Self::MARGIN_DB {
            self.quiet += samples.len() as u64;
        } else {
            self.quiet = 0;
        }
    }

    pub fn quiet_for(&self) -> Duration {
        Duration::from_secs_f64(self.quiet as f64 / self.sample_rate as f64)
    }
}

/// Loudness each millisecond, the last `cap` of them.
struct Envelope {
    block: usize,
    power: f64,
    count: usize,
    values: VecDeque<f32>,
    cap: usize,
}

impl Envelope {
    fn new(block: usize, cap: usize) -> Self {
        Self {
            block,
            power: 0.0,
            count: 0,
            values: VecDeque::with_capacity(cap + 1),
            cap,
        }
    }

    fn push(&mut self, samples: &[i16]) {
        for &sample in samples {
            self.power += (sample as f64).powi(2);
            self.count += 1;
            if self.count == self.block {
                self.values
                    .push_back((10.0 * (self.power / self.block as f64 + 1.0).log10()) as f32);
                if self.values.len() > self.cap {
                    self.values.pop_front();
                }
                self.power = 0.0;
                self.count = 0;
            }
        }
    }

    fn clear(&mut self) {
        self.values.clear();
        self.power = 0.0;
        self.count = 0;
    }
}

/// How far a Bluetooth headset's audio trails the built-in microphone's,
/// from both hearing the same voice: the shift that best lines up their
/// loudness over the last [`Self::WINDOW`] of the headset's audio. Loudness,
/// not the waveforms: the two microphones color a voice differently.
pub struct Aligner {
    block: usize,
    local: Envelope,
    headset: Envelope,
}

impl Aligner {
    /// Bluetooth delays a headset microphone by a tenth of a second or two.
    pub const MAX_LAG: Duration = Duration::from_millis(500);
    pub const WINDOW: Duration = Duration::from_millis(400);
    /// How well the two must agree at the best shift.
    pub const MIN_CORRELATION: f32 = 0.7;
    /// Loudness that varies less than this over the window is no voice to
    /// line up on.
    pub const MIN_SPREAD_DB: f32 = 4.0;

    /// For two streams at `sample_rate`.
    pub fn new(sample_rate: u32) -> Self {
        let block = (sample_rate / 1000).max(1) as usize;
        let window = Self::WINDOW.as_millis() as usize;
        Self {
            block,
            local: Envelope::new(block, window + Self::MAX_LAG.as_millis() as usize),
            headset: Envelope::new(block, window),
        }
    }

    pub fn push_local(&mut self, samples: &[i16]) {
        self.local.push(samples);
    }

    pub fn push_headset(&mut self, samples: &[i16]) {
        self.headset.push(samples);
    }

    /// Start over on the headset: its audio broke off.
    pub fn reset_headset(&mut self) {
        self.headset.clear();
    }

    /// Samples by which the headset trails, and how well the two agree there;
    /// None until a voice lines them up.
    pub fn lag(&self) -> Option<(usize, f32)> {
        let window = Self::WINDOW.as_millis() as usize;
        let headset = &self.headset.values;
        let local = &self.local.values;
        if headset.len() < window || local.len() < window {
            return None;
        }
        let (h_mean, h_spread) = mean_and_spread(headset.iter().copied());
        if h_spread < Self::MIN_SPREAD_DB {
            return None;
        }
        let max_lag = (local.len() - window).min(Self::MAX_LAG.as_millis() as usize);
        let mut best: Option<(usize, f32)> = None;
        for lag in 0..=max_lag {
            let end = local.len() - lag;
            let segment = local.range(end - window..end);
            let (l_mean, l_spread) = mean_and_spread(segment.clone().copied());
            if l_spread < Self::MIN_SPREAD_DB {
                continue;
            }
            let covariance = segment
                .zip(headset.iter())
                .map(|(&l, &h)| (l - l_mean) * (h - h_mean))
                .sum::<f32>()
                / window as f32;
            let correlation = covariance / (l_spread * h_spread);
            if best.is_none_or(|(_, c)| correlation > c) {
                best = Some((lag, correlation));
            }
        }
        best.filter(|&(_, c)| c >= Self::MIN_CORRELATION)
            .map(|(lag, c)| (lag * self.block, c))
    }
}

/// Mean and standard deviation.
fn mean_and_spread(values: impl Iterator<Item = f32> + Clone) -> (f32, f32) {
    let n = values.clone().count().max(1) as f32;
    let mean = values.clone().sum::<f32>() / n;
    let variance = values.map(|v| (v - mean).powi(2)).sum::<f32>() / n;
    (mean, variance.sqrt())
}

pub fn native_device_id(name: &str) -> String {
    format!("{NATIVE_DEVICE_PREFIX}{name}")
}

/// Index of the saved device among `names`, or `None` for the system
/// default (nothing saved, a non-native id, or the device is gone).
pub fn pick_device(names: &[String], saved_id: Option<&str>) -> Option<usize> {
    let wanted = saved_id?.strip_prefix(NATIVE_DEVICE_PREFIX)?;
    names.iter().position(|name| name == wanted)
}

/// Pick a capture rate the server accepts. Prefer the device default; else
/// the closest rate inside a supported `(min, max)` range.
pub fn choose_sample_rate(default_rate: u32, supported: &[(u32, u32)]) -> Option<u32> {
    if (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&default_rate) {
        return Some(default_rate);
    }
    // Closest accepted rate to the default within any supported range.
    supported
        .iter()
        .filter_map(|&(min, max)| {
            let low = min.max(MIN_SAMPLE_RATE);
            let high = max.min(MAX_SAMPLE_RATE);
            (low <= high).then(|| default_rate.clamp(low, high))
        })
        .min_by_key(|&rate| rate.abs_diff(default_rate))
}

/// Mono s16 WAV bytes for the batch fallback upload.
pub fn encode_wav(pcm: &[i16], sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::with_capacity(44 + pcm.len() * 2));
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).map_err(|e| e.to_string())?;
        let mut samples = writer.get_i16_writer(pcm.len() as u32);
        for &sample in pcm {
            samples.write_sample(sample);
        }
        samples.flush().map_err(|e| e.to_string())?;
        writer.finalize().map_err(|e| e.to_string())?;
    }
    Ok(cursor.into_inner())
}

/// Timing evidence for one take, logged when it ends.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TakeMetrics {
    /// Key-down to the input stream running.
    pub stream_open: Option<Duration>,
    /// Key-down to the first non-zero sample from the device.
    pub first_sound: Option<Duration>,
    /// Leading zero samples before the first non-zero one.
    pub leading_zero_samples: u64,
    pub samples: u64,
    pub sample_rate: u32,
    /// Key-down to key-up.
    pub wall: Duration,
    /// Samples the ring buffer had to drop (should always be zero).
    pub dropped_samples: u64,
    /// Key-down to the move from the built-in microphone to a Bluetooth
    /// headset; `None` when the take never moved.
    pub handoff: Option<Duration>,
    /// Times the headset went silent and the take fell back to the built-in
    /// microphone.
    pub headset_dropouts: u32,
    /// How far the headset's audio trailed the built-in microphone's, when
    /// the take moved to it on a lineup.
    pub headset_lag: Option<Duration>,
}

impl TakeMetrics {
    pub fn audio_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples as f64 / self.sample_rate as f64
        }
    }

    /// Audio length over wall-clock length; about 1 for a healthy capture
    /// (a little under 1 because the device opens after key-down).
    pub fn audio_wall_ratio(&self) -> f64 {
        let wall = self.wall.as_secs_f64();
        if wall <= 0.0 {
            0.0
        } else {
            self.audio_seconds() / wall
        }
    }

    pub fn summary(&self) -> String {
        fn ms(d: Option<Duration>) -> String {
            d.map(|d| format!("{:.0}ms", d.as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "n/a".into())
        }
        format!(
            "keydown→stream {} keydown→first-sound {} keydown→handoff {} headset-lag {} headset-dropouts {} leading-zeros {:.0}ms audio {:.2}s wall {:.2}s ratio {:.3} rate {}Hz dropped {}",
            ms(self.stream_open),
            ms(self.first_sound),
            ms(self.handoff),
            ms(self.headset_lag),
            self.headset_dropouts,
            if self.sample_rate == 0 {
                0.0
            } else {
                self.leading_zero_samples as f64 * 1000.0 / self.sample_rate as f64
            },
            self.audio_seconds(),
            self.wall.as_secs_f64(),
            self.audio_wall_ratio(),
            self.sample_rate,
            self.dropped_samples,
        )
    }
}

/// Below this, a microphone picks up hum (fans, air conditioning, mains)
/// rather than words. A fan on the user's microphone hummed at 120–140 Hz, as
/// loud as their voice. On 27 of their takes, cutting it left Whisper's
/// transcripts and the voice detector's results essentially unchanged.
pub const LOW_CUT_HZ: f64 = 200.0;

/// An 8th-order Butterworth high-pass at [`LOW_CUT_HZ`], as four biquads.
/// Steep enough to take a hum at 130 Hz down ~30 dB.
pub struct LowCut {
    stages: [Biquad; 4],
}

impl LowCut {
    pub fn new(sample_rate: u32) -> Self {
        // The Q of each pole pair of an 8th-order Butterworth.
        let q = |k: f64| 1.0 / (2.0 * ((2.0 * k - 1.0) * std::f64::consts::PI / 16.0).cos());
        Self {
            stages: [1.0, 2.0, 3.0, 4.0].map(|k| Biquad::high_pass(sample_rate, LOW_CUT_HZ, q(k))),
        }
    }

    /// Filter samples in place.
    pub fn process(&mut self, samples: &mut [i16]) {
        for sample in samples {
            let mut value = *sample as f64;
            for stage in &mut self.stages {
                value = stage.process(value);
            }
            *sample = value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
    }
}

/// One second-order section (direct form I).
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    /// The Audio EQ Cookbook high-pass.
    fn high_pass(sample_rate: u32, cutoff: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * cutoff / sample_rate as f64;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b: [
                (1.0 + cos) / 2.0 / a0,
                -(1.0 + cos) / a0,
                (1.0 + cos) / 2.0 / a0,
            ],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// How often the recording HUD gets a new input level.
pub const LEVEL_INTERVAL: Duration = Duration::from_millis(50);
/// Reported for digital silence, which would otherwise be -inf dBFS.
pub const LEVEL_FLOOR_DBFS: f32 = -100.0;

/// Measures input loudness as RMS dBFS over fixed windows.
pub struct LevelMeter {
    window: usize,
    count: usize,
    sum_squares: f64,
}

impl LevelMeter {
    pub fn new(sample_rate: u32) -> Self {
        let window = ((sample_rate as u128 * LEVEL_INTERVAL.as_millis()) / 1000).max(1) as usize;
        Self {
            window,
            count: 0,
            sum_squares: 0.0,
        }
    }

    /// Add samples, calling `emit` with the level of each completed window.
    pub fn push(&mut self, samples: &[i16], mut emit: impl FnMut(f32)) {
        for &sample in samples {
            let value = sample as f64 / 32768.0;
            self.sum_squares += value * value;
            self.count += 1;
            if self.count == self.window {
                emit(dbfs(self.sum_squares / self.count as f64));
                self.count = 0;
                self.sum_squares = 0.0;
            }
        }
    }
}

/// Mean square of full-scale-normalized samples, in dBFS.
fn dbfs(mean_square: f64) -> f32 {
    if mean_square <= 0.0 {
        return LEVEL_FLOOR_DBFS;
    }
    ((10.0 * mean_square.log10()) as f32).max(LEVEL_FLOOR_DBFS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(sample_rate: u32, samples: &[i16]) -> Vec<f32> {
        let mut meter = LevelMeter::new(sample_rate);
        let mut out = Vec::new();
        meter.push(samples, |db| out.push(db));
        out
    }

    #[test]
    fn level_meter_reports_one_level_per_50ms() {
        // 48 kHz: 2400 samples per window; the remainder carries over.
        let mut meter = LevelMeter::new(48_000);
        let mut out = Vec::new();
        meter.push(&vec![1000; 5000], |db| out.push(db));
        assert_eq!(out.len(), 2);
        meter.push(&vec![1000; 2200], |db| out.push(db));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn level_meter_measures_rms_dbfs() {
        let silence = levels(16_000, &vec![0; 800]);
        assert_eq!(silence, vec![LEVEL_FLOOR_DBFS]);

        let full: Vec<i16> = (0..800)
            .map(|i| if i % 2 == 0 { 32767 } else { -32768 })
            .collect();
        assert!(levels(16_000, &full)[0].abs() < 0.01);

        let half: Vec<i16> = (0..800)
            .map(|i| if i % 2 == 0 { 16384 } else { -16384 })
            .collect();
        assert!((levels(16_000, &half)[0] + 6.02).abs() < 0.05);
    }

    fn tone_gain_db(sample_rate: u32, hz: f64) -> f64 {
        let tone: Vec<i16> = (0..sample_rate)
            .map(|i| {
                (10_000.0 * (2.0 * std::f64::consts::PI * hz * i as f64 / sample_rate as f64).sin())
                    as i16
            })
            .collect();
        let mut filtered = tone.clone();
        LowCut::new(sample_rate).process(&mut filtered);
        // Skip the first 100 ms while the filter settles.
        let rms = |s: &[i16]| {
            let tail = &s[s.len() / 10..];
            (tail.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt()
        };
        20.0 * (rms(&filtered) / rms(&tone)).log10()
    }

    #[test]
    fn low_cut_removes_hum_and_keeps_the_voice() {
        for rate in [16_000, 44_100, 48_000] {
            assert!(tone_gain_db(rate, 130.0) < -25.0, "hum at {rate} Hz");
            assert!(tone_gain_db(rate, 60.0) < -60.0, "mains at {rate} Hz");
            assert!(tone_gain_db(rate, 400.0).abs() < 0.5, "voice at {rate} Hz");
            assert!(
                tone_gain_db(rate, 2_000.0).abs() < 0.5,
                "voice at {rate} Hz"
            );
        }
    }

    #[test]
    fn low_cut_keeps_digital_silence_silent() {
        // Leading zeros mark how long the device took to start.
        let mut silence = vec![0i16; 4800];
        LowCut::new(48_000).process(&mut silence);
        assert!(silence.iter().all(|&s| s == 0));
    }

    #[test]
    fn float_samples_convert_and_clamp() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(-1.0), -i16::MAX);
        assert_eq!(f32_to_i16(2.0), i16::MAX);
        assert_eq!(f32_to_i16(-2.0), -i16::MAX);
        assert_eq!(f32_to_i16(f32::NAN), 0);
    }

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::new();
        downmix(&[1.0f32, 0.0, -1.0, -1.0], 2, |s| out.push(s));
        assert_eq!(out, vec![f32_to_i16(0.5), -i16::MAX]);
        let mut mono = Vec::new();
        downmix(&[0.5f32, -0.5], 1, |s| mono.push(s));
        assert_eq!(mono, vec![f32_to_i16(0.5), f32_to_i16(-0.5)]);
        let mut ints = Vec::new();
        downmix(&[i16::MAX, 0i16], 2, |s| ints.push(s));
        assert!((ints[0] - i16::MAX / 2).abs() <= 1);
    }

    #[test]
    fn digital_silence_finds_zero_runs_across_chunks() {
        // 1 ms at 24 kHz is 24 samples.
        let mut silence = DigitalSilence::new(24_000);
        assert!(!silence.push(&[5, -3, 0, 0, 7]));
        assert!(!silence.push(&[0; 12]));
        assert!(silence.push(&[0; 12]));
        assert!(!silence.push(&[1, 0, 2, -1]));
    }

    #[test]
    fn digital_silence_ignores_quiet_noise() {
        let mut silence = DigitalSilence::new(48_000);
        let hiss: Vec<i16> = (0..48_000).map(|i| [1, -1, 0, 2][i % 4]).collect();
        assert!(!silence.push(&hiss));
    }

    /// A second of speech-like sound at 48 kHz: noise in syllables of
    /// varying loudness, with gaps, plus room noise. `seed` picks the noise.
    fn syllables(seconds: f32, seed: u64) -> Vec<f32> {
        let mut state = seed;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        };
        let len = (48_000.0 * seconds) as usize;
        let mut out = vec![0.0; len];
        let mut at = 0;
        let mut syllable = 0usize;
        while at < len {
            syllable += 1;
            let length = 2_400 + (syllable * 3_517) % 7_200; // 50–200 ms
            let gap = (syllable * 1_931) % 4_800; // 0–100 ms
            let loudness = 0.2 + ((syllable * 7_919) % 100) as f32 / 125.0;
            for sample in out.iter_mut().skip(at).take(length) {
                *sample = next() * loudness;
            }
            at += length + gap;
        }
        out
    }

    fn mic(voice: &[f32], gain: f32, delay: usize, noise_seed: u64) -> Vec<i16> {
        let room = syllables(voice.len() as f32 / 48_000.0 + 1.0, noise_seed);
        (0..voice.len())
            .map(|i| {
                let said = if i >= delay { voice[i - delay] } else { 0.0 };
                (said * gain + room[i] * 30.0) as i16
            })
            .collect()
    }

    #[test]
    fn aligner_finds_how_far_the_headset_trails() {
        let voice: Vec<f32> = syllables(1.5, 7).iter().map(|v| v * 1_000.0).collect();
        // The built-in microphone hears the voice quietly; the headset
        // loudly, 180 ms late.
        let local = mic(&voice, 1.0, 0, 11);
        let headset = mic(&voice, 6.0, 8_640, 13);
        let mut aligner = Aligner::new(48_000);
        for (l, h) in local.chunks(480).zip(headset.chunks(480)) {
            aligner.push_local(l);
            aligner.push_headset(h);
        }
        let (lag, correlation) = aligner.lag().expect("lined up");
        assert!((lag as i64 - 8_640).abs() <= 96, "lag {lag}");
        assert!(correlation > 0.9, "{correlation}");
    }

    #[test]
    fn aligner_needs_a_voice() {
        let mut aligner = Aligner::new(48_000);
        let hiss: Vec<i16> = (0..48_000).map(|i| [3, -2, 5, -4][i % 4]).collect();
        for chunk in hiss.chunks(480) {
            aligner.push_local(chunk);
            aligner.push_headset(chunk);
        }
        assert_eq!(aligner.lag(), None);
    }

    #[test]
    fn a_pause_is_time_back_at_the_room_noise() {
        let tone = |amplitude: f32| -> Vec<i16> {
            (0..480)
                .map(|i| ((i as f32 * 0.3).sin() * amplitude) as i16)
                .collect()
        };
        let room = tone(20.0);
        let speech = tone(400.0); // 26 dB over the room
        let mut pause = Pause::new(48_000);
        // The microphone opening near-silent sets no floor.
        for _ in 0..10 {
            pause.push(&[0; 480]);
        }
        assert_eq!(pause.quiet_for(), Duration::ZERO);
        for _ in 0..10 {
            pause.push(&room);
        }
        assert_eq!(pause.quiet_for(), Duration::from_millis(100));
        pause.push(&speech);
        assert_eq!(pause.quiet_for(), Duration::ZERO);
        for _ in 0..30 {
            pause.push(&room);
        }
        assert_eq!(pause.quiet_for(), Duration::from_millis(300));
    }

    #[test]
    fn resampler_keeps_the_rate_ratio_across_chunks() {
        let mut resampler = Resampler::new(16_000, 48_000);
        let mut out = Vec::new();
        for _ in 0..10 {
            resampler.push(&[1000; 1600], &mut out);
        }
        // 1 s in, 1 s out (less one input sample of lookahead).
        assert!((out.len() as i64 - 48_000).abs() <= 3, "{}", out.len());
        assert!(out.iter().all(|&s| s == 1000));

        let mut down = Resampler::new(48_000, 16_000);
        let mut out = Vec::new();
        down.push(&vec![0; 48_000], &mut out);
        assert!((out.len() as i64 - 16_000).abs() <= 1);
    }

    #[test]
    fn resampler_interpolates_between_samples() {
        let mut resampler = Resampler::new(24_000, 48_000);
        let mut out = Vec::new();
        resampler.push(&[0, 100, 200], &mut out);
        assert_eq!(out, vec![0, 50, 100, 150]);
    }

    #[test]
    fn framer_emits_100ms_frames_and_flushes_the_tail() {
        let mut framer = Framer::new(16_000);
        assert!(framer.push(&[1; 1000]).is_empty());
        let frames = framer.push(&[2; 2500]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 1600);
        assert_eq!(frames[0][999], 1);
        assert_eq!(frames[0][1000], 2);
        assert_eq!(framer.flush(), Some(vec![2; 300]));
        assert_eq!(framer.flush(), None);
    }

    #[test]
    fn saved_native_device_is_found_and_anything_else_means_default() {
        let names = vec!["MacBook Pro Microphone".to_string(), "AirPods".to_string()];
        assert_eq!(
            pick_device(&names, Some(&native_device_id("AirPods"))),
            Some(1)
        );
        assert_eq!(pick_device(&names, None), None);
        assert_eq!(
            pick_device(&names, Some(&native_device_id("Unplugged"))),
            None
        );
        // A legacy WebKit media device id.
        assert_eq!(pick_device(&names, Some("3f9a0c1d")), None);
        assert_eq!(pick_device(&names, Some("AirPods")), None);
    }

    #[test]
    fn sample_rate_prefers_default_within_server_limits() {
        assert_eq!(choose_sample_rate(48_000, &[]), Some(48_000));
        assert_eq!(choose_sample_rate(16_000, &[]), Some(16_000));
        assert_eq!(choose_sample_rate(44_100, &[]), Some(44_100));
        // 96 kHz default: use a supported rate the server accepts.
        assert_eq!(
            choose_sample_rate(96_000, &[(96_000, 96_000), (8_000, 48_000)]),
            Some(48_000)
        );
        assert_eq!(choose_sample_rate(8_000, &[(8_000, 8_000)]), None);
        assert_eq!(choose_sample_rate(8_000, &[(8_000, 22_050)]), Some(16_000));
    }

    #[test]
    fn wav_roundtrips() {
        let bytes = encode_wav(&[0, 1, -1, i16::MAX], 16_000).unwrap();
        let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(samples, vec![0, 1, -1, i16::MAX]);
    }

    #[test]
    fn audio_wall_ratio() {
        let metrics = TakeMetrics {
            samples: 48_000,
            sample_rate: 48_000,
            wall: Duration::from_secs(2),
            ..Default::default()
        };
        assert!((metrics.audio_wall_ratio() - 0.5).abs() < 1e-9);
        assert_eq!(TakeMetrics::default().audio_wall_ratio(), 0.0);
        assert!(metrics.summary().contains("ratio 0.500"));
    }
}
