//! The voice frame by frame: pitch (YIN) and loudness, followed as the
//! audio arrives. Python: `PitchTrack` and `analyse` in
//! `backend/services/prosody.py`.
//!
//! Frames come every 10 ms, each 40 ms of audio resampled to 16 kHz. A frame
//! is analysed once its window has arrived, well under a millisecond per
//! 100 ms of audio, so nothing waits on it after release.

/// Seconds between frames.
pub const HOP: f64 = 0.01;
const ANALYSIS_RATE: f64 = 16_000.0;
const WINDOW: usize = 640;
const MIN_HZ: f64 = 70.0;
const MAX_HZ: f64 = 500.0;
/// `int(ANALYSIS_RATE / MAX_HZ)`.
const MIN_LAG: usize = (ANALYSIS_RATE / MAX_HZ) as usize;
/// `ceil(ANALYSIS_RATE / MIN_HZ)`: 229 (228.6 is no whole number).
const MAX_LAG: usize = (ANALYSIS_RATE / MIN_HZ) as usize + 1;
/// Samples each lag's difference is summed over.
const SUMMED: usize = WINDOW - MAX_LAG;
/// YIN: the first lag whose normalized difference dips below this is the
/// period.
const YIN_THRESHOLD: f64 = 0.1;
/// A frame is voiced when its best lag's normalized difference is under
/// this.
const MAX_APERIODICITY: f64 = 0.35;
/// Quieter frames are never voiced (dB relative to full scale).
const SILENCE_DB: f64 = -55.0;

/// Pitch in Hz (NaN where unvoiced) and loudness in dB, one of each per
/// frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frames {
    pub hz: Vec<f64>,
    pub db: Vec<f64>,
}

/// Pitch and loudness of a recording, measured as its audio arrives. Only
/// the last window of audio is kept.
#[derive(Clone, Debug)]
pub struct PitchTrack {
    rate: f64,
    /// The audio from recording sample `start` on, scaled to [-1, 1).
    audio: Vec<f32>,
    start: i64,
    received: i64,
    frames: Frames,
    /// Frames analysed.
    count: i64,
    /// A boxcar over this many source samples keeps 48 kHz audio from
    /// aliasing at 16 kHz.
    smooth: usize,
    /// Each window sample's place around its frame's center, in source
    /// samples.
    offsets: Vec<f64>,
}

impl PitchTrack {
    pub fn new(rate: u32) -> Self {
        let rate = f64::from(rate);
        let step = rate / ANALYSIS_RATE;
        Self {
            rate,
            audio: Vec::new(),
            start: 0,
            received: 0,
            frames: Frames::default(),
            count: 0,
            smooth: (rate / ANALYSIS_RATE).round_ties_even().max(1.0) as usize,
            offsets: (0..WINDOW)
                .map(|i| (i as f64 - (WINDOW / 2) as f64) * step)
                .collect(),
        }
    }

    /// Add int16 samples and analyse every frame they complete.
    pub fn feed(&mut self, pcm: &[i16]) {
        self.audio
            .extend(pcm.iter().map(|&sample| f32::from(sample) / 32768.0));
        self.received += pcm.len() as i64;
        // Frame k is centered at k * HOP; it needs audio to its window's end.
        let reach = self.received as f64 - self.offsets[WINDOW - 1] - self.smooth as f64;
        let last = (reach / (HOP * self.rate)).floor() as i64;
        if last < self.count {
            return;
        }
        for k in self.count..=last {
            let window = self.resample(k as f64 * HOP * self.rate);
            let (hz, db) = analyse(&window);
            self.frames.hz.push(hz);
            self.frames.db.push(db);
        }
        self.count = last + 1;
        // Keep from the next frame's window on.
        let next = self.count as f64 * HOP * self.rate + self.offsets[0];
        let keep_from = next as i64 - self.smooth as i64 - 1;
        if keep_from > self.start {
            let drop = ((keep_from - self.start) as usize).min(self.audio.len());
            self.audio.drain(..drop);
            self.start = keep_from;
        }
    }

    /// The 16 kHz window around `center` (a source sample), each sample
    /// the mean of the boxcar's linear interpolations.
    fn resample(&self, center: f64) -> [f32; WINDOW] {
        let shifts = self.smooth;
        let mid = (shifts - 1) as f64 / 2.0;
        let mut window = [0f32; WINDOW];
        for (sample, offset) in window.iter_mut().zip(&self.offsets) {
            let position = center + offset - self.start as f64;
            let mut total = 0f32;
            for shift in 0..shifts {
                total += interp(position + (shift as f64 - mid), &self.audio) as f32;
            }
            *sample = total / shifts as f32;
        }
        window
    }

    pub fn frames(&self) -> &Frames {
        &self.frames
    }
}

/// `np.interp(x, arange(len(audio)), audio, left=0, right=0)`.
fn interp(x: f64, audio: &[f32]) -> f64 {
    let last = audio.len() as f64 - 1.0;
    if audio.is_empty() || x < 0.0 || x > last || x.is_nan() {
        return 0.0;
    }
    if x == last {
        return f64::from(audio[audio.len() - 1]);
    }
    let j = x.floor() as usize;
    let low = f64::from(audio[j]);
    let slope = f64::from(audio[j + 1]) - low;
    slope * (x - j as f64) + low
}

