//! How a speaker writes the way things were said, learned from their own
//! edits. Python: `backend/services/expression_learning.py`.
//!
//! The defaults are strict: not everyone wants "!" or "wayyy" from their
//! voice. A speaker who wants more shows it by editing their text:
//!
//! - A period they change to "!" on a sentence they said with some energy
//!   lowers their "!" cutoff toward it. A "!" they change back raises it.
//! - A word they write drawn out ("way" to "wayyy") teaches what their own
//!   stretches sound like; one they write plainly again stops it.
//!
//! A redictation is weaker evidence (the user may only have reworded): it
//! can make the rules stricter, never looser. The rule never moves on one
//! edit, and never so far that more than a few of the speaker's own plain
//! sentences or words would change.

use std::collections::{BTreeSet, HashMap, HashSet};

use kass_text::difflib::SequenceMatcher;
use serde::Deserialize;

use super::math::{quantile, round2};
use super::text::{is_function_word, key};
use super::voice::{
    StretchRule, WrittenEnergy, EXCLAIM_ENERGY, STRETCH_LOUDER_DB, STRETCH_PACE, STRETCH_PLATEAU_S,
};

/// Edits that must agree before a rule moves.
const MIN_EXAMPLES: usize = 2;
/// A learned "!" cutoff never goes below this: a "!" typed on a sentence
/// said calmly is the user's taste in punctuation, not something heard.
const MIN_CUTOFF: f64 = 1.5;
/// How far past an edit's sentence or word the rule moves.
const ENERGY_MARGIN: f64 = 0.25;
const PACE_MARGIN: f64 = 0.9;
const LOUDER_MARGIN_DB: f64 = 0.5;
const PLATEAU_MARGIN_S: f64 = 0.05;
/// Bounds no learned stretch rule passes.
const MIN_PACE: f64 = 1.5;
const MIN_LOUDER_DB: f64 = -2.0;
const PLATEAU_LIMITS_S: (f64, f64) = (0.1, 1.0);
/// At most this share of the speaker's own plain sentences and words may
/// change.
const PLAIN_SHARE: f64 = 0.02;
const STRETCH_SHARE: f64 = 0.005;
/// Plain sentences needed before they bound the cutoff.
const MIN_PLAIN: usize = 50;
/// What may close a sentence after its mark.
const CLOSERS: &str = "\"')]\u{201d}\u{2019}";

/// A speaker's own "!" cutoff and stretch rule; the defaults until they
/// teach otherwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Learned {
    pub cutoff: f64,
    pub rule: StretchRule,
}

impl Default for Learned {
    fn default() -> Self {
        Self {
            cutoff: EXCLAIM_ENERGY,
            rule: StretchRule::default(),
        }
    }
}

/// How drawn out a word was, as saved in `shapes`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "(String, f64, f64, f64)")]
pub struct Shape {
    pub word: String,
    pub stretch: f64,
    pub louder: f64,
    pub plateau: f64,
}

impl From<(String, f64, f64, f64)> for Shape {
    fn from((word, stretch, louder, plateau): (String, f64, f64, f64)) -> Self {
        Self {
            word,
            stretch,
            louder,
            plateau,
        }
    }
}

impl Shape {
    fn drawn_by(&self, rule: &StretchRule) -> bool {
        rule.holds(&self.word, self.stretch, self.louder, self.plateau)
    }
}

/// The parts of a capture's saved measurements learning reads.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub written: Vec<WrittenEnergy>,
}

/// A capture's saved measurements and the text it delivered.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedCapture {
    pub id: String,
    pub saved: Saved,
    pub delivered: String,
}

/// What the speaker's edits and plain dictations show.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub wanted: Vec<f64>,
    pub unwanted: Vec<f64>,
    pub plain: Vec<f64>,
    pub stretched: Vec<Shape>,
    pub unstretched: Vec<Shape>,
    /// Every content word of the speaker's dictations, as (capture, index,
    /// shape).
    pub shapes: Vec<(String, usize, Shape)>,
    pub edited_shapes: HashSet<(String, usize)>,
}

