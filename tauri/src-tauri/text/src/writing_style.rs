//! The user's punctuation habits, learned from teaching and corrections.
//!
//! Teaching keeps each dictated reply: what was said, Kass's cleanup and what
//! the user sent. Comparing the cleanup with what was sent, word by word,
//! counts what the user does at each sentence break (keep the period, turn it
//! into a comma, drop it), whether they lowercase sentence starts, drop commas
//! after opening words or before conjunctions, and end with a period.
//! Refined-output corrections add the same evidence.
//!
//! "Match my writing" uses those counts to pick the cleanup prompt's
//! punctuation section and for a deterministic pass over cleaned-up text
//! ([`apply_style`]), which only ever changes punctuation and the case of a
//! sentence's first letter, never words.
//!
//! Ported from the backend's `writing_style`. Each style's learned state
//! ([`Profile`]) is read by the caller from `writing-style.json` and passed
//! in; saving and recounting stay with whoever owns that file.

use serde_json::{json, Value};

use crate::difflib::SequenceMatcher;
use crate::py::{self, Re};
use crate::re;

/// Least evidence before a habit is applied. Calibration alone gives about
/// ten sentence breaks and a handful of each comma kind per run.
pub const MIN_EVIDENCE: u64 = 2;

const CONJUNCTIONS: &[&str] = &["and", "but", "so", "or", "yet", "though", "because"];
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "approx",
];
const OPENING_PUNCTUATION: &str = "\"'(\u{201c}\u{2018}[";

fn trailing() -> &'static Re {
    re!(r"[.,!?;:…—-]+$")
}

// --- Tokens --------------------------------------------------------------------

/// A whitespace token's word and trailing punctuation.
fn split(token: &str) -> (&str, &str) {
    match trailing().search(token) {
        Some(found) if found.get(0).unwrap().start() > 0 => {
            let at = found.get(0).unwrap().start();
            (&token[..at], &token[at..])
        }
        _ => (token, ""),
    }
}

fn key(word: &str) -> String {
    let word = py::lstrip_chars(word, OPENING_PUNCTUATION);
    let word = py::rstrip_chars(word, "\"')\u{201d}\u{2019}]");
    py::casefold(word).replace('\u{2019}', "'")
}

fn is_abbreviation(word: &str) -> bool {
    ABBREVIATIONS.contains(&key(word).as_str())
}

fn is_conjunction(word: &str) -> bool {
    CONJUNCTIONS.contains(&key(word).as_str())
}

fn is_boundary(trail: &str) -> bool {
    trail.contains('.') && !trail.contains("..") && !trail.contains('\u{2026}')
}

/// Whether `trail` ends a sentence.
fn ends_sentence(trail: &str) -> bool {
    !trail.is_empty() && (py::in_chars(py::final_char(trail), "!?") || is_boundary(trail))
}

fn capitalized(word: &str) -> bool {
    py::first(py::lstrip_chars(word, OPENING_PUNCTUATION))
        .is_some_and(|first| py::isupper(first.encode_utf8(&mut [0; 4])))
}

/// `I`, its contractions and acronyms keep their capitals anywhere.
fn keeps_capital(word: &str) -> bool {
    let letters = py::lstrip_chars(word, OPENING_PUNCTUATION);
    letters == "I"
        || letters.starts_with("I'")
        || letters.starts_with("I\u{2019}")
        || (py::len(letters) > 1 && py::isupper(letters))
}

fn lower_first(word: &str) -> String {
    if keeps_capital(word) {
        return word.to_string();
    }
    let index = word.len() - py::lstrip_chars(word, OPENING_PUNCTUATION).len();
    let rest = &word[index..];
    let first = py::head(rest, 1);
    format!(
        "{}{}{}",
        &word[..index],
        py::lower(first),
        &rest[first.len()..]
    )
}

// --- Learning ------------------------------------------------------------------

/// How often the user made each punctuation choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub boundary_period: u64,
    pub boundary_comma: u64,
    pub boundary_none: u64,
    pub lowercase_start_yes: u64,
    pub lowercase_start_no: u64,
    pub intro_comma_kept: u64,
    pub intro_comma_dropped: u64,
    pub conjunction_comma_kept: u64,
    pub conjunction_comma_dropped: u64,
    pub final_period_kept: u64,
    pub final_period_dropped: u64,
}

