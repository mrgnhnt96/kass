//! Is anyone speaking? A voice detector that runs ahead of Whisper.
//! Python: `backend/services/speech_detect.py`.
//!
//! Whisper writes something for any audio it is given: on a take with no
//! voice in it, it returns a phrase from its training subtitles ("Thank
//! you."). Loudness can't tell a voice either, since room noise on one
//! microphone is as loud as quiet speech on another. Silero VAD
//! (`backend/assets/silero_vad.onnx`) is a small model trained to tell a
//! voice from other sound. It costs about 3 ms per second of audio on one
//! CPU core, so a dictation checks its audio as it arrives.

use std::path::Path;
use std::sync::{Arc, Mutex};

use ort::session::Session;
use ort::value::Tensor;

/// The rate Silero runs at.
pub const RATE: u32 = 16_000;
/// 32 ms, the window Silero is trained on at 16 kHz.
pub const WINDOW: usize = 512;
/// Samples of the previous window Silero sees before each one.
const CONTEXT: usize = 64;
/// Silero's own default. Silent takes stay below 0.2, speech reaches 1.0.
pub const VOICE_PROBABILITY: f32 = 0.5;
const STATE: usize = 2 * 128;

/// The Silero model, loaded once and shared by every dictation.
pub struct VoiceModel {
    session: Mutex<Session>,
}

impl VoiceModel {
    /// Load the model on one CPU thread, as the Python worker does.
    pub fn load(path: &Path) -> Result<Arc<Self>, String> {
        let environment = ort::init()
            .with_name("kass-voice")
            .build()
            .map_err(|e| format!("Could not start ONNX Runtime: {e}"))?;
        let session = Session::builder(&environment)
            .and_then(|builder| Ok(builder.with_inter_threads(1)?))
            .and_then(|builder| Ok(builder.with_intra_threads(1)?))
            .and_then(|mut builder| builder.commit_from_file(path))
            .map_err(|e| format!("Could not load the voice detector {}: {e}", path.display()))?;
        Ok(Arc::new(Self {
            session: Mutex::new(session),
        }))
    }

    /// The voice probability of one window, given the context before it,
    /// carrying Silero's recurrent state.
    fn probability(&self, frame: Vec<f32>, state: &mut Vec<f32>) -> Result<f32, String> {
        let input =
            Tensor::from_array(([1usize, CONTEXT + WINDOW], frame)).map_err(|e| e.to_string())?;
        let carried = Tensor::from_array(([2usize, 1, 128], std::mem::take(state)))
            .map_err(|e| e.to_string())?;
        let rate = Tensor::from_array(((), vec![i64::from(RATE)])).map_err(|e| e.to_string())?;
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outputs = session
            .run(ort::inputs!["input" => input, "state" => carried, "sr" => rate])
            .map_err(|e| e.to_string())?;
        let (_, probability) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let (_, next) = outputs[1]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        *state = next.to_vec();
        Ok(probability[0])
    }
}

/// Voice detection over a stream of 16-bit PCM at any sample rate.
///
/// `feed` audio as it arrives; `heard(start, end)` says whether any voice
/// was detected between two sample offsets of the stream. A voice detected
/// before `ignore_before` (source samples) doesn't count: the dictation start
/// cue can be picked up there by the microphone, and a loud chime reads as a
/// voice to Silero. Without a model every take counts as speech, as before
/// the detector existed.
pub struct SpeechDetector {
    rate: u32,
    /// In 16 kHz samples.
    ignore_before: f64,
    model: Option<Arc<VoiceModel>>,
    state: Vec<f32>,
    context: Vec<f32>,
    /// 16 kHz samples not yet run.
    buffer: Vec<f32>,
    /// Source samples not yet resampled.
    carry: Vec<f32>,
    /// The next 16 kHz sample, in source samples from `carry[0]`.
    position: f64,
    /// 16 kHz samples run through the model.
    analyzed: i64,
    /// 16 kHz offsets of voiced windows.
    voiced: Vec<i64>,
    /// 16 kHz spans of Kass's own sounds.
    ignored: Vec<(f64, f64)>,
}

impl SpeechDetector {
    pub fn new(rate: u32, ignore_before: i64, model: Option<Arc<VoiceModel>>) -> Self {
        Self {
            rate,
            ignore_before: to_16k(ignore_before, rate),
            model,
            state: vec![0.0; STATE],
            context: vec![0.0; CONTEXT],
            buffer: Vec::new(),
            carry: Vec::new(),
            position: 0.0,
            analyzed: 0,
            voiced: Vec::new(),
            ignored: Vec::new(),
        }
    }

    /// Whether the model loaded. Without it, every take is speech and no
    /// pause is ever found.
    pub fn available(&self) -> bool {
        self.model.is_some()
    }