/// A word with every repeated letter written once: "wayyy" and "way"
/// agree, so do "goood" and "good".
fn plain(word: &str) -> String {
    let mut collapsed = String::new();
    for c in key(word).chars() {
        if !collapsed.ends_with(c) {
            collapsed.push(c);
        }
    }
    collapsed
}

/// `re.findall(r"\S.*?[.!?][closers]*(?=\s|$)", text, re.DOTALL)`: each
/// sentence that ends with a mark, its closers and then a space or the end.
fn sentences(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte = |index: usize| chars.get(index).map_or(text.len(), |(at, _)| *at);
    let mut found = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars[start].1.is_whitespace() {
            start += 1;
            continue;
        }
        let end = (start + 1..chars.len()).find_map(|mark| {
            if !matches!(chars[mark].1, '.' | '!' | '?') {
                return None;
            }
            let closed = mark
                + 1
                + chars[mark + 1..]
                    .iter()
                    .take_while(|(_, c)| CLOSERS.contains(*c))
                    .count();
            (closed == chars.len() || chars[closed].1.is_whitespace()).then_some(closed)
        });
        // Any later start would end at the same marks, so none is found.
        let Some(end) = end else { break };
        found.push(&text[byte(start)..byte(end)]);
        start = end;
    }
    found
}

/// The mark that ends a sentence, before its closers.
fn ending(sentence: &str) -> Option<char> {
    let mark = sentence
        .trim_end_matches(|c| CLOSERS.contains(c))
        .chars()
        .next_back()?;
    matches!(mark, '.' | '!' | '?').then_some(mark)
}

fn words_key(sentence: &str) -> Vec<String> {
    sentence
        .split_whitespace()
        .map(plain)
        .filter(|word| !word.is_empty())
        .collect()
}

/// Three of the same letter in a row ("wayyy").
fn drawn(word: &str) -> bool {
    let letters: Vec<char> = word.chars().collect();
    letters.windows(3).any(|run| {
        run[0].is_ascii_alphabetic() && run[1..].iter().all(|c| c.eq_ignore_ascii_case(&run[0]))
    })
}

/// Evidence from each capture's saved measurements and, for an edited one,
/// its text as the user left it. `edits` maps a capture id to its edited
/// text. An edit in `redictated` counts only where it makes the rules
/// stricter.
pub fn gather(
    captures: &[SavedCapture],
    edits: &HashMap<String, String>,
    redictated: &HashSet<String>,
) -> Evidence {
    let mut evidence = Evidence::default();
    for capture in captures {
        let shapes = &capture.saved.shapes;
        evidence.shapes.extend(
            shapes
                .iter()
                .enumerate()
                .map(|(index, shape)| (capture.id.clone(), index, shape.clone())),
        );
        let written = &capture.saved.written;
        let Some(edited) = edits.get(&capture.id) else {
            evidence.plain.extend(
                written
                    .iter()
                    .filter(|sentence| ending(&sentence.text) == Some('.'))
                    .map(|sentence| sentence.energy),
            );
            continue;
        };
        let strict_only = redictated.contains(&capture.id);
        let endings: HashMap<Vec<String>, Option<char>> = sentences(edited)
            .into_iter()
            .map(|sentence| (words_key(sentence), ending(sentence)))
            .collect();
        for sentence in written {
            let before = ending(&sentence.text);
            let after = endings.get(&words_key(&sentence.text)).copied().flatten();
            match (before, after) {
                (Some('.'), Some('!')) if !strict_only => evidence.wanted.push(sentence.energy),
                (Some('!'), Some('.')) => evidence.unwanted.push(sentence.energy),
                (Some('.'), Some('.')) => evidence.plain.push(sentence.energy),
                _ => {}
            }
        }
        gather_stretches(&mut evidence, capture, edited, strict_only);
    }
    evidence
}

