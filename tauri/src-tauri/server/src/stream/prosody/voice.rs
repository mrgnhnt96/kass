//! A recording's voice measured against its words: per sentence how high,
//! how varied and how loud it was, the words drawn out, and the "!" and
//! drawn-out spellings written into the text. Python: `semitones`,
//! `Baseline`, `Voice`, `measure_voice`, `written_sentences`, `exclaim` and
//! `stretch` in `backend/services/prosody.py`.
//!
//! Everything is relative to the speaker. A sentence's energy is how far
//! above their own statements its pitch level, pitch range and loudness are,
//! in spreads of each (`Baseline`); drawn-out words are measured against the
//! recording's own pace.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use serde::{Deserialize, Serialize};

use kass_text::difflib::SequenceMatcher;

use super::math::{median, percentile, percentiles, round2, round_to};
use super::pitch::{Frames, HOP};
use super::text::{
    core, ends_heard_sentence, ends_written_sentence, final_period, is_alpha, is_function_word,
    is_upper, key, letters, stretch_word, syllables, tokens,
};
use super::Word;

/// Voiced runs shorter than this are noise, not syllables.
const MIN_RUN_FRAMES: usize = 3;
/// Fewer voiced frames than this (0.1 s) say too little.
const MIN_VOICED: usize = 10;
/// Fewer statements than this say too little about the speaker.
const MIN_STATEMENTS: usize = 20;
/// Pitch steps under this (semitones per frame) keep a vowel steady.
const STEADY_STEP: f64 = 0.35;
/// The question hint looks at the last stretch of each sentence's voice.
const FINAL_S: f64 = 0.3;
/// At least this share of a written sentence's words must be found among
/// the words heard, or it isn't measured (cleanup rewrote it).
const MIN_FOUND: f64 = 0.5;

/// A word is drawn out (all must hold): this many times the recording's
/// time per syllable, this much louder than its sentence, a steady vowel
/// this long, and neither first nor last in its sentence nor a closed-class
/// word. The default pace is past a thinking drawl ("naaame" said while
/// finding the next words), which sounds just like emphasis.
pub const STRETCH_PACE: f64 = 7.0;
pub const STRETCH_LOUDER_DB: f64 = 1.5;
pub const STRETCH_PLATEAU_S: (f64, f64) = (0.3, 0.6);

/// A sentence said with at least this much energy (spreads above the
/// speaker's statements) ends with "!". Strict on purpose: it adapts per
/// user from their edits, never by lowering this.
pub const EXCLAIM_ENERGY: f64 = 2.9;

/// When a word counts as drawn out (all must hold). A closed-class word
/// never does, but for `words`: the ones this speaker has written drawn out
/// themselves ("sooo").
#[derive(Clone, Debug, PartialEq)]
pub struct StretchRule {
    pub pace: f64,
    pub louder_db: f64,
    pub plateau: (f64, f64),
    pub words: BTreeSet<String>,
}

impl Default for StretchRule {
    fn default() -> Self {
        Self {
            pace: STRETCH_PACE,
            louder_db: STRETCH_LOUDER_DB,
            plateau: STRETCH_PLATEAU_S,
            words: BTreeSet::new(),
        }
    }
}

impl StretchRule {
    pub fn holds(&self, word: &str, stretch: f64, louder: f64, plateau: f64) -> bool {
        let key = key(word);
        (!is_function_word(&key) || self.words.contains(&key))
            && stretch >= self.pace
            && louder >= self.louder_db
            && self.plateau.0 <= plateau
            && plateau <= self.plateau.1
    }
}

