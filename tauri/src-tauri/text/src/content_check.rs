//! Does cleaned-up text still say what the speaker said?
//!
//! Cleanup may restructure: drop false starts and repeats, reorder a jumbled
//! sentence, fix grammar. It must not add ideas, summarize, or leave something
//! out. Three rules, by comparing words (no second model):
//!
//! 1. What the speaker said that carries meaning must survive: every number,
//!    technical term, name and spoken command, whether they said "not", who
//!    does what to whom ("you ... for me" never becomes "I ... for you"), and
//!    all but a few of their other words. Losing any of it rejects.
//! 2. Anything the cleanup added is listed for review, never rejected on its
//!    own, except a "not" nobody said. Formatting such as list numbers lands
//!    here.
//! 3. Many added words, a cleanup that is mostly (or only) words nobody said,
//!    or a question that comes back without a question mark, means the model
//!    answered or obeyed the dictation instead of cleaning it up; that
//!    rejects.
//!
//! Verdicts: `ok` (only structure changed), `review` (the cleanup is kept and
//! the capture flagged for the user to check) and `reject` (the transcript is
//! pasted instead and the capture flagged).
//!
//! `check_refinement` stays in Python until the refinement prompt is ported:
//! it needs `prepare_refinement` and the examples the model is shown.

use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

use crate::difflib::{SequenceMatcher, Tag};
use crate::py::{self, Groups, Re};
use crate::re;
use crate::voice_commands::count_commands;

const FUNCTION_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "but",
    "so",
    "yet",
    "nor",
    "to",
    "of",
    "in",
    "on",
    "at",
    "for",
    "with",
    "from",
    "by",
    "as",
    "into",
    "onto",
    "about",
    "than",
    "then",
    "that",
    "this",
    "these",
    "those",
    "there",
    "here",
    "it",
    "its",
    "it's",
    "i",
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "me",
    "my",
    "mine",
    "you",
    "you're",
    "your",
    "yours",
    "we",
    "we're",
    "our",
    "us",
    "they",
    "they're",
    "their",
    "them",
    "he",
    "she",
    "his",
    "her",
    "him",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "am",
    "do",
    "does",
    "did",
    "done",
    "have",
    "has",
    "had",
    "having",
    "will",
    "would",
    "can",
    "could",
    "should",
    "shall",
    "may",
    "might",
    "must",
    "just",
    "like",
    "really",
    "very",
    "actually",
    "basically",
    "literally",
    "kind",
    "sort",
    "mean",
    "know",
    "well",
    "okay",
    "ok",
    "yeah",
    "oh",
    "um",
    "uh",
    "er",
    "hmm",
    "ah",
    "some",
    "any",
    "all",
    "what",
    "which",
    "who",
    "whom",
    "whose",
    "when",
    "where",
    "why",
    "how",
    "if",
    "because",
    "while",
    "also",
    "too",
    "wait",
    "right",
    "anyway",
];
/// "no" is left out: as an interjection ("no wait, actually...") a cleanup drops it.
const NEGATIONS: &[&str] = &[
    "not",
    "never",
    "nothing",
    "none",
    "nobody",
    "neither",
    "nor",
    "cannot",
    "can't",
    "don't",
    "doesn't",
    "didn't",
    "isn't",
    "aren't",
    "wasn't",
    "weren't",
    "won't",
    "wouldn't",
    "shouldn't",
    "couldn't",
    "haven't",
    "hasn't",
    "hadn't",
    "mustn't",
    "without",
];
const FILLERS: &[&str] = &["um", "uh", "er", "hmm", "ah"];
const FIRST_PERSON: &[&str] = &[
    "i", "me", "my", "mine", "myself", "i'm", "i've", "i'll", "i'd",
];
const SECOND_PERSON: &[&str] = &[
    "you", "your", "yours", "yourself", "you're", "you've", "you'll", "you'd",
];