impl Counts {
    fn fields(&self) -> [(&'static str, &'static str, u64); 11] {
        [
            ("boundary", "period", self.boundary_period),
            ("boundary", "comma", self.boundary_comma),
            ("boundary", "none", self.boundary_none),
            ("lowercase_start", "yes", self.lowercase_start_yes),
            ("lowercase_start", "no", self.lowercase_start_no),
            ("intro_comma", "kept", self.intro_comma_kept),
            ("intro_comma", "dropped", self.intro_comma_dropped),
            ("conjunction_comma", "kept", self.conjunction_comma_kept),
            (
                "conjunction_comma",
                "dropped",
                self.conjunction_comma_dropped,
            ),
            ("final_period", "kept", self.final_period_kept),
            ("final_period", "dropped", self.final_period_dropped),
        ]
    }

    fn field(&mut self, habit: &str, outcome: &str) -> Option<&mut u64> {
        Some(match (habit, outcome) {
            ("boundary", "period") => &mut self.boundary_period,
            ("boundary", "comma") => &mut self.boundary_comma,
            ("boundary", "none") => &mut self.boundary_none,
            ("lowercase_start", "yes") => &mut self.lowercase_start_yes,
            ("lowercase_start", "no") => &mut self.lowercase_start_no,
            ("intro_comma", "kept") => &mut self.intro_comma_kept,
            ("intro_comma", "dropped") => &mut self.intro_comma_dropped,
            ("conjunction_comma", "kept") => &mut self.conjunction_comma_kept,
            ("conjunction_comma", "dropped") => &mut self.conjunction_comma_dropped,
            ("final_period", "kept") => &mut self.final_period_kept,
            ("final_period", "dropped") => &mut self.final_period_dropped,
            _ => return None,
        })
    }

    /// Counts as `writing-style.json` keeps them (`{"boundary": {"period": 3, ...}, ...}`);
    /// unknown habits and outcomes are ignored.
    pub fn from_json(value: &Value) -> Counts {
        let mut counts = Counts::default();
        for (habit, outcomes) in value.as_object().into_iter().flatten() {
            for (outcome, count) in outcomes.as_object().into_iter().flatten() {
                if let (Some(field), Some(count)) = (counts.field(habit, outcome), count.as_u64()) {
                    *field += count;
                }
            }
        }
        counts
    }

    pub fn to_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for (habit, outcome, count) in self.fields() {
            out.entry(habit)
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap()
                .insert(outcome.into(), json!(count));
        }
        Value::Object(out)
    }

    pub fn add(&mut self, other: &Counts) {
        for (habit, outcome, count) in other.fields() {
            *self.field(habit, outcome).unwrap() += count;
        }
    }

    fn total(&self) -> u64 {
        self.fields().iter().map(|(_, _, count)| count).sum()
    }
}

/// Count the user's punctuation choices in `written` against `shown`.
///
/// A teach reply is `deliberate`: what the user left is what they write. A
/// correction is not: one that only fixes words leaves the punctuation as it
/// was shown without choosing it, so it counts only if some punctuation or
/// capital changed.
pub fn observe(shown: &str, written: &str, deliberate: bool) -> Counts {
    let mut counts = Counts::default();
    let before: Vec<(&str, &str)> = py::split(shown).into_iter().map(split).collect();
    let after: Vec<(&str, &str)> = py::split(written).into_iter().map(split).collect();
    if before.is_empty() || after.is_empty() {
        return counts;
    }
    let before_keys: Vec<String> = before.iter().map(|(word, _)| key(word)).collect();
    let after_keys: Vec<String> = after.iter().map(|(word, _)| key(word)).collect();
    let mut aligned: Vec<Option<usize>> = vec![None; before.len()];
    for block in SequenceMatcher::new(&before_keys, &after_keys, false).get_matching_blocks() {
        for offset in 0..block.size {
            aligned[block.a + offset] = Some(block.b + offset);
        }
    }
    let get = |i: usize| aligned.get(i).copied().flatten();
    if !deliberate
        && aligned.iter().enumerate().all(|(i, j)| {
            j.is_none_or(|j| {
                let ((word, trail), (written_word, written_trail)) = (before[i], after[j]);
                trail == written_trail && capitalized(word) == capitalized(written_word)
            })
        })
    {
        return counts;
    }

    let record_start = |counts: &mut Counts, i: usize, j: usize| {
        let word = before[i].0;
        if capitalized(word) && !keeps_capital(word) {
            if capitalized(after[j].0) {
                counts.lowercase_start_no += 1;
            } else {
                counts.lowercase_start_yes += 1;
            }
        }
    };

    if get(0) == Some(0) {
        record_start(&mut counts, 0, 0);
    }
    let mut sentence_start = true;
    for (i, &(word, trail)) in before.iter().enumerate() {
        let last = i == before.len() - 1;
        if let Some(j) = get(i) {
            let written_trail = after[j].1;
            let written_end = py::final_char(written_trail);
            if last {
                // A period turned into "?" or "!" is a different mark, not a dropped one.
                if j == after.len() - 1 && !matches!(written_end, Some('?' | '!')) {
                    if is_boundary(trail) {
                        if is_boundary(written_trail) {
                            counts.final_period_kept += 1;
                        } else {
                            counts.final_period_dropped += 1;
                        }
                    } else if trail.is_empty() && is_boundary(written_trail) {
                        counts.final_period_kept += 1;
                    }
                }
            } else if get(i + 1) == Some(j + 1) {
                // Both words and the one after them survived, so the seam is comparable.
                if is_boundary(trail) && !is_abbreviation(word) {
                    // Python's `"" in "!?"` is true: a dropped mark counts as a period.
                    if is_boundary(written_trail) || py::in_chars(written_end, "!?") {
                        counts.boundary_period += 1;
                        record_start(&mut counts, i + 1, j + 1);
                    } else if written_trail == "," {
                        counts.boundary_comma += 1;
                    } else if written_trail.is_empty() {
                        counts.boundary_none += 1;
                    }
                } else if trail == "," && matches!(written_trail, "," | "") {
                    let kept = written_trail == ",";
                    if sentence_start {
                        if kept {
                            counts.intro_comma_kept += 1;
                        } else {
                            counts.intro_comma_dropped += 1;
                        }
                    } else if is_conjunction(before[i + 1].0) {
                        if kept {
                            counts.conjunction_comma_kept += 1;
                        } else {
                            counts.conjunction_comma_dropped += 1;
                        }
                    }
                }
            }
        }
        sentence_start = ends_sentence(trail);
    }
    counts
}