/// Pitch (YIN; Hz, NaN where unvoiced) and loudness (dB) of one 16 kHz
/// window.
pub fn analyse(frame: &[f32; WINDOW]) -> (f64, f64) {
    let x: Vec<f64> = frame.iter().map(|&s| f64::from(s)).collect();
    let mut squares = [0f64; WINDOW + 1];
    for (i, sample) in x.iter().enumerate() {
        squares[i + 1] = squares[i] + sample * sample;
    }
    // numpy keeps loudness in float32.
    let db = f64::from((10.0 * (squares[WINDOW] / WINDOW as f64 + 1e-10).log10()) as f32);

    let head = squares[SUMMED];
    let mut difference = [0f64; MAX_LAG + 1];
    for (lag, value) in difference.iter_mut().enumerate() {
        let shifted = squares[lag + SUMMED] - squares[lag];
        let correlation: f64 = x[..SUMMED]
            .iter()
            .zip(&x[lag..lag + SUMMED])
            .map(|(a, b)| a * b)
            .sum();
        *value = (head + shifted - 2.0 * correlation).max(0.0);
    }
    let mut normalized = [1f64; MAX_LAG + 1];
    let mut running = 0.0;
    for lag in 1..=MAX_LAG {
        running += difference[lag];
        normalized[lag] = difference[lag] * lag as f64 / running.max(1e-12);
    }

    let window = &normalized[MIN_LAG..=MAX_LAG];
    let last = window.len() - 1;
    let mut lag = window
        .iter()
        .position(|&value| value < YIN_THRESHOLD)
        .unwrap_or_else(|| argmin(window));
    lag = slide_down(window, lag);
    // A strong second harmonic dips first, at half the period: take the
    // period itself when it is the better fit.
    let doubled = 2 * (lag + MIN_LAG) - MIN_LAG;
    if doubled < last - 1 {
        let near = slide_down(window, doubled.saturating_sub(2).min(last));
        if window[near] < window[lag] {
            lag = near;
        }
    }
    let aperiodicity = window[lag];
    // Parabolic interpolation between neighbouring lags.
    let left = window[lag.saturating_sub(1)];
    let right = window[(lag + 1).min(last)];
    let curve = left - 2.0 * aperiodicity + right;
    let offset = if curve.abs() > 1e-9 {
        (0.5 * (left - right) / curve).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let hz = ANALYSIS_RATE / ((lag + MIN_LAG) as f64 + offset);
    let voiced = aperiodicity < MAX_APERIODICITY && db > SILENCE_DB;
    (if voiced { hz } else { f64::NAN }, db)
}

fn argmin(values: &[f64]) -> usize {
    let mut best = 0;
    for (index, value) in values.iter().enumerate() {
        if *value < values[best] {
            best = index;
        }
    }
    best
}

/// From `lag`, step to larger lags while the difference keeps falling.
fn slide_down(window: &[f64], mut lag: usize) -> usize {
    while lag + 1 < window.len() && window[lag + 1] < window[lag] {
        lag += 1;
    }
    lag
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, seconds: f64) -> Vec<i16> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                (0.3 * 32767.0 * (2.0 * std::f64::consts::PI * hz * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn follows_a_tone_fed_in_chunks() {
        for (rate, hz) in [
            (16_000, 120.0),
            (48_000, 220.0),
            (44_100, 180.0),
            (24_000, 95.0),
        ] {
            let mut track = PitchTrack::new(rate);
            for chunk in tone(rate, hz, 1.0).chunks(rate as usize / 10 + 7) {
                track.feed(chunk);
            }
            let frames = track.frames();
            assert!(frames.hz.len() >= 95, "{rate}: {} frames", frames.hz.len());
            let middle = &frames.hz[5..frames.hz.len() - 5];
            for value in middle {
                assert!(
                    (value - hz).abs() / hz < 0.01,
                    "{rate} Hz: {value} for {hz}"
                );
            }
        }
    }

    #[test]
    fn chunking_never_changes_a_frame() {
        let pcm = tone(48_000, 180.0, 0.8);
        let mut whole = PitchTrack::new(48_000);
        whole.feed(&pcm);
        let mut pieces = PitchTrack::new(48_000);
        for chunk in pcm.chunks(997) {
            pieces.feed(chunk);
        }
        assert_eq!(whole.frames().db, pieces.frames().db);
        let same = |a: f64, b: f64| a == b || (a.is_nan() && b.is_nan());
        assert!(whole
            .frames()
            .hz
            .iter()
            .zip(&pieces.frames().hz)
            .all(|(a, b)| same(*a, *b)));
    }

    #[test]
    fn silence_is_unvoiced() {
        let mut track = PitchTrack::new(16_000);
        track.feed(&vec![0; 16_000]);
        let frames = track.frames();
        assert!(frames.hz.iter().all(|hz| hz.is_nan()));
        assert!(frames.db.iter().all(|db| *db < -90.0));
    }
}