fn gather_stretches(
    evidence: &mut Evidence,
    capture: &SavedCapture,
    edited: &str,
    strict_only: bool,
) {
    let before: Vec<&str> = capture.delivered.split_whitespace().collect();
    let after: Vec<&str> = edited.split_whitespace().collect();
    let plain_before: Vec<String> = before.iter().map(|word| plain(word)).collect();
    let plain_after: Vec<String> = after.iter().map(|word| plain(word)).collect();
    for block in SequenceMatcher::new(&plain_before, &plain_after, false).get_matching_blocks() {
        for offset in 0..block.size {
            let (was, now) = (before[block.a + offset], after[block.b + offset]);
            let (drawn_before, drawn_now) = (drawn(was), drawn(now));
            if drawn_before == drawn_now || (drawn_now && strict_only) {
                continue;
            }
            // The most drawn-out reading of that word in the recording.
            let word = &plain_before[block.a + offset];
            let mut most: Option<(usize, &Shape)> = None;
            for (index, shape) in capture.saved.shapes.iter().enumerate() {
                if plain(&shape.word) == *word
                    && most.is_none_or(|(_, best)| shape.stretch > best.stretch)
                {
                    most = Some((index, shape));
                }
            }
            let Some((index, shape)) = most else { continue };
            evidence.edited_shapes.insert((capture.id.clone(), index));
            if drawn_now {
                evidence.stretched.push(shape.clone());
            } else {
                evidence.unstretched.push(shape.clone());
            }
        }
    }
}

pub fn learn(evidence: &Evidence) -> Learned {
    Learned {
        cutoff: cutoff(evidence),
        rule: rule(evidence),
    }
}

fn cutoff(evidence: &Evidence) -> f64 {
    let mut cutoff = EXCLAIM_ENERGY;
    // A "!" typed on a sentence said calmly isn't about the voice.
    let mut wanted: Vec<f64> = evidence
        .wanted
        .iter()
        .copied()
        .filter(|energy| *energy >= MIN_CUTOFF)
        .collect();
    wanted.sort_by(f64::total_cmp);
    if wanted.len() >= MIN_EXAMPLES {
        cutoff = cutoff.min(wanted[0] - ENERGY_MARGIN);
        if evidence.plain.len() >= MIN_PLAIN {
            cutoff = cutoff.max(quantile(&evidence.plain, 1.0 - PLAIN_SHARE));
        }
        cutoff = cutoff.max(MIN_CUTOFF);
    }
    if let Some(most) = evidence.unwanted.iter().copied().reduce(f64::max) {
        cutoff = cutoff.max(most + ENERGY_MARGIN);
    }
    round2(cutoff)
}

fn rule(evidence: &Evidence) -> StretchRule {
    let mut rule = StretchRule::default();
    let wanted = &evidence.stretched;
    if wanted.len() >= MIN_EXAMPLES {
        let least = |measure: fn(&Shape) -> f64| {
            wanted
                .iter()
                .map(measure)
                .reduce(f64::min)
                .unwrap_or(f64::NAN)
        };
        let most_plateau = wanted
            .iter()
            .map(|shape| shape.plateau)
            .reduce(f64::max)
            .unwrap_or(f64::NAN);
        rule = StretchRule {
            pace: MIN_PACE.max(STRETCH_PACE.min(least(|s| s.stretch) * PACE_MARGIN)),
            louder_db: MIN_LOUDER_DB
                .max(STRETCH_LOUDER_DB.min(least(|s| s.louder) - LOUDER_MARGIN_DB)),
            plateau: (
                PLATEAU_LIMITS_S.0.max(
                    STRETCH_PLATEAU_S
                        .0
                        .min(least(|s| s.plateau) - PLATEAU_MARGIN_S),
                ),
                PLATEAU_LIMITS_S
                    .1
                    .min(STRETCH_PLATEAU_S.1.max(most_plateau + PLATEAU_MARGIN_S)),
            ),
            words: wanted
                .iter()
                .map(|shape| key(&shape.word))
                .filter(|word| is_function_word(word))
                .collect::<BTreeSet<_>>(),
        };
        // Slower until few of the speaker's own plain words would be drawn
        // out.
        let plain: Vec<&Shape> = evidence
            .shapes
            .iter()
            .filter(|(id, index, _)| !evidence.edited_shapes.contains(&(id.clone(), *index)))
            .map(|(_, _, shape)| shape)
            .collect();
        let allowed = ((STRETCH_SHARE * plain.len() as f64) as usize).max(1);
        while rule.pace < STRETCH_PACE
            && plain.iter().filter(|shape| shape.drawn_by(&rule)).count() > allowed
        {
            let pace = STRETCH_PACE.min(rule.pace * 1.1);
            rule = with_pace(rule, pace);
        }
    }
    for shape in &evidence.unstretched {
        if shape.drawn_by(&rule) {
            rule = with_pace(rule, shape.stretch + 0.1);
        }
    }
    rule
}