/// What the user does at a sentence break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Period,
    Comma,
    None,
}

impl Boundary {
    pub fn as_str(self) -> &'static str {
        match self {
            Boundary::Period => "period",
            Boundary::Comma => "comma",
            Boundary::None => "none",
        }
    }
}

/// The habits to apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Habits {
    pub boundary: Option<Boundary>,
    pub lowercase_start: bool,
    pub drop_intro_comma: bool,
    pub drop_conjunction_comma: bool,
    pub drop_final_period: bool,
}

impl Habits {
    pub fn to_json(&self) -> Value {
        json!({
            "boundary": self.boundary.map(Boundary::as_str),
            "lowercase_start": self.lowercase_start,
            "drop_intro_comma": self.drop_intro_comma,
            "drop_conjunction_comma": self.drop_conjunction_comma,
            "drop_final_period": self.drop_final_period,
        })
    }

    pub fn from_json(value: &Value) -> Habits {
        let flag = |name: &str| value[name].as_bool().unwrap_or(false);
        Habits {
            boundary: match value["boundary"].as_str() {
                Some("period") => Some(Boundary::Period),
                Some("comma") => Some(Boundary::Comma),
                Some("none") => Some(Boundary::None),
                _ => None,
            },
            lowercase_start: flag("lowercase_start"),
            drop_intro_comma: flag("drop_intro_comma"),
            drop_conjunction_comma: flag("drop_conjunction_comma"),
            drop_final_period: flag("drop_final_period"),
        }
    }
}

/// More than half of the habit's evidence, once there is enough of it.
fn mostly(chosen: u64, other: u64) -> bool {
    let total = chosen + other;
    total >= MIN_EVIDENCE && chosen as f64 / total as f64 > 0.5
}

/// Turn counts into the habits to apply; a habit needs [`MIN_EVIDENCE`] first.
pub fn decide(counts: &Counts) -> Habits {
    let outcomes = [
        (Boundary::Period, counts.boundary_period),
        (Boundary::Comma, counts.boundary_comma),
        (Boundary::None, counts.boundary_none),
    ];
    let mut boundary = None;
    if outcomes.iter().map(|(_, count)| count).sum::<u64>() >= MIN_EVIDENCE {
        // Ties keep the period; the user has to show the change more often than not.
        let rank = |(outcome, count): (Boundary, u64)| (count, outcome == Boundary::Period);
        let mut best = outcomes[0];
        for outcome in &outcomes[1..] {
            if rank(*outcome) > rank(best) {
                best = *outcome;
            }
        }
        boundary = Some(best.0);
    }
    Habits {
        boundary,
        lowercase_start: mostly(counts.lowercase_start_yes, counts.lowercase_start_no),
        drop_intro_comma: mostly(counts.intro_comma_dropped, counts.intro_comma_kept),
        drop_conjunction_comma: mostly(
            counts.conjunction_comma_dropped,
            counts.conjunction_comma_kept,
        ),
        drop_final_period: mostly(counts.final_period_dropped, counts.final_period_kept),
    }
}

