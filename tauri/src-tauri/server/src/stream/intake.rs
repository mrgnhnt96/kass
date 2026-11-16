//! A dictation's audio as it arrives: the start message, PCM frames, the
//! recording, pause cuts and the phrase windows sent to recognition.
//! Python: the audio half of `StreamingCapture` in
//! `backend/services/capture_stream.py` (`__init__`, `append`,
//! `ignore_cue` and the window choice in `_run`).
//!
//! Recognition windows end on pauses where possible. The full audio is
//! written to the recording once; only a bounded backlog is kept in memory.
//! A phrase ends at 0.7 s without a voice, at least 2 s after the last cut.
//! Speech that runs 20 s without one is cut there, keeping a second of
//! overlap, and the take is then recognized whole at finish.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::speech::{SpeechDetector, VoiceModel};

pub const MAX_FRAME_BYTES: usize = 65_536;
pub const MAX_SECONDS: i64 = 3600;
/// Longest phrase window, in seconds.
const WINDOW_SECONDS: i64 = 20;

pub const FELL_BEHIND: &str = "Recognition fell behind; final output uses full-audio recognition.";
pub const CONTINUOUS: &str =
    "Continuous speech exceeded the safe phrase window; final output uses full-audio recognition.";

/// Where a dictation's text is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Dictation,
    Recording,
    Command,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Dictation => "dictation",
            Source::Recording => "recording",
            Source::Command => "command",
        }
    }
}

/// The start message, checked. `language` and `stt_model` fall back to the
/// user's capture settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    pub sample_rate: u32,
    /// The client shows provisional cleaned text.
    pub provisional: bool,
    /// The first milliseconds may hold the client's own start cue.
    pub start_cue_ms: u32,
    pub source: Source,
    /// None: detect it.
    pub language: Option<String>,
    pub stt_model: String,
}

const STT_MODELS: [&str; 5] = ["base", "small", "medium", "large", "turbo"];

impl Start {
    /// Read a start message, with the messages Python's `StreamingCapture`
    /// gives for one it can't use.
    pub fn parse(start: &Value, language: Option<&str>, stt_model: &str) -> Result<Self, String> {
        let field = |name: &str| start.get(name);
        if field("type").and_then(Value::as_str) != Some("start")
            || !is_int(field("protocol_version"), 1)
        {
            return Err("Expected protocol version 1 start message".into());
        }
        let sample_rate = field("sample_rate")
            .and_then(int)
            .filter(|rate| (16_000..=48_000).contains(rate))
            .ok_or("sample_rate must be between 16000 and 48000")? as u32;
        if !is_int(field("channels"), 1)
            || field("encoding").and_then(Value::as_str) != Some("pcm_s16le")
        {
            return Err("Audio must be mono pcm_s16le".into());
        }
        let provisional = field("provisional") == Some(&Value::Bool(true));
        let start_cue_ms = match field("start_cue_ms") {
            None => 0,
            Some(value) => int(value)
                .filter(|ms| (0..=1000).contains(ms))
                .ok_or("start_cue_ms must be between 0 and 1000")?
                as u32,
        };
        let source = match field("source") {
            None => Source::Dictation,
            Some(Value::String(name)) if name == "dictation" => Source::Dictation,
            Some(Value::String(name)) if name == "recording" => Source::Recording,
            Some(Value::String(name)) if name == "command" => Source::Command,
            Some(_) => return Err("Invalid streaming capture source".into()),
        };
        let language = match field("language") {
            None => language.map(str::to_string),
            Some(Value::Null) => None,
            Some(Value::String(code)) if is_language(code) => Some(code.clone()),
            Some(_) => return Err("Invalid language".into()),
        };
        if language.as_deref().is_some_and(|code| !is_language(code)) {
            return Err("Invalid language".into());
        }
        let language = language.filter(|code| code != "auto");
        let stt_model = match field("stt_model") {
            Some(Value::String(name)) if !name.is_empty() => name.clone(),
            Some(value) if truthy(value) => return Err("Invalid STT model".into()),
            _ => stt_model.to_string(),
        };
        if !STT_MODELS.contains(&stt_model.as_str()) {
            return Err("Invalid STT model".into());
        }
        Ok(Self {
            sample_rate,
            provisional,
            start_cue_ms,
            source,
            language,
            stt_model,
        })
    }
}

/// A JSON integer, as Python's `isinstance(value, int)` reads one (never a
/// float like `16000.0`).
fn int(value: &Value) -> Option<i64> {
    value.as_i64().filter(|_| value.is_i64() || value.is_u64())
}