/// The model answered or obeyed instead of cleaning up when it added this
/// many content words, or this share of the transcript's, whichever is larger.
pub const ANSWERED_WORDS: usize = 4;
pub const ANSWERED_SHARE: f64 = 0.5;
/// A word or two dropped is normal for false starts; a larger share of a
/// short take gets a review.
pub const MISSING_SHARE: f64 = 0.3;
/// A cleanup that leaves out more than this share of the speaker's content
/// words dropped what they said: a request, a question, a sentence. A
/// rejected cleanup pastes the transcript instead, which costs only polish.
pub const DROPPED_SHARE: f64 = 0.15;
/// And at least this many: a short take loses a third of its content words
/// to one dropped hedge or false start ("I think it's ready" -> "It's ready").
pub const DROPPED_WORDS: usize = 2;
/// This many consecutive output words the speaker never said, found in an
/// example the model was shown, means it copied the example.
pub const COPIED_RUN: usize = 3;

/// Spoken quotes, brackets and symbols, counted per group.
const MARKS: &[&str] = &[
    "\"\u{201c}\u{201d}",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "<",
    ">",
    "/",
    "\\",
    "|",
    "^",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    pub outcome: String,
    pub added: Vec<String>,
    pub missing: Vec<String>,
    pub reason: Option<String>,
}

impl Verdict {
    fn new(
        outcome: &str,
        reason: Option<&str>,
        added: Vec<String>,
        missing: Vec<String>,
    ) -> Verdict {
        Verdict {
            outcome: outcome.to_string(),
            added,
            missing,
            reason: reason.map(String::from),
        }
    }

    fn ok() -> Verdict {
        Verdict::new("ok", None, Vec::new(), Vec::new())
    }

    fn reject(reason: &str, added: Vec<String>, missing: Vec<String>) -> Verdict {
        Verdict::new("reject", Some(reason), added, missing)
    }
}

/// One flag for a capture, from its phrases' verdicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub outcome: String,
    pub added: Vec<String>,
    pub missing: Vec<String>,
    pub reasons: Vec<String>,
}

fn nfkc(text: &str) -> String {
    text.nfkc().collect()
}

fn ignored(token: &str) -> bool {
    FUNCTION_WORDS.contains(&token) || NEGATIONS.contains(&token) || token == "no"
}

fn tokens(text: &str) -> Vec<String> {
    let normalized = py::casefold(&nfkc(text))
        .replace('\u{2019}', "'")
        .replace('\u{2212}', "-");
    let mut tokens = Vec::new();
    for token in re!(r"[+-]?\d+(?:[.,]\d+)*%?|[\w'./-]*\w").findall(&normalized) {
        // A sign belongs to a number ("-3.5"); anywhere else it is punctuation.
        let token = if is_number(py::rstrip_chars(token, ".")) {
            py::rstrip_chars(token, "./-'")
        } else {
            py::strip_chars(token, "./-'")
        };
        if !token.is_empty() && !FILLERS.contains(&token) {
            tokens.push(token.to_string());
        }
    }
    tokens
}

fn is_number(token: &str) -> bool {
    re!(r"[+-]?\d+(?:[.,]\d+)*%?").fullmatch(token).is_some()
}

fn is_technical(token: &str) -> bool {
    re!(r"\w[_/.]\w").is_match(token) && !is_number(token)
}

fn parts(token: &str) -> Vec<&str> {
    re!(r"[_/.-]+")
        .split(token)
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect()
}

/// Words capitalized mid-sentence: names, places, products ("Walmart").
///
/// Whisper capitalizes proper nouns, so this needs no model. A capital at
/// the start of a sentence, and "I", say nothing about the word.
fn names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let text = nfkc(text).replace('\u{2019}', "'");
    for found in re!(r"[\w'][\w'.-]*\w|\w").finditer(&text) {
        let word = found.text();
        let before = py::rstrip(&text[..found.start()]);
        if !py::isupper(&py::first(word).unwrap().to_string())
            || before.is_empty()
            || ".!?:\n".contains(py::final_char(before).unwrap())
            || re!(r"I('\w+)?").fullmatch(word).is_some()
        {
            continue;
        }
        let name = py::casefold(word);
        let name = py::strip_chars(&name, "'.-");
        // Whisper also capitalizes a word that restarts a cut-off sentence
        // ("It might But we'll see"); common words are never names.
        if !FUNCTION_WORDS.contains(&name) && !NEGATIONS.contains(&name) {
            names.insert(name.to_string());
        }
    }
    names
}