/// Stable codes the app turns into plain-language lines.
pub fn summary(habits: &Habits) -> Vec<String> {
    let mut codes = Vec::new();
    if let Some(boundary) = habits.boundary {
        codes.push(format!("boundary_{}", boundary.as_str()));
    }
    for (code, on) in [
        ("lowercase_start", habits.lowercase_start),
        ("drop_intro_comma", habits.drop_intro_comma),
        ("drop_conjunction_comma", habits.drop_conjunction_comma),
        ("drop_final_period", habits.drop_final_period),
    ] {
        if on {
            codes.push(code.to_string());
        }
    }
    codes
}

// --- Applying ------------------------------------------------------------------

/// Re-punctuate `text` with the user's habits; words are never changed.
pub fn apply_style(text: &str, habits: &Habits) -> String {
    let codes = summary(habits);
    if text.is_empty() || codes.is_empty() || codes == ["boundary_period"] {
        return text.to_string();
    }
    let parts = re!(r"(\s+)").split(text);
    let mut words: Vec<String> = parts
        .iter()
        .step_by(2)
        .map(|part| part.to_string())
        .collect();
    let spaces: Vec<&str> = parts.iter().skip(1).step_by(2).copied().collect();
    let mut sentence_start = true;
    for index in 0..words.len() {
        if words[index].is_empty() {
            continue;
        }
        let token = words[index].clone();
        let (word, trail) = split(&token);
        let mut word = word.to_string();
        let mut trail = trail.to_string();
        if sentence_start && habits.lowercase_start && capitalized(&word) {
            word = lower_first(&word);
        }
        let following = words[index + 1..].iter().find(|w| !w.is_empty()).cloned();
        let gap = spaces.get(index).copied().unwrap_or("");
        let inline = following.is_some() && !gap.contains('\n');
        let mut next_start = ends_sentence(&trail) || gap.contains('\n');
        let joins = matches!(habits.boundary, Some(Boundary::Comma | Boundary::None));
        if inline
            && is_boundary(&trail)
            && capitalized(following.as_deref().unwrap())
            && !is_abbreviation(&word)
        {
            if joins {
                let mark = if habits.boundary == Some(Boundary::Comma) {
                    ","
                } else {
                    ""
                };
                trail = trail.replacen('.', mark, 1);
                words[index + 1] = lower_first(following.as_deref().unwrap());
                next_start = false;
            }
        } else if trail == "," && inline {
            let following = following.as_deref().unwrap();
            if (sentence_start && habits.drop_intro_comma)
                || (!sentence_start
                    && habits.drop_conjunction_comma
                    && is_conjunction(split(following).0))
            {
                trail.clear();
            }
        } else if following.is_none()
            && is_boundary(&trail)
            && trail.ends_with('.')
            && habits.drop_final_period
        {
            trail.pop();
        }
        words[index] = word + &trail;
        sentence_start = next_start;
    }
    let mut out = String::with_capacity(text.len());
    for (index, word) in words.iter().enumerate() {
        out.push_str(word);
        if let Some(space) = spaces.get(index) {
            out.push_str(space);
        }
    }
    out
}

// --- Profile -------------------------------------------------------------------

/// One teach reply or calibration rewrite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Example {
    /// What was said; `None` for a typed reply or a punctuation paragraph.
    pub said: Option<String>,
    pub shown: String,
    pub written: String,
}

/// What one writing style learned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub runs: u64,
    pub examples: Vec<Example>,
    pub feedback_counts: Counts,
}

impl Profile {
    /// A style's entry in `writing-style.json`; missing parts are empty.
    pub fn from_json(value: &Value) -> Profile {
        let examples = value["examples"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|example| {
                Some(Example {
                    said: example["said"].as_str().map(String::from),
                    shown: example["shown"].as_str()?.to_string(),
                    written: example["written"].as_str()?.to_string(),
                })
            })
            .collect();
        Profile {
            runs: value["runs"].as_u64().unwrap_or(0),
            examples,
            feedback_counts: Counts::from_json(&value["feedback_counts"]),
        }
    }

    fn counts(&self) -> Counts {
        let mut total = self.feedback_counts;
        for example in &self.examples {
            total.add(&observe(&example.shown, &example.written, true));
        }
        total
    }