/// Pitch in semitones above 100 Hz, octave jumps folded back and lightly
/// smoothed.
///
/// A tracker sometimes locks onto twice or half the true period for a few
/// frames. Within one voiced stretch pitch doesn't move 7 semitones from its
/// median, so a frame that far moves by octaves toward it. Whole stretches
/// are never moved: stretches far above the speaker's norm were real, the
/// very excitement this measures. Voiced stretches under 30 ms are dropped.
pub fn semitones(hz: &[f64]) -> Vec<f64> {
    let mut st: Vec<f64> = hz.iter().map(|hz| 12.0 * (hz / 100.0).log2()).collect();
    let mut smoothed = vec![f64::NAN; st.len()];
    let mut start = 0;
    while start < st.len() {
        if st[start].is_nan() {
            start += 1;
            continue;
        }
        let end = start + st[start..].iter().take_while(|v| !v.is_nan()).count();
        if end - start >= MIN_RUN_FRAMES {
            let run = &mut st[start..end];
            let middle = median(run);
            for value in run.iter_mut() {
                let away = *value - middle;
                if away.abs() > 7.0 {
                    *value -= 12.0 * (away / 12.0).round_ties_even();
                }
            }
            // A 5-frame median within the stretch, its ends repeated.
            let last = run.len() - 1;
            for (i, out) in smoothed[start..end].iter_mut().enumerate() {
                let window: Vec<f64> = (i..i + 5)
                    .map(|j| run[j.saturating_sub(2).min(last)])
                    .collect();
                *out = median(&window);
            }
        }
        start = end;
    }
    smoothed
}

/// The speaker's statements: median and spread of pitch level, range and
/// loudness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Baseline {
    pub level: (f64, f64),
    pub span: (f64, f64),
    pub loudness: (f64, f64),
}

impl Baseline {
    /// From saved sentences; only statements (ending with a period) count.
    pub fn of(sentences: &[Statement]) -> Option<Self> {
        let statements: Vec<[f64; 3]> = sentences
            .iter()
            .filter(|sentence| sentence.text.trim_end().ends_with('.'))
            .filter_map(|sentence| Some([sentence.level?, sentence.span?, sentence.loudness?]))
            .collect();
        if statements.len() < MIN_STATEMENTS {
            return None;
        }
        let spread = |measure: usize, floor: f64| {
            let values: Vec<f64> = statements.iter().map(|stats| stats[measure]).collect();
            let middle = median(&values);
            let deviations: Vec<f64> = values.iter().map(|value| (value - middle).abs()).collect();
            (middle, floor.max(1.4826 * median(&deviations)))
        };
        Some(Self {
            level: spread(0, 0.5),
            span: spread(1, 0.5),
            loudness: spread(2, 1.0),
        })
    }

    /// Mean of how many spreads above the speaker's statements each measure
    /// is.
    pub fn energy(&self, level: f64, span: f64, loudness: f64) -> f64 {
        ((level - self.level.0) / self.level.1
            + (span - self.span.0) / self.span.1
            + (loudness - self.loudness.0) / self.loudness.1)
            / 3.0
    }
}

/// A saved sentence as the baseline reads it: its text, and its pitch
/// level, range and loudness when it had enough voice to measure.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Statement {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub level: Option<f64>,
    #[serde(default)]
    pub span: Option<f64>,
    #[serde(default)]
    pub loudness: Option<f64>,
}

impl From<&Sentence> for Statement {
    fn from(sentence: &Sentence) -> Self {
        let stats = sentence.stats.as_ref();
        Self {
            text: sentence.text.clone(),
            level: stats.map(|stats| stats.level),
            span: stats.map(|stats| stats.span),
            loudness: stats.map(|stats| stats.loudness),
        }
    }
}

/// One sentence Whisper heard, as saved with its capture. `stats` is
/// missing with too little voice to measure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sentence {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    #[serde(flatten)]
    pub stats: Option<Stats>,
}

/// How high (semitones), how varied and how loud (dB) the voice was, the
/// pitch's slope and rise over its last 0.3 s, and its energy against the
/// speaker's baseline (null without one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub level: f64,
    pub span: f64,
    pub loudness: f64,
    pub slope: f64,
    pub rise: f64,
    #[serde(default)]
    pub energy: Option<f64>,
}

/// How drawn out one word was: its time per syllable against the
/// recording's pace, how much louder than its sentence, and its longest
/// steady pitch in seconds. `at` is its place among all the words heard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WordShape {
    pub word: String,
    pub sentence: usize,
    pub index: usize,
    pub at: usize,
    pub stretch: f64,
    pub louder: f64,
    pub plateau: f64,
}