fn number(token: &str) -> String {
    token.replace(',', "")
}

fn correction_cue() -> &'static Re {
    re!(
        r"\b(actually|no wait|no actually|make that|I mean|scratch that)\b",
        py::I
    )
}

/// What the speaker took back: the words before their last correction cue,
/// and `retractable`.
struct Retracted {
    spoken: Option<String>,
}

impl Retracted {
    fn new(said: &str, retractable: &str, allowed: bool) -> Retracted {
        if !allowed {
            return Retracted { spoken: None };
        }
        let cues = correction_cue().finditer(said);
        let spoken = cues.last().map_or("", |cue| &said[..cue.start()]);
        let spoken = py::casefold(&nfkc(&format!("{spoken} {retractable}"))).replace(',', "");
        Retracted {
            spoken: Some(spoken),
        }
    }

    fn contains(&self, word: &str) -> bool {
        let Some(spoken) = &self.spoken else {
            return false;
        };
        let pattern = format!(r"(?<![\w]){}(?![\w])", py::escape(&word.replace(',', "")));
        Re::new(&pattern).is_match(spoken)
    }
}

fn count_marks(text: &str, marks: &str) -> usize {
    marks.chars().map(|mark| text.matches(mark).count()).sum()
}

fn sorted<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    items
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(String::from)
        .collect()
}