    pub fn habits(&self) -> Habits {
        decide(&self.counts())
    }

    pub fn is_ready(&self) -> bool {
        self.runs > 0 || self.feedback_counts.total() >= 3
    }

    /// `text` re-punctuated the way this style learned, once it is ready.
    pub fn apply_learned(&self, text: &str) -> String {
        if self.is_ready() {
            apply_style(text, &self.habits())
        } else {
            text.to_string()
        }
    }

    /// Cleanup instructions describing how the user punctuates in this style.
    pub fn prompt_section(&self) -> Option<String> {
        if !self.is_ready() {
            return None;
        }
        let codes = summary(&self.habits());
        if codes.is_empty() {
            return None;
        }
        let lines: Vec<String> = codes
            .iter()
            .map(|code| format!("- {}", instruction(code)))
            .collect();
        Some(format!(
            "Punctuation style: match how this speaker writes.\n{}\n- Still use question marks for questions.",
            lines.join("\n")
        ))
    }

    /// The user's latest calibration rewrite that differs from what was shown,
    /// as (said, written).
    pub fn prompt_example(&self) -> Option<(String, String)> {
        if !self.is_ready() {
            return None;
        }
        self.examples
            .iter()
            .rev()
            .find(|example| py::strip(&example.written) != py::strip(&example.shown))
            .map(|example| {
                let said = example
                    .said
                    .clone()
                    .filter(|said| !said.is_empty())
                    .unwrap_or_else(|| as_spoken(&example.shown));
                (said, example.written.clone())
            })
    }
}

fn instruction(code: &str) -> &'static str {
    match code {
        "boundary_period" => {
            "End each thought with a period and start the next one as a new sentence."
        }
        "boundary_comma" => "Join related thoughts with commas instead of starting new sentences.",
        "boundary_none" => "Run related thoughts together without punctuation between them.",
        "lowercase_start" => {
            "Start sentences with a lowercase letter. Keep \"I\" and acronyms capitalized."
        }
        "drop_intro_comma" => {
            "Do not put a comma after opening words like \"yeah\", \"so\", \"okay\" or \"honestly\"."
        }
        "drop_conjunction_comma" => {
            "Do not put a comma before \"and\", \"but\", \"so\" or \"or\"."
        }
        "drop_final_period" => "Do not end the text with a period.",
        _ => unreachable!("unknown habit {code}"),
    }
}

/// What the speech-to-text step would hand cleanup for `text`.
pub fn as_spoken(text: &str) -> String {
    let words: Vec<String> = py::split(text)
        .into_iter()
        .map(|word| re!(r"[^\w'’$%-]").replace(word, ""))
        .collect();
    py::casefold(&words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn habits(boundary: Option<Boundary>) -> Habits {
        Habits {
            boundary,
            lowercase_start: true,
            drop_intro_comma: true,
            drop_conjunction_comma: true,
            drop_final_period: true,
        }
    }

    #[test]
    fn applies_casual_habits() {
        assert_eq!(
            apply_style(
                "Yeah, I tested it. It works, and we should ship it.",
                &habits(Some(Boundary::Comma))
            ),
            "yeah I tested it, it works and we should ship it"
        );
        assert_eq!(
            apply_style("Hello there.", &Habits::default()),
            "Hello there."
        );
    }

    #[test]
    fn counts_what_the_user_changed() {
        let counts = observe("So, it works. We ship.", "so it works, we ship", true);
        assert_eq!(counts.boundary_comma, 1);
        assert_eq!(counts.intro_comma_dropped, 1);
        assert_eq!(counts.final_period_dropped, 1);
        assert_eq!(counts.lowercase_start_yes, 1);
        assert_eq!(observe("It works.", "It works.", false), Counts::default());
    }

    #[test]
    fn a_profile_is_ready_after_a_run() {
        let profile = Profile {
            runs: 1,
            examples: vec![Example {
                said: None,
                shown: "So, it works. We ship. Done.".into(),
                written: "so it works, we ship, done".into(),
            }],
            feedback_counts: Counts::default(),
        };
        assert!(profile.is_ready());
        assert_eq!(profile.habits().boundary, Some(Boundary::Comma));
        assert_eq!(
            profile.prompt_example(),
            Some((
                "so it works we ship done".into(),
                "so it works, we ship, done".into()
            ))
        );
        assert!(profile.prompt_section().unwrap().contains("commas"));
        assert!(!Profile::default().is_ready());
    }
}
