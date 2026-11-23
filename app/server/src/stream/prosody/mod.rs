//! Expressive dictation: how a dictation was said, measured while the user
//! speaks, written into its text ("!" for a sentence said with clearly more
//! energy, "wayyy" for a word drawn out) and saved with its capture to
//! learn from the user's edits. Python: `backend/services/prosody.py` and
//! `backend/services/expression_learning.py`.
//!
//! - `pitch`: pitch and loudness frame by frame, as the audio arrives.
//! - `voice`: a recording's voice against its words; "!" and drawn-out
//!   words written into a text.
//! - `learning`: the speaker's own "!" cutoff and stretch rule, from their
//!   edits.
//! - `store`: the baseline and what was learned, read from the captures, and
//!   the measurements saved with one.
//!
//! Measuring never fails a dictation: anything that goes wrong here is
//! logged and the text is written as it is.

pub mod learning;
pub mod math;
pub mod pitch;
pub mod store;
pub mod text;
pub mod voice;

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::db::Db;

pub use learning::Learned;
pub use pitch::{Frames, PitchTrack};
pub use voice::{
    exclaim, measure_voice, measure_written, stretch, Baseline, Measured, Stretched, Voice,
};

/// A word heard, in seconds from the start of its phrase or, once placed,
/// of the recording: the worker's word alignments.
pub use super::models::Word;

/// Where a forced cut overlaps the phrase before, its words that start
/// before the last one placed ends are already placed.
const OVERLAP_S: f64 = 0.05;

/// `found` and a phrase's `words`, which began `start` seconds into the
/// recording. Where a forced cut overlaps the phrase before, the overlap's
/// words are kept once.
pub fn placed(mut found: Vec<Word>, start: f64, words: &[Word]) -> Vec<Word> {
    let reached = found.last().map_or(0.0, |word| word.end);
    found.extend(
        words
            .iter()
            .filter(|word| start + word.start >= reached - OVERLAP_S)
            .map(|word| Word {
                text: word.text.clone(),
                start: start + word.start,
                end: start + word.end,
            }),
    );
    found
}

/// One dictation's voice and words, gathered while the user speaks: the
/// voice is tracked as audio arrives, and each recognized phrase's words
/// are added with where the phrase began.
pub struct Expression {
    rate: u32,
    track: PitchTrack,
    failed: bool,
    /// (start in seconds, the phrase's words).
    phrases: Vec<(f64, Vec<Word>)>,
}

impl Expression {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            track: PitchTrack::new(rate),
            failed: false,
            phrases: Vec::new(),
        }
    }

    /// Track the voice in more of the recording's int16 samples.
    pub fn feed(&mut self, pcm: &[i16]) {
        if self.failed {
            return;
        }
        if catch_unwind(AssertUnwindSafe(|| self.track.feed(pcm))).is_err() {
            eprintln!("[server] pitch tracking failed; this dictation won't be measured");
            self.failed = true;
        }
    }

    /// A recognized phrase's words (in seconds from its start), which began
    /// at `start_sample` of the recording.
    pub fn phrase(&mut self, start_sample: u64, words: Vec<Word>) {
        if !self.failed {
            self.phrases
                .push((start_sample as f64 / f64::from(self.rate), words));
        }
    }

    /// Every phrase's words, in seconds from the recording's start.
    pub fn words(&self) -> Vec<Word> {
        self.phrases
            .iter()
            .fold(Vec::new(), |found, (start, words)| {
                placed(found, *start, words)
            })
    }

    pub fn frames(&self) -> &Frames {
        self.track.frames()
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    /// The voice so far with every phrase's words, and the words drawn out
    /// by `rule`; None when nothing can be measured.
    pub fn said(&self, rule: &voice::StretchRule) -> Option<Said> {
        if self.failed {
            return None;
        }
        let words = self.words();
        if words.is_empty() {
            return None;
        }
        let voice = Voice::new(self.frames(), words);
        let stretched = measure_voice(&voice, None, rule).stretched;
        Some(Said { voice, stretched })
    }
}

/// A dictation's voice worked out, ready to write into its text.
pub struct Said {
    pub voice: Voice,
    pub stretched: Vec<Stretched>,
}

/// `text` with "!" where it was said with clearly more energy than the
/// speaker's statements (by what they taught), and drawn-out words written
/// so ("wayyy").
pub fn express(text: &str, said: &Said, baseline: Option<&Baseline>, taught: &Learned) -> String {
    let exclaimed = exclaim(text, &said.voice, baseline, taught.cutoff);
    stretch(&exclaimed, &said.voice, &said.stretched)
}

/// The speaker's baseline and what their edits taught, read when a
/// dictation starts. The defaults when they can't be read.
pub async fn load(db: &Db) -> (Option<Baseline>, Learned) {
    let read = db
        .read(|conn| Ok((store::load_baseline(conn)?, store::load_learned(conn)?)))
        .await;
    read.unwrap_or_else(|error| {
        eprintln!(
            "[server] couldn't read how the speaker usually sounds; no \"!\" this time: {error}"
        );
        (None, Learned::default())
    })
}

/// Measure a sent dictation and save it with its capture; run after the
/// final event. `text` is the text it returned: its sentences are saved
/// with their energy, whether or not "!" is written, to learn from the
/// user's edits of them.
pub async fn measure_and_save(db: &Db, capture_id: &str, expression: &Expression, text: &str) {
    let saved = async {
        let baseline = db.read(store::load_baseline).await?;
        if expression.failed() {
            return Ok(());
        }
        let words = expression.words();
        if words.is_empty() {
            return Ok(());
        }
        let voice = Voice::new(expression.frames(), words);
        let measured = measure_written(&voice, baseline.as_ref(), text);
        let capture_id = capture_id.to_string();
        db.call(move |conn| store::save(conn, &capture_id, &measured))
            .await
    };
    if let Err(error) = saved.await {
        eprintln!("[server] couldn't measure how the dictation was said: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.to_string(),
            start,
            end,
        }
    }

    #[test]
    fn places_phrases_and_keeps_an_overlap_once() {
        let first = placed(
            Vec::new(),
            1.0,
            &[word("one", 0.0, 0.5), word("two", 0.5, 1.0)],
        );
        assert_eq!(first.last(), Some(&word("two", 1.5, 2.0)));
        // A forced cut: the next phrase starts half a second back.
        let both = placed(
            first,
            1.5,
            &[word("two", 0.0, 0.5), word("three", 0.48, 0.9)],
        );
        let texts: Vec<&str> = both.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "three"]);
        assert!((both[2].start - 1.98).abs() < 1e-9);
    }

    #[test]
    fn an_expression_without_words_says_nothing() {
        let mut expression = Expression::new(16_000);
        expression.feed(&[0; 16_000]);
        assert!(expression.said(&voice::StretchRule::default()).is_none());
        expression.phrase(8_000, vec![word("hi.", 0.0, 0.3)]);
        let said = expression.said(&voice::StretchRule::default()).unwrap();
        assert_eq!(said.voice.words, [word("hi.", 0.5, 0.8)]);
        assert_eq!(express("Hi.", &said, None, &Learned::default()), "Hi.");
    }
}