/// A word drawn out, and how it is written so.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stretched {
    #[serde(flatten)]
    pub shape: WordShape,
    pub written: String,
}

/// A written sentence's energy, saved to learn from the user's edits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WrittenEnergy {
    pub text: String,
    pub energy: f64,
}

/// The measurements saved with a capture (`captures.prosody`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    pub version: u32,
    pub pace: Option<f64>,
    pub voiced: f64,
    pub sentences: Vec<Sentence>,
    pub stretched: Vec<Stretched>,
    /// Every content word as `[word, stretch, louder, plateau]`: what a
    /// speaker's own stretches are learned from.
    pub shapes: Vec<(String, f64, f64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<Vec<WrittenEnergy>>,
}

/// A sentence of the written text that was said and measured.
#[derive(Clone, Debug, PartialEq)]
pub struct WrittenSentence {
    pub text: String,
    /// Where it starts and ends in the text, in bytes.
    pub start: usize,
    pub end: usize,
    pub energy: f64,
    /// The speaker said its period: what they said wins.
    pub said_period: bool,
}

/// A recording's words and its voice, ready to measure any stretch of it.
#[derive(Clone, Debug)]
pub struct Voice {
    pub words: Vec<Word>,
    st: Vec<f64>,
    db: Vec<f64>,
}

/// `np.median` of float32 loudness: numpy keeps float32 there, so the mean
/// of the middle two is rounded to float32 as well.
fn loudness_median(loud: &[f64]) -> f64 {
    f64::from(median(loud) as f32)
}

impl Voice {
    pub fn new(frames: &Frames, words: Vec<Word>) -> Self {
        Self {
            words,
            st: semitones(&frames.hz),
            db: frames.db.clone(),
        }
    }

    /// Pitch per frame in semitones (NaN where unvoiced).
    pub fn semitones(&self) -> &[f64] {
        &self.st
    }

    fn part(&self, start: f64, end: f64) -> Range<usize> {
        let to = ((end / HOP).ceil().max(0.0) as usize).min(self.st.len());
        let from = ((start / HOP) as i64).max(0) as usize;
        from.min(to)..to
    }

    /// Pitch over `start`–`end` (seconds), its voiced frames' loudness and
    /// their indices.
    fn frames_in(&self, start: f64, end: f64) -> (&[f64], Vec<f64>, Vec<usize>) {
        let part = self.part(start, end);
        let values = &self.st[part.clone()];
        let voiced = || part.clone().filter(|&i| !self.st[i].is_nan());
        (
            values,
            voiced().map(|i| self.db[i]).collect(),
            voiced().collect(),
        )
    }

    /// `Baseline::energy` of the voice from `start` to `end`: `stats`'
    /// level, span and loudness only, in one pass over the frames.
    pub fn energy(&self, start: f64, end: f64, baseline: &Baseline) -> Option<f64> {
        let (values, loud, _) = self.frames_in(start, end);
        let pitched: Vec<f64> = values.iter().copied().filter(|v| !v.is_nan()).collect();
        if pitched.len() < MIN_VOICED {
            return None;
        }
        let [low, level, high] = percentiles(&pitched, [10.0, 50.0, 90.0]);
        Some(baseline.energy(level, high - low, loudness_median(&loud)))
    }

    /// How high, how varied and how loud the voice was from `start` to
    /// `end`, unrounded and without energy. None with too little voice.
    fn stats(&self, start: f64, end: f64) -> Option<Stats> {
        let (values, loud, indices) = self.frames_in(start, end);
        let pitched: Vec<f64> = values.iter().copied().filter(|v| !v.is_nan()).collect();
        if pitched.len() < MIN_VOICED {
            return None;
        }
        let level = median(&pitched);
        let since = *indices.last()? as f64 * HOP - FINAL_S;
        let last: Vec<usize> = indices
            .into_iter()
            .filter(|&i| i as f64 * HOP >= since)
            .collect();
        let times: Vec<f64> = last.iter().map(|&i| i as f64 * HOP).collect();
        let finals: Vec<f64> = last.iter().map(|&i| self.st[i]).collect();
        Some(Stats {
            level,
            span: percentile(&pitched, 90.0) - percentile(&pitched, 10.0),
            loudness: loudness_median(&loud),
            slope: theil_sen(&times, &finals),
            rise: median(&finals) - level,
            energy: None,
        })
    }
}