fn with_pace(rule: StretchRule, pace: f64) -> StretchRule {
    StretchRule {
        pace: round2(pace),
        ..rule
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(text: &str, energy: f64) -> WrittenEnergy {
        WrittenEnergy {
            text: text.to_string(),
            energy,
        }
    }

    fn capture(
        id: &str,
        written: Vec<WrittenEnergy>,
        shapes: Vec<Shape>,
        delivered: &str,
    ) -> SavedCapture {
        SavedCapture {
            id: id.to_string(),
            saved: Saved { shapes, written },
            delivered: delivered.to_string(),
        }
    }

    #[test]
    fn finds_sentences_as_python_does() {
        assert_eq!(sentences("Hi there. Ok?\" then"), ["Hi there.", "Ok?\""]);
        assert_eq!(sentences("v1.2 is out. Yes!"), ["v1.2 is out.", "Yes!"]);
        assert_eq!(sentences("!! hi."), ["!!", "hi."]);
        assert!(sentences("no ending").is_empty());
        assert_eq!(ending("Done.\u{201d}"), Some('.'));
        assert_eq!(ending("Done"), None);
        assert_eq!(plain("Wayyy!"), "way");
        assert!(drawn("sooo,") && drawn("AaA") && !drawn("good"));
    }

    #[test]
    fn two_wanted_exclamations_lower_the_cutoff() {
        let edits = HashMap::from([
            ("a".to_string(), "We did it! Fine.".to_string()),
            ("b".to_string(), "Great news!".to_string()),
        ]);
        let captures = [
            capture(
                "a",
                vec![written("We did it.", 2.0), written("Fine.", 0.1)],
                vec![],
                "",
            ),
            capture("b", vec![written("Great news.", 2.4)], vec![], ""),
        ];
        let learned = learn(&gather(&captures, &edits, &HashSet::new()));
        assert_eq!(learned.cutoff, 1.75);
        // A redictation only makes things stricter.
        let redictated = HashSet::from(["b".to_string()]);
        assert_eq!(learn(&gather(&captures, &edits, &redictated)).cutoff, 2.9);
    }

    #[test]
    fn drawn_out_edits_teach_the_stretch_rule() {
        let shape = |word: &str, stretch: f64| Shape {
            word: word.to_string(),
            stretch,
            louder: 1.0,
            plateau: 0.35,
        };
        let edits = HashMap::from([
            ("a".to_string(), "It was wayyy better".to_string()),
            ("b".to_string(), "That is sooo good".to_string()),
        ]);
        let captures = [
            capture(
                "a",
                vec![],
                vec![shape("was", 1.0), shape("way", 4.0)],
                "It was way better",
            ),
            capture(
                "b",
                vec![],
                vec![shape("is", 1.0), shape("so", 3.0)],
                "That is so good",
            ),
        ];
        let learned = learn(&gather(&captures, &edits, &HashSet::new()));
        assert!((learned.rule.pace - 2.7).abs() < 1e-9);
        assert_eq!(learned.rule.louder_db, 0.5);
        assert_eq!(learned.rule.plateau, (0.3, 0.6));
        assert_eq!(learned.rule.words, BTreeSet::from(["so".to_string()]));
    }
}