/// Compare `cleaned` against the transcript `said`.
///
/// With `allow_retractions`, a number, name or term said before a spoken
/// correction ("Tuesday, actually Wednesday"), or in `retractable`, may be
/// left out.
pub fn check(said: &str, cleaned: &str, allow_retractions: bool, retractable: &str) -> Verdict {
    let retracted = Retracted::new(said, retractable, allow_retractions);
    let before = tokens(said);
    let after = tokens(cleaned);
    let mut heard_parts: BTreeSet<&str> = before.iter().map(String::as_str).collect();
    heard_parts.extend(before.iter().flat_map(|token| parts(token)));
    let kept: BTreeSet<&str> = after.iter().map(String::as_str).collect();
    let mut kept_parts = kept.clone();
    kept_parts.extend(after.iter().flat_map(|token| parts(token)));
    let numbers_before: BTreeSet<String> = before
        .iter()
        .filter(|t| is_number(t))
        .map(|t| number(t))
        .collect();
    let numbers_after: BTreeSet<String> = after
        .iter()
        .filter(|t| is_number(t))
        .map(|t| number(t))
        .collect();
    let added_numbers = sorted(
        numbers_after
            .difference(&numbers_before)
            .map(String::as_str),
    );

    // Rule 1: what the speaker said that carries meaning must survive.
    // "index dot tsx" may become "index.tsx", so a said word also survives as
    // part of a joined technical term.
    let lost_numbers = sorted(
        numbers_before
            .difference(&numbers_after)
            .map(String::as_str)
            .filter(|n| !retracted.contains(n)),
    );
    if !lost_numbers.is_empty() {
        return Verdict::reject("number", added_numbers, lost_numbers);
    }
    let negations_before: BTreeSet<&str> = before
        .iter()
        .map(String::as_str)
        .filter(|t| NEGATIONS.contains(t))
        .collect();
    let negations_after: BTreeSet<&str> = after
        .iter()
        .map(String::as_str)
        .filter(|t| NEGATIONS.contains(t))
        .collect();
    // "don't" may become "do not"; only losing every negation, or adding one
    // where none was said, flips the meaning.
    if negations_before.is_empty() != negations_after.is_empty() {
        return Verdict::reject(
            "negation",
            sorted(negations_after.difference(&negations_before).copied()),
            sorted(negations_before.difference(&negations_after).copied()),
        );
    }
    if count_commands(cleaned) != count_commands(said) {
        return Verdict::reject("command", Vec::new(), Vec::new());
    }
    // Spoken quotes, brackets and symbols are in `said` as characters; losing
    // one loses what the speaker asked for.
    if MARKS
        .iter()
        .any(|marks| count_marks(cleaned, marks) < count_marks(said, marks))
    {
        return Verdict::reject("command", Vec::new(), Vec::new());
    }
    let lost_terms = sorted(
        before
            .iter()
            .map(String::as_str)
            .filter(|t| is_technical(t) && !kept.contains(t) && !retracted.contains(t)),
    );
    if !lost_terms.is_empty() {
        return Verdict::reject("technical", Vec::new(), lost_terms);
    }
    let said_names = names(said);
    let lost_names = sorted(
        said_names
            .iter()
            .map(String::as_str)
            .filter(|name| !kept_parts.contains(name) && !retracted.contains(name)),
    );
    if !lost_names.is_empty() {
        return Verdict::reject("name", Vec::new(), lost_names);
    }

    let content_before: Vec<&str> = before
        .iter()
        .map(String::as_str)
        .filter(|t| !ignored(t) && !is_number(t))
        .collect();
    let content_after: Vec<&str> = after
        .iter()
        .map(String::as_str)
        .filter(|t| !ignored(t) && !is_number(t))
        .collect();
    let content_said: BTreeSet<&str> = content_before.iter().copied().collect();
    let content_kept: BTreeSet<&str> = content_after.iter().copied().collect();
    let mut said_words = content_said.clone();
    said_words.extend(heard_parts.iter().copied());
    // Rule 2: anything the cleanup added is listed for review, never rejected
    // on its own. List numbering, "1st", headings are formatting it may add.
    let added =
        sorted(content_after.iter().copied().filter(|t| {
            !said_words.contains(t) && !parts(t).iter().all(|p| said_words.contains(p))
        }));
    let after_parts: BTreeSet<&str> = content_after.iter().flat_map(|t| parts(t)).collect();
    let missing = sorted(
        content_said
            .iter()
            .copied()
            .filter(|t| !content_kept.contains(t) && !after_parts.contains(t)),
    );

    // Rule 3: many new words, or new words outnumbering the ones kept, means
    // the model answered or obeyed the dictation. So does keeping none of the
    // speaker's words at all ("Thank you." -> "You're welcome."), or turning a
    // question into a statement, even when the answer reuses the speaker's
    // words.
    if said.contains('?') && !cleaned.contains('?') {
        return Verdict::reject("answered", added, missing);
    }
    let kept_content: BTreeSet<&str> = content_kept
        .iter()
        .copied()
        .filter(|t| !added.iter().any(|a| a == t))
        .collect();
    let said_count = content_said.len() as f64;
    if added.len() as f64 >= (ANSWERED_WORDS as f64).max(ANSWERED_SHARE * said_count)
        || (added.len() > kept_content.len() && (added.len() >= 2 || kept_content.is_empty()))
    {
        return Verdict::reject("answered", added, missing);
    }
    let dropped = missing.iter().filter(|t| !retracted.contains(t)).count();
    if dropped >= DROPPED_WORDS && dropped as f64 > DROPPED_SHARE * said_count {
        return Verdict::reject("dropped", added, missing);
    }
    if swapped_people(&before, &after) {
        return Verdict::reject("person", added, missing);
    }
    // A new word in place of a heard one may fix a mishearing ("merge domain"
    // -> "merge to main"); one put between heard words replaces nothing, so
    // the model made it up ("Merge." -> "Merge to main").
    let invented = sorted(
        inserted(&before, &after)
            .into_iter()
            .filter(|t| added.iter().any(|a| a == t)),
    );
    if !invented.is_empty() {
        return Verdict::reject("invented", invented, missing);
    }
    let added = sorted(added.iter().chain(added_numbers.iter()).map(String::as_str));
    if !added.is_empty() || missing.len() as f64 > MISSING_SHARE * said_count {
        return Verdict::new("review", None, added, missing);
    }
    Verdict::ok()
}

/// Words of `after` that sit between kept words without replacing any heard one.
fn inserted<'t>(before: &[String], after: &'t [String]) -> Vec<&'t str> {
    SequenceMatcher::new(before, after, false)
        .get_opcodes()
        .into_iter()
        .filter(|op| op.tag == Tag::Insert)
        .flat_map(|op| after[op.j1..op.j2].iter().map(String::as_str))
        .collect()
}