fn theil_sen(times: &[f64], values: &[f64]) -> f64 {
    let mut slopes = Vec::new();
    for i in 0..times.len() {
        for j in i + 1..times.len() {
            let dt = times[j] - times[i];
            if dt > 0.0 {
                slopes.push((values[j] - values[i]) / dt);
            }
        }
    }
    if slopes.is_empty() {
        0.0
    } else {
        median(&slopes)
    }
}

/// Frames in the longest run of voiced pitch with steps under
/// `STEADY_STEP`.
fn longest_steady(st: &[f64]) -> usize {
    let (mut best, mut run) = (0, 0);
    for pair in st.windows(2) {
        let (previous, current) = (pair[0], pair[1]);
        if !previous.is_nan() && !current.is_nan() && (current - previous).abs() < STEADY_STEP {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    if best > 0 {
        best + 1
    } else {
        0
    }
}

/// Words grouped into sentences, each ending at a word ending in . ! or ?.
pub fn sentences(words: &[Word]) -> Vec<&[Word]> {
    let mut grouped = Vec::new();
    let mut first = 0;
    for (index, word) in words.iter().enumerate() {
        if ends_heard_sentence(&word.text) {
            grouped.push(&words[first..=index]);
            first = index + 1;
        }
    }
    if first < words.len() {
        grouped.push(&words[first..]);
    }
    grouped
}

/// Per-sentence pitch, loudness and energy, and drawn-out words (by
/// `rule`). `shapes` are how drawn out each content word was, against the
/// recording's pace: what a speaker's own stretches are learned from.
pub fn measure_voice(voice: &Voice, baseline: Option<&Baseline>, rule: &StretchRule) -> Measured {
    let pace = pace(voice).filter(|pace| *pace != 0.0);
    let (mut measured, mut shapes, mut offset) = (Vec::new(), Vec::new(), 0);
    for (number, sentence) in sentences(&voice.words).into_iter().enumerate() {
        let (start, end) = (sentence[0].start, sentence[sentence.len() - 1].end);
        let stats = voice.stats(start, end).map(|stats| Stats {
            level: round2(stats.level),
            span: round2(stats.span),
            loudness: round2(stats.loudness),
            slope: round2(stats.slope),
            rise: round2(stats.rise),
            energy: baseline.map(|b| round2(b.energy(stats.level, stats.span, stats.loudness))),
        });
        if let (Some(pace), Some(stats)) = (pace, &stats) {
            shapes.extend(word_shapes(
                sentence,
                number,
                offset,
                stats.loudness,
                pace,
                voice,
            ));
        }
        measured.push(Sentence {
            text: sentence
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            start: round2(start),
            end: round2(end),
            stats,
        });
        offset += sentence.len();
    }
    let voiced = voice.st.iter().filter(|value| !value.is_nan()).count();
    Measured {
        version: 1,
        pace: pace.map(|pace| round_to(pace, 3)),
        voiced: round2(voiced as f64 * HOP),
        sentences: measured,
        stretched: shapes
            .iter()
            .filter(|s| rule.holds(&s.word, s.stretch, s.louder, s.plateau))
            .map(|shape| Stretched {
                written: stretch_word(&shape.word, letters(shape.stretch)),
                shape: shape.clone(),
            })
            .collect(),
        shapes: shapes
            .into_iter()
            .map(|s| (s.word, s.stretch, s.louder, s.plateau))
            .collect(),
        written: None,
    }
}

/// `measure_voice` with the default rule, and the energy of each sentence
/// of `text` (the text the dictation returned): what is saved with a
/// capture, to learn from the user's edits.
pub fn measure_written(voice: &Voice, baseline: Option<&Baseline>, text: &str) -> Measured {
    let mut measured = measure_voice(voice, baseline, &StretchRule::default());
    measured.written = Some(
        written_sentences(text, voice, baseline)
            .into_iter()
            .map(|sentence| WrittenEnergy {
                text: sentence.text,
                energy: round2(sentence.energy),
            })
            .collect(),
    );
    measured
}

/// The recording's time per syllable, from its voiced frames.
fn pace(voice: &Voice) -> Option<f64> {
    let per_syllable: Vec<f64> = voice
        .words
        .iter()
        .filter_map(|word| {
            let seconds = voice.frames_in(word.start, word.end).2.len() as f64 * HOP;
            (seconds != 0.0).then(|| seconds / syllables(&word.text) as f64)
        })
        .collect();
    (!per_syllable.is_empty()).then(|| median(&per_syllable))
}

/// How drawn out each word of a sentence that could be was: not the first
/// or last word, a number or an initialism ("UI", said letter by letter).
/// Closed-class words are measured too, though only a speaker's own rule
/// may draw one out: "the...", "a..." are mostly hesitations.
fn word_shapes(
    sentence: &[Word],
    number: usize,
    offset: usize,
    sentence_loudness: f64,
    pace: f64,
    voice: &Voice,
) -> Vec<WordShape> {
    let inner = sentence.len().saturating_sub(1);
    let mut found = Vec::new();
    for (index, word) in sentence.iter().enumerate().take(inner).skip(1) {
        let core = core(&word.text);
        if !is_alpha(&core) || (core.chars().count() > 1 && is_upper(&core)) {
            continue;
        }
        let (values, loud, indices) = voice.frames_in(word.start, word.end);
        let louder = if loud.is_empty() {
            0.0
        } else {
            loudness_median(&loud) - sentence_loudness
        };
        found.push(WordShape {
            word: word.text.clone(),
            sentence: number,
            index,
            at: offset + index,
            stretch: round2(indices.len() as f64 * HOP / syllables(&word.text) as f64 / pace),
            louder: round2(louder),
            plateau: round2(longest_steady(values) as f64 * HOP),
        });
    }
    found
}

/// Whether the speaker said the period after `words[index]`.
fn said_period(words: &[Word], index: usize) -> bool {
    let after: Vec<String> = words
        .iter()
        .skip(index + 1)
        .take(2)
        .map(|word| key(&word.text))
        .collect();
    after.first().is_some_and(|word| word == "period") || after == ["full", "stop"]
}

/// `text`'s words (byte ranges), and for each one found among the words
/// heard, which it is.
fn heard(text: &str, voice: &Voice) -> (Vec<(usize, usize)>, HashMap<usize, usize>) {
    let tokens = tokens(text);
    let said: Vec<String> = voice.words.iter().map(|word| key(&word.text)).collect();
    let written: Vec<String> = tokens.iter().map(|&(s, e)| key(&text[s..e])).collect();
    let mut heard_at = HashMap::new();
    for block in SequenceMatcher::new(&said, &written, false).get_matching_blocks() {
        for offset in 0..block.size {
            heard_at.insert(block.b + offset, block.a + offset);
        }
    }
    (tokens, heard_at)
}

/// The sentences of `text` that were said and measured, with their energy.
///
/// Sentences are the written text's (the cleanup's), each found in the
/// recording by its words, so a sentence Whisper ran into the next one is
/// still measured on its own. One too rewritten to find, or with too little
/// voice, is left out.
pub fn written_sentences(
    text: &str,
    voice: &Voice,
    baseline: Option<&Baseline>,
) -> Vec<WrittenSentence> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    if voice.words.is_empty() {
        return Vec::new();
    }
    let (tokens, heard_at) = heard(text, voice);
    let (mut found, mut first) = (Vec::new(), 0);
    for (index, &(_, end)) in tokens.iter().enumerate() {
        if !ends_written_sentence(&text[tokens[index].0..end]) {
            continue;
        }
        let sentence = first..index + 1;
        first = index + 1;
        let heard: Vec<usize> = sentence
            .clone()
            .filter(|i| {
                let (s, e) = tokens[*i];
                !key(&text[s..e]).is_empty()
            })
            .filter_map(|i| heard_at.get(&i).copied())
            .collect();
        if heard.is_empty() || (heard.len() as f64) < MIN_FOUND * sentence.len() as f64 {
            continue;
        }
        let (low, high) = (heard.iter().min(), heard.iter().max());
        let (Some(&low), Some(&high)) = (low, high) else {
            continue;
        };
        let Some(energy) = voice.energy(voice.words[low].start, voice.words[high].end, baseline)
        else {
            continue;
        };
        let start = tokens[sentence.start].0;
        found.push(WrittenSentence {
            text: text[start..end].to_string(),
            start,
            end,
            energy,
            said_period: said_period(&voice.words, high),
        });
    }
    found
}

/// `text` with "!" ending each sentence said with clearly more energy
/// (`cutoff`) than the speaker's statements. Only a period changes, and not
/// one the speaker said: a question, a sentence left without an ending and
/// one too rewritten to find keep theirs.
pub fn exclaim(text: &str, voice: &Voice, baseline: Option<&Baseline>, cutoff: f64) -> String {
    let mut changed = text.to_string();
    for sentence in written_sentences(text, voice, baseline) {
        if let Some(period) = final_period(&sentence.text) {
            if sentence.energy >= cutoff && !sentence.said_period {
                let at = sentence.start + period;
                changed.replace_range(at..at + 1, "!");
            }
        }
    }
    changed
}

/// `text` with each drawn-out word (`measure_voice`'s `stretched`) written
/// drawn out, only where the written word is the word heard: one the
/// cleanup changed stays as written.
pub fn stretch(text: &str, voice: &Voice, stretched: &[Stretched]) -> String {
    if stretched.is_empty() {
        return text.to_string();
    }
    let (tokens, heard_at) = heard(text, voice);
    let written_at: HashMap<usize, usize> = heard_at.into_iter().map(|(w, h)| (h, w)).collect();
    let mut order: Vec<&Stretched> = stretched.iter().collect();
    order.sort_by_key(|stretched| std::cmp::Reverse(stretched.shape.at));
    let mut changed = text.to_string();
    // From the end, so earlier positions stay put.
    for drawn in order {
        let Some(&index) = written_at.get(&drawn.shape.at) else {
            continue;
        };
        let (s, e) = tokens[index];
        changed.replace_range(
            s..e,
            &stretch_word(&text[s..e], letters(drawn.shape.stretch)),
        );
    }
    changed
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

    /// A voice at `hz` for `seconds`, louder (`db`) where `loud` covers.
    fn steady(seconds: f64, hz: f64) -> Frames {
        let count = (seconds / HOP) as usize;
        Frames {
            hz: vec![hz; count],
            db: vec![-20.0; count],
        }
    }

    #[test]
    fn semitones_fold_octave_jumps_and_drop_short_runs() {
        let mut hz = vec![200.0; 20];
        hz[10] = 400.0;
        hz[11] = 400.0;
        hz.push(f64::NAN);
        hz.extend([150.0, 150.0]);
        let st = semitones(&hz);
        let expected = 12.0 * 2f64.log2();
        assert!(st[..20].iter().all(|v| (v - expected).abs() < 1e-9));
        assert!(st[20..].iter().all(|v| v.is_nan()));
    }

    #[test]
    fn baseline_needs_twenty_statements() {
        let sentence = |text: &str, level: f64| Statement {
            text: text.to_string(),
            level: Some(level),
            span: Some(4.0),
            loudness: Some(-20.0),
        };
        let mut saved: Vec<Statement> = (0..19)
            .map(|i| sentence("Fine.", 10.0 + i as f64 * 0.1))
            .collect();
        saved.push(sentence("Really?", 30.0));
        assert_eq!(Baseline::of(&saved), None);
        saved.push(sentence("Also fine. ", 11.0));
        let baseline = Baseline::of(&saved).unwrap();
        assert!((baseline.level.0 - 10.95).abs() < 1e-9);
        assert_eq!(baseline.span, (4.0, 0.5));
        assert_eq!(baseline.loudness, (-20.0, 1.0));
        assert_eq!(baseline.energy(10.95, 4.5, -19.0), (0.0 + 1.0 + 1.0) / 3.0);
    }

    #[test]
    fn sentences_end_at_marks() {
        let words = [
            word("Hi", 0.0, 0.1),
            word("there.", 0.1, 0.2),
            word("Ok?\"", 0.2, 0.3),
            word("then", 0.3, 0.4),
        ];
        let grouped = sentences(&words);
        assert_eq!(
            grouped.iter().map(|s| s.len()).collect::<Vec<_>>(),
            [2, 1, 1]
        );
    }

    #[test]
    fn a_loud_high_sentence_ends_with_an_exclamation() {
        let mut frames = steady(2.0, 300.0);
        frames.db = vec![-5.0; frames.hz.len()];
        let words = vec![
            word("We", 0.0, 0.4),
            word("did", 0.4, 0.8),
            word("it.", 0.8, 1.9),
        ];
        let voice = Voice::new(&frames, words);
        let baseline = Baseline {
            level: (10.0, 1.0),
            span: (0.0, 1.0),
            loudness: (-20.0, 1.0),
        };
        assert_eq!(
            exclaim("We did it.", &voice, Some(&baseline), 2.9),
            "We did it!"
        );
        assert_eq!(exclaim("We did it.", &voice, None, 2.9), "We did it.");
        // Rewritten past finding: left as it is.
        assert_eq!(
            exclaim("Something else entirely.", &voice, Some(&baseline), 2.9),
            "Something else entirely."
        );
        // A period the speaker said stays.
        let said = Voice::new(
            &frames,
            vec![word("Done", 0.0, 1.0), word("period", 1.0, 1.9)],
        );
        assert_eq!(exclaim("Done.", &said, Some(&baseline), 2.9), "Done.");
    }

    #[test]
    fn stretch_writes_only_the_word_heard() {
        let frames = steady(2.0, 200.0);
        let voice = Voice::new(
            &frames,
            vec![
                word("It", 0.0, 0.2),
                word("was", 0.2, 0.4),
                word("way", 0.4, 1.8),
                word("better.", 1.8, 2.0),
            ],
        );
        let drawn = Stretched {
            shape: WordShape {
                word: "way".into(),
                sentence: 0,
                index: 2,
                at: 2,
                stretch: 4.6,
                louder: 2.0,
                plateau: 0.5,
            },
            written: "waaaay".into(),
        };
        let drawn = [drawn];
        assert_eq!(
            stretch("It was way better.", &voice, &drawn),
            "It was wayyyy better."
        );
        assert_eq!(
            stretch("It was far better.", &voice, &drawn),
            "It was far better."
        );
        assert_eq!(
            stretch("It was way better.", &voice, &[]),
            "It was way better."
        );
    }

    #[test]
    fn rule_keeps_function_words_unless_taught() {
        let rule = StretchRule::default();
        assert!(rule.holds("way", 7.0, 1.5, 0.3));
        assert!(!rule.holds("so", 9.0, 3.0, 0.4));
        assert!(!rule.holds("way", 6.9, 3.0, 0.4));
        assert!(!rule.holds("way", 8.0, 3.0, 0.7));
        let taught = StretchRule {
            words: BTreeSet::from(["so".to_string()]),
            ..StretchRule::default()
        };
        assert!(taught.holds("So,", 9.0, 3.0, 0.4));
    }

    #[test]
    fn longest_steady_counts_frames() {
        assert_eq!(longest_steady(&[1.0, 1.1, 1.2, 3.0, 3.1]), 3);
        assert_eq!(longest_steady(&[1.0, f64::NAN, 2.0]), 0);
        assert_eq!(longest_steady(&[]), 0);
    }
}
