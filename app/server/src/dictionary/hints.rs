//! Phrases the user fixed take after take, misheard a different way each
//! time: "merge to main" heard as "merge to mean", "merge the name", "and
//! domain". No one mishearing repeats, so no learned rule catches them, and
//! every word is common, so no corrected word does either. Whisper hears
//! them right when its prompt has the phrase: once one was fixed in
//! [`TAKES`] takes, [`store::add_prompted_phrases`](super::store::add_prompted_phrases)
//! keeps it in the dictionary as a prompt-only entry.

use std::collections::{BTreeMap, HashSet};

use kass_text::difflib::{SequenceMatcher, Tag};
use kass_text::lexicon;
use kass_text::py::{self, Groups};
use kass_text::re;

/// How many takes a phrase must have been fixed in: two can be chance.
pub const TAKES: usize = 3;

/// Phrase lengths, in words: one word is a corrected word's job, and five
/// is a sentence rather than something the user says often.
const SHORTEST: usize = 2;
const LONGEST: usize = 4;

/// A longer phrase stands for a shorter one in it when it holds at least
/// this share of the shorter one's takes: "merge to main" (7 takes) for "to
/// main" (8), but not "PR merge to main" (4) for "merge to main".
const HOLDS: (usize, usize) = (3, 4);

/// A phrase as the user wrote it, and its words casefolded as its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phrase {
    pub key: String,
    pub written: String,
}

/// Words, as the dictionary matches them.
fn word() -> &'static py::Re {
    re!(r"\w+(?:['’-]\w+)*")
}

/// A word written drawn out ("wayyy") is how it was said, not vocabulary.
fn drawn() -> &'static py::Re {
    re!(r"([^\W\d_])\1\1", py::I)
}

/// The phrases of two to four words in `corrected` that hold a word the
/// correction wrote over a misheard one: up to three words heard replaced
/// by up to three written. A phrase stays inside one sentence (a comma, a
/// pause, doesn't end one) and has no digits or drawn-out spelling.
pub fn fixed_phrases(original: &str, corrected: &str) -> Vec<Phrase> {
    fixed_phrases_with(original, corrected, &lexicon::common_word)
}

/// [`fixed_phrases`], deciding common words with `common`.
pub fn fixed_phrases_with(
    original: &str,
    corrected: &str,
    common: &dyn Fn(&str) -> bool,
) -> Vec<Phrase> {
    let heard: Vec<String> = word()
        .finditer(original)
        .iter()
        .map(|found| py::casefold(found.text()))
        .collect();
    let found = word().finditer(corrected);
    let wrote: Vec<String> = found.iter().map(|w| py::casefold(w.text())).collect();
    let mut fixed = vec![false; wrote.len()];
    for code in SequenceMatcher::new(&heard, &wrote, false).get_opcodes() {
        if code.tag == Tag::Replace
            && (1..=3).contains(&(code.i2 - code.i1))
            && (1..=3).contains(&(code.j2 - code.j1))
        {
            fixed[code.j1..code.j2].fill(true);
        }
    }
    let mut sentence = Vec::with_capacity(found.len());
    let (mut at, mut last) = (0, 0);
    for w in &found {
        if corrected[last..w.start()].contains(['.', '!', '?', ';', ':', '\n']) {
            at += 1;
        }
        sentence.push(at);
        last = w.end();
    }
    let mut seen = HashSet::new();
    let mut phrases = Vec::new();
    for len in SHORTEST..=LONGEST {
        for i in 0..wrote.len().saturating_sub(len - 1) {
            let j = i + len;
            if sentence[i] != sentence[j - 1] || !fixed[i..j].contains(&true) {
                continue;
            }
            let words: Vec<&str> = found[i..j].iter().map(|w| w.text()).collect();
            let written = words.join(" ");
            if written.chars().any(py::is_digit) || drawn().is_match(&written) {
                continue;
            }
            let key = wrote[i..j].join(" ");
            if !seen.insert(key.clone()) {
                continue;
            }
            let starts = i == 0 || sentence[i - 1] != sentence[i];
            phrases.push(Phrase {
                key,
                written: if starts {
                    uncapitalized(&written, words[0], common)
                } else {
                    written
                },
            });
        }
    }
    phrases
}

/// A phrase that starts a sentence, without the capital the sentence gave
/// its first word ("Merge to main"): kept for a name or "I".
fn uncapitalized(phrase: &str, first: &str, common: &dyn Fn(&str) -> bool) -> String {
    let mut chars = first.chars();
    let (Some(head), rest) = (chars.next(), chars.as_str()) else {
        return phrase.to_string();
    };
    let lower = py::casefold(first);
    if rest.is_empty() || py::isupper(rest) || lower == first || !common(&lower) {
        return phrase.to_string();
    }
    format!("{}{}", head.to_lowercase(), &phrase[head.len_utf8()..])
}