    pub fn feed(&mut self, pcm: &[i16]) -> Result<(), String> {
        let Some(model) = self.model.clone() else {
            return Ok(());
        };
        let samples: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let resampled = self.resample(samples);
        self.buffer.extend_from_slice(&resampled);
        let mut used = 0;
        while self.buffer.len() - used >= WINDOW {
            let window = &self.buffer[used..used + WINDOW];
            let mut frame = Vec::with_capacity(CONTEXT + WINDOW);
            frame.extend_from_slice(&self.context);
            frame.extend_from_slice(window);
            let probability = model.probability(frame, &mut self.state)?;
            self.context.copy_from_slice(&window[WINDOW - CONTEXT..]);
            used += WINDOW;
            let offset = self.analyzed;
            if probability >= VOICE_PROBABILITY
                && offset as f64 >= self.ignore_before
                && !self.is_ignored(offset as f64)
            {
                self.voiced.push(offset);
            }
            self.analyzed += WINDOW as i64;
        }
        self.buffer.drain(..used);
        Ok(())
    }

    /// Don't take anything between two source sample offsets for a voice: a
    /// sound Kass played there (the style cue) that the microphone may have
    /// picked up. The span may reach past the audio fed so far.
    pub fn ignore(&mut self, start: i64, end: i64) {
        self.ignored
            .push((to_16k(start, self.rate), to_16k(end, self.rate)));
        let voiced = std::mem::take(&mut self.voiced);
        self.voiced = voiced
            .into_iter()
            .filter(|&offset| !self.is_ignored(offset as f64))
            .collect();
    }

    fn is_ignored(&self, offset: f64) -> bool {
        self.ignored
            .iter()
            .any(|&(low, high)| low < offset + WINDOW as f64 && offset < high)
    }

    /// Whether a voice was detected between source sample offsets.
    pub fn heard(&self, start: i64, end: i64) -> bool {
        if self.model.is_none() {
            return true;
        }
        let (low, high) = (to_16k(start, self.rate), to_16k(end, self.rate));
        self.voiced
            .iter()
            .any(|&offset| low < (offset + WINDOW as i64) as f64 && (offset as f64) < high)
    }

    /// The source sample offset where a voice was first detected, or None
    /// before one. 0 without the model: every take is speech from its start.
    pub fn first_voice(&self) -> Option<i64> {
        if self.model.is_none() {
            return Some(0);
        }
        self.voiced.first().map(|&offset| self.to_source(offset))
    }

    /// Source samples since the last voice, over the audio run so far. 0
    /// without the model, so no pause is ever found.
    pub fn quiet(&self) -> i64 {
        if self.model.is_none() {
            return 0;
        }
        let last = self
            .voiced
            .last()
            .map_or(0, |&offset| offset + WINDOW as i64);
        self.to_source(self.analyzed - last)
    }

    /// 16 kHz offsets of the voiced windows, and how much was run.
    pub fn voiced(&self) -> &[i64] {
        &self.voiced
    }

    pub fn analyzed(&self) -> i64 {
        self.analyzed
    }

    fn to_source(&self, samples: i64) -> i64 {
        ((samples * i64::from(self.rate)) as f64 / f64::from(RATE)).round_ties_even() as i64
    }

    /// Source audio at 16 kHz. 32 and 48 kHz average each group of samples,
    /// which also filters out most of what would alias; other rates are
    /// interpolated linearly, carrying the position between calls.
    fn resample(&mut self, samples: Vec<f32>) -> Vec<f32> {
        if self.rate == RATE {
            return samples;
        }
        let mut source = std::mem::take(&mut self.carry);
        source.extend_from_slice(&samples);
        if self.rate.is_multiple_of(RATE) {
            let factor = (self.rate / RATE) as usize;
            let usable = source.len() / factor * factor;
            let out = source[..usable]
                .chunks_exact(factor)
                .map(|group| group.iter().fold(0.0f32, |sum, &s| sum + s) / factor as f32)
                .collect();
            self.carry = source[usable..].to_vec();
            return out;
        }
        let step = f64::from(self.rate) / f64::from(RATE);
        let length = source.len() as f64;
        let count = (((length - 1.0 - self.position) / step).floor() + 1.0).max(0.0) as usize;
        let out = (0..count)
            .map(|k| interpolate(&source, self.position + step * k as f64) as f32)
            .collect();
        let travelled = self.position + step * count as f64;
        let consumed = (travelled.floor() as usize).min(source.len());
        self.position = travelled - consumed as f64;
        self.carry = source[consumed..].to_vec();
        out
    }
}

/// A source sample offset in 16 kHz samples, as Python's `start * RATE / rate`.
fn to_16k(samples: i64, rate: u32) -> f64 {
    (samples * i64::from(RATE)) as f64 / f64::from(rate)
}

/// `np.interp(x, arange(len(fp)), fp)`, computed in f64 like NumPy.
fn interpolate(fp: &[f32], x: f64) -> f64 {
    let last = fp.len() - 1;
    if x >= last as f64 {
        return f64::from(fp[last]);
    }
    let j = x.floor() as usize;
    let slope = f64::from(fp[j + 1]) - f64::from(fp[j]);
    slope * (x - j as f64) + f64::from(fp[j])
}