/// `value == expected` in Python, where `True == 1`.
fn is_int(value: Option<&Value>, expected: i64) -> bool {
    match value {
        Some(Value::Bool(flag)) => i64::from(*flag) == expected,
        Some(Value::Number(number)) => number.as_f64() == Some(expected as f64),
        _ => false,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

fn is_language(code: &str) -> bool {
    (2..=32).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
}

/// The next piece of work the audio calls for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Recognize this phrase, then `advance` past it.
    Phrase(Window),
    /// Recognize the whole recording instead: phrases can't be joined safely.
    FullAudio,
    /// Released and every phrase recognized.
    Done,
    /// Wait for more audio.
    Wait,
}

/// A phrase window: `size` samples from source sample `start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: i64,
    pub size: i64,
    /// Cut at a pause rather than at the 20 s limit or the release.
    pub paused: bool,
    /// Cut at the 20 s limit: the next window starts a second earlier.
    pub forced: bool,
    /// The rest of the audio after release.
    pub last: bool,
}

/// The audio of one dictation.
pub struct Intake {
    rate: u32,
    speech: SpeechDetector,
    archive: Option<hound::WavWriter<BufWriter<File>>>,
    path: PathBuf,
    /// Audio not yet recognized, from `offset`.
    pending: Vec<i16>,
    /// Pause cuts not yet reached, as sample offsets.
    cuts: Vec<i64>,
    last_cut: i64,
    /// Where `pending` starts.
    offset: i64,
    /// Samples received.
    samples: i64,
    sequence: u32,
    /// Samples recognition has covered.
    covered: i64,
    backlogged: bool,
    peak_backlog: f64,
    degraded_reason: Option<&'static str>,
}

impl Intake {
    /// Start a dictation's audio, recording it to `path` as it arrives.
    pub fn new(start: &Start, path: &Path, model: Option<Arc<VoiceModel>>) -> Result<Self, String> {
        let rate = start.sample_rate;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let archive = hound::WavWriter::create(path, spec)
            .map_err(|e| format!("Could not record to {}: {e}", path.display()))?;
        let ignore_before = i64::from(rate) * i64::from(start.start_cue_ms) / 1000;
        Ok(Self {
            rate,
            speech: SpeechDetector::new(rate, ignore_before, model),
            archive: Some(archive),
            path: path.to_path_buf(),
            pending: Vec::new(),
            cuts: Vec::new(),
            last_cut: 0,
            offset: 0,
            samples: 0,
            sequence: 0,
            covered: 0,
            backlogged: false,
            peak_backlog: 0.0,
            degraded_reason: None,
        })
    }

    /// Add one binary frame: a little-endian u32 sequence number and sample
    /// offset, then 16-bit PCM.
    pub fn append(&mut self, frame: &[u8]) -> Result<(), String> {
        if frame.len() < 10 || frame.len() > MAX_FRAME_BYTES || !(frame.len() - 8).is_multiple_of(2)
        {
            return Err("Invalid PCM frame size".into());
        }
        let sequence = u32::from_le_bytes(frame[0..4].try_into().unwrap());
        let offset = u32::from_le_bytes(frame[4..8].try_into().unwrap());
        if sequence != self.sequence || i64::from(offset) != self.samples {
            return Err("Audio frames must have contiguous sequence and sample offsets".into());
        }
        let pcm: Vec<i16> = frame[8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| i16::from_le_bytes(pair))
            .collect();
        let count = pcm.len() as i64;
        if self.samples + count > i64::from(self.rate) * MAX_SECONDS {
            return Err("Streaming session exceeds one hour".into());
        }
        if let Some(archive) = self.archive.as_mut() {
            let mut writer = archive.get_i16_writer(pcm.len() as u32);
            for &sample in &pcm {
                writer.write_sample(sample);
            }
            writer
                .flush()
                .map_err(|e| format!("Could not record the dictation: {e}"))?;
        }
        self.speech.feed(&pcm)?;
        self.samples += count;
        self.sequence += 1;
        if self.backlogged {
            return Ok(());
        }
        let rate = i64::from(self.rate);
        if (self.pending.len() + pcm.len()) as i64 > rate * 60 {
            // Recognition fell a minute behind. Keep recording and recognize
            // the whole take at finish rather than failing the dictation.
            self.backlogged = true;
            self.degraded_reason = Some(FELL_BEHIND);
            self.pending = Vec::new();
            self.cuts.clear();
            return Ok(());
        }
        self.pending.extend_from_slice(&pcm);
        self.peak_backlog = self
            .peak_backlog
            .max(self.pending.len() as f64 / f64::from(self.rate));
        // A phrase ends at 0.7 s without a voice. Loudness can't tell: a fan's
        // hum on one microphone is as loud as speech on another.
        if self.samples - self.last_cut >= rate * 2
            && self.speech.quiet() as f64 >= f64::from(self.rate) * 0.7
        {
            self.cuts.push(self.samples);
            self.last_cut = self.samples;
        }
        Ok(())
    }