/// Whether `inner`'s words are some of `outer`'s, in a row.
fn within(inner: &str, outer: &str) -> bool {
    inner != outer && format!(" {outer} ").contains(&format!(" {inner} "))
}

/// The phrases to prompt, from how many takes fixed each (by key).
///
/// A phrase needs [`TAKES`] takes. Of phrases inside one another, the
/// longest that still holds most of a shorter one's takes ([`HOLDS`]) is
/// the one: "merge to main", not "to main" or "PR merge to main".
pub fn chosen(takes: &BTreeMap<String, usize>) -> Vec<&str> {
    let (part, whole) = HOLDS;
    let kept: Vec<(&str, usize)> = takes
        .iter()
        .filter(|(_, n)| **n >= TAKES)
        .map(|(key, n)| (key.as_str(), *n))
        .collect();
    kept.iter()
        .filter(|(key, n)| {
            kept.iter().all(|(other, m)| {
                if within(key, other) {
                    m * whole < n * part
                } else if within(other, key) {
                    n * whole >= m * part
                } else {
                    true
                }
            })
        })
        .map(|(key, _)| *key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMON: &[&str] = &["merge", "ask", "main", "to", "now", "it"];

    fn common(word: &str) -> bool {
        COMMON.contains(&word)
    }

    fn keys(original: &str, corrected: &str) -> Vec<String> {
        fixed_phrases_with(original, corrected, &common)
            .into_iter()
            .map(|phrase| phrase.key)
            .collect()
    }

    fn takes(counts: &[(&str, usize)]) -> BTreeMap<String, usize> {
        counts.iter().map(|(k, n)| (k.to_string(), *n)).collect()
    }

    #[test]
    fn phrases_hold_a_fixed_word() {
        assert_eq!(
            keys("commit and merge to mean", "commit and merge to main"),
            ["to main", "merge to main", "and merge to main"]
        );
    }

    #[test]
    fn a_fix_of_several_words_counts_each() {
        assert_eq!(
            keys("PR merge the name", "PR merge to main"),
            [
                "merge to",
                "to main",
                "pr merge to",
                "merge to main",
                "pr merge to main",
            ]
        );
    }

    #[test]
    fn nothing_without_a_misheard_word() {
        assert!(keys("merge to main", "merge to main").is_empty());
        assert!(keys("merge to main now", "merge to main").is_empty());
        assert!(keys("merge to main", "please merge to main").is_empty());
    }

    #[test]
    fn phrases_stay_in_one_sentence() {
        assert_eq!(keys("Done. Mean it", "Done. Main it"), ["main it"]);
        assert_eq!(
            keys("merge, to mean", "merge, to main"),
            ["to main", "merge to main"]
        );
    }

    #[test]
    fn no_digits_or_drawn_out_words() {
        assert!(keys("wait for too", "wait for 2").is_empty());
        assert!(keys("that was way", "that was wayyy").is_empty());
    }

    #[test]
    fn a_sentence_capital_is_dropped_but_a_name_kept() {
        let written = |original, corrected| -> Vec<String> {
            fixed_phrases_with(original, corrected, &common)
                .into_iter()
                .map(|phrase| phrase.written)
                .collect()
        };
        assert_eq!(
            written("Merge to mean", "Merge to main"),
            ["to main", "merge to main"]
        );
        assert_eq!(
            written("Ask clod now", "Ask Claude now"),
            ["ask Claude", "Claude now", "ask Claude now"]
        );
        assert_eq!(
            written("I mean it", "I main it"),
            ["I main", "main it", "I main it"]
        );
        assert_eq!(written("PR merged", "PR merge"), ["PR merge"]);
    }

    #[test]
    fn a_phrase_needs_three_takes() {
        assert_eq!(chosen(&takes(&[("merge to main", 3)])), ["merge to main"]);
        assert!(chosen(&takes(&[("merge to main", 2)])).is_empty());
    }

    #[test]
    fn the_longest_phrase_holding_most_takes_wins() {
        let real = takes(&[
            ("to main", 8),
            ("merge to main", 7),
            ("merge to", 5),
            ("pr merge to main", 4),
            ("pr merge to", 3),
        ]);
        assert_eq!(chosen(&real), ["merge to main"]);
    }

    #[test]
    fn unrelated_phrases_are_each_kept() {
        let found = takes(&[("merge to main", 3), ("mark it as done", 4)]);
        assert_eq!(chosen(&found), ["mark it as done", "merge to main"]);
    }
}