fn people(tokens: &[String]) -> Vec<u8> {
    tokens
        .iter()
        .filter_map(|t| {
            if FIRST_PERSON.contains(&t.as_str()) {
                Some(1)
            } else if SECOND_PERSON.contains(&t.as_str()) {
                Some(2)
            } else {
                None
            }
        })
        .collect()
}

/// Whether a "you" became "I" and an "I" became "you" in the same places.
///
/// "so that you can pull some data for me" -> "so that I can pull some data
/// for you" keeps every word, so no other rule sees it, yet it reverses who
/// asked for what. Pronouns are compared in order; only one of each turning
/// into the other counts.
fn swapped_people(before: &[String], after: &[String]) -> bool {
    let (said, cleaned) = (people(before), people(after));
    if said.len() != cleaned.len() {
        return false;
    }
    let changes: BTreeSet<(u8, u8)> = said
        .into_iter()
        .zip(cleaned)
        .filter(|(a, b)| a != b)
        .collect();
    changes == BTreeSet::from([(1, 2), (2, 1)])
}

/// A run of unsaid output words that appears word for word in an example.
///
/// `examples` are the (transcript, cleanup) pairs the model was shown.
pub fn copied_example<S: AsRef<str>>(
    said: &str,
    cleaned: &str,
    examples: &[(S, S)],
) -> Option<Vec<String>> {
    let mut heard: BTreeSet<String> = tokens(said).into_iter().collect();
    let heard_parts: Vec<String> = heard
        .iter()
        .flat_map(|token| parts(token))
        .map(String::from)
        .collect();
    heard.extend(heard_parts);
    let shown: Vec<String> = examples
        .iter()
        .flat_map(|(said, cleaned)| [said.as_ref(), cleaned.as_ref()])
        .map(|text| format!(" {} ", tokens(text).join(" ")))
        .collect();
    let mut run: Vec<String> = Vec::new();
    let cleaned_tokens = tokens(cleaned);
    for token in cleaned_tokens.iter().map(Some).chain([None]) {
        if let Some(token) = token.filter(|token| !heard.contains(*token)) {
            run.push(token.clone());
            continue;
        }
        if run.len() >= COPIED_RUN {
            // Any COPIED_RUN-long stretch of the run is enough.
            for window in run.windows(COPIED_RUN) {
                let window = format!(" {} ", window.join(" "));
                if shown.iter().any(|text| text.contains(&window)) {
                    return Some(run);
                }
            }
        }
        run.clear();
    }
    None
}

/// One flag for a capture from its phrases' verdicts; `None` when all passed.
pub fn summarize_reviews(verdicts: &[Verdict]) -> Option<Summary> {
    let flagged: Vec<&Verdict> = verdicts.iter().filter(|v| v.outcome != "ok").collect();
    if flagged.is_empty() {
        return None;
    }
    Some(Summary {
        outcome: if flagged.iter().any(|v| v.outcome == "reject") {
            "reject"
        } else {
            "review"
        }
        .to_string(),
        added: sorted(
            flagged
                .iter()
                .flat_map(|v| v.added.iter().map(String::as_str)),
        ),
        missing: sorted(
            flagged
                .iter()
                .flat_map(|v| v.missing.iter().map(String::as_str)),
        ),
        reasons: sorted(
            flagged
                .iter()
                .filter_map(|v| v.reason.as_deref())
                .filter(|r| !r.is_empty()),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_lost_number() {
        let verdict = check("meet at 5 on Tuesday", "Meet on Tuesday.", false, "");
        assert_eq!(verdict.outcome, "reject");
        assert_eq!(verdict.reason.as_deref(), Some("number"));
        assert_eq!(
            check("Is this working?", "Is this working?", false, "").outcome,
            "ok"
        );
        assert_eq!(
            check("Is this working?", "Yes, it's working.", false, "").outcome,
            "reject"
        );
    }

    #[test]
    fn finds_a_copied_example() {
        let examples = [("list go to the store", "1. Go to the store")];
        let copied = copied_example("hi", "Hi go to the store", &examples).unwrap();
        assert_eq!(copied, ["go", "to", "the", "store"]);
        assert_eq!(
            copied_example("go to the store", "Go to the store", &examples),
            None
        );
    }
}