    /// A sound Kass played during the take (the style cue), between two
    /// sample offsets: the microphone may have picked it up, and a chime
    /// reads as a voice. Only voice detection skips it; recognition hears
    /// the audio as it was.
    pub fn ignore_cue(&mut self, start: &Value, end: &Value) -> Result<(), String> {
        let (Some(start), Some(end)) = (int(start), int(end)) else {
            return Err("Cue offsets must be sample counts".into());
        };
        if !(0 <= start && start <= end) || end - start > i64::from(self.rate) {
            return Err("A cue lasts at most a second".into());
        }
        self.speech.ignore(start, end);
        Ok(())
    }

    /// What to do next, `finished` once the user released. Python's `_run`
    /// loop, without the style peek, which the session decides first.
    pub fn next(&mut self, finished: bool) -> Next {
        if self.backlogged {
            return if finished {
                Next::FullAudio
            } else {
                Next::Wait
            };
        }
        let offset = self.offset;
        self.cuts.retain(|&cut| cut > offset);
        let rate = i64::from(self.rate);
        let limit = rate * WINDOW_SECONDS;
        let available = self.pending.len() as i64;
        let cut = self.cuts.first().map(|&cut| cut - offset);
        let forced = available >= limit && cut.is_none_or(|cut| cut > limit);
        if finished && (self.degraded_reason.is_some() || forced) {
            self.degraded_reason = Some(CONTINUOUS);
            return Next::FullAudio;
        }
        let mut size = if forced { Some(limit) } else { cut };
        let paused = !forced && cut.is_some();
        let mut last = false;
        if size.is_none() && finished {
            size = Some(available);
            last = true;
        }
        match size {
            Some(size) if size > 0 => Next::Phrase(Window {
                start: offset,
                size,
                paused,
                forced,
                last,
            }),
            _ if finished => Next::Done,
            _ => Next::Wait,
        }
    }

    /// The window's audio, to send to recognition.
    pub fn pcm(&self, window: &Window) -> &[i16] {
        &self.pending[..window.size as usize]
    }

    /// Move past a recognized window. A window cut at the 20 s limit keeps
    /// its last second for the next one, and the take is recognized whole
    /// at finish.
    pub fn advance(&mut self, window: &Window) {
        if window.forced {
            self.degraded_reason = Some(CONTINUOUS);
        }
        let advance = if window.forced {
            window.size - i64::from(self.rate)
        } else {
            window.size
        };
        self.pending.drain(..advance as usize);
        self.covered = self.covered.max(self.offset + window.size);
        self.offset += advance;
    }

    /// Recognition covers the whole take (the full-audio fallback).
    pub fn cover_all(&mut self) {
        self.covered = self.samples;
    }

    /// Finish the recording file, so it can be read whole or saved.
    pub fn close_recording(&mut self) -> Result<(), String> {
        match self.archive.take() {
            Some(archive) => archive
                .finalize()
                .map_err(|e| format!("Could not finish the recording: {e}")),
            None => Ok(()),
        }
    }

    pub fn speech(&self) -> &SpeechDetector {
        &self.speech
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn samples(&self) -> i64 {
        self.samples
    }

    pub fn offset(&self) -> i64 {
        self.offset
    }

    pub fn covered(&self) -> i64 {
        self.covered
    }

    pub fn last_cut(&self) -> i64 {
        self.last_cut
    }

    /// Whether a pause cut is waiting to be recognized.
    pub fn has_cut(&self) -> bool {
        self.cuts.iter().any(|&cut| cut > self.offset)
    }

    /// Audio not yet recognized.
    pub fn pending(&self) -> &[i16] {
        &self.pending
    }

    pub fn peak_backlog(&self) -> f64 {
        self.peak_backlog
    }

    pub fn degraded_reason(&self) -> Option<&'static str> {
        self.degraded_reason
    }
}

impl Drop for Intake {
    fn drop(&mut self) {
        let _ = self.close_recording();
    }
}
