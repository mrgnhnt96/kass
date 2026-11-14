//! Write a punctuation mark the speaker said as a word, and keep it.
//!
//! "I'm home comma see you soon period" is "I'm home, see you soon." Whisper's
//! own pauses around the word go with it ("home, comma, see" and "home. Comma."
//! are one comma). A mark the speaker said wins over every later choice: the
//! cleanup model, the learned writing style and the closing of a dictation may
//! punctuate the rest, but [`keep_spoken_punctuation`] puts a said mark back
//! where it was said. An exclamation mark Whisper heard in the speaker's voice
//! is kept the same way when cleanup flattens it to a period.
//!
//! A mark word that is talked about stays a word: "a comma", "the Oxford
//! comma" once the speaker corrects it, "a period of time". Corrections also
//! teach the speaker's own words for a mark ("bang" for "!"); see [`learn`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use crate::difflib::{self, SequenceMatcher, Tag};
use crate::py::{self, Groups, Re};
use crate::re;

pub(crate) const MARKS: &[(&str, &str)] = &[
    ("comma", ","),
    ("period", "."),
    ("full stop", "."),
    ("question mark", "?"),
    ("exclamation point", "!"),
    ("exclamation mark", "!"),
    ("colon", ":"),
    ("semicolon", ";"),
    ("semi colon", ";"),
    ("semi-colon", ";"),
];

pub(crate) fn mark_named(name: &str) -> Option<&'static str> {
    MARKS.iter().find(|(word, _)| *word == name).map(|(_, mark)| *mark)
}

const SENTENCE_END: &str = ".?!";
/// Before a mark word, these make it something talked about: "a comma", "the
/// period", "with an exclamation point".
const NAMED: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "each", "every", "another", "any", "some", "no", "our",
    "my", "your", "his", "her", "its", "their", "one",
];
/// Whisper's pause marks around a said mark, never a line break.
const PAUSE: &str = " \t,.;:!?";
pub(crate) const CLOSERS: &str = "\"')]}\u{201d}\u{2019}";
const OPENERS: &str = "\"'([{\u{201c}\u{2018}";

fn word_re() -> &'static Re {
    re!(r"[\w'’]+")
}

fn previous_word_re() -> &'static Re {
    re!(r"([\w'’]+)[^\w\n]*\Z")
}

fn next_word_re() -> &'static Re {
    re!(r"[ \t]*([\w'’]+)")
}

/// What the speaker taught: their own words for marks, and the mark words
/// that stay words after a given word ("trial period").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Learned {
    pub aliases: BTreeMap<String, String>,
    pub kept: BTreeSet<String>,
}

static PATTERNS: LazyLock<Mutex<HashMap<Vec<String>, Arc<Re>>>> = LazyLock::new(Default::default);

fn words_pattern(words: Vec<String>) -> Arc<Re> {
    let mut cache = PATTERNS.lock().unwrap();
    if let Some(found) = cache.get(&words) {
        return found.clone();
    }
    let mut sorted = words.clone();
    sorted.sort_by_key(|word| std::cmp::Reverse(py::len(word)));
    let mut alternatives = sorted
        .iter()
        .map(|word| {
            re!(r"[ -]")
                .split(word)
                .iter()
                .map(|part| py::escape(part))
                .collect::<Vec<_>>()
                .join(r"[ \t-]?")
        })
        .collect::<Vec<_>>()
        .join("|");
    // Last, so a mark word said right wins: any word before "point" or
    // "mark", which `mark_for` keeps only when it sounds like a mark's name.
    alternatives.push_str(r"|[^\W\d_]{4,}[ \t-]+(?:point|mark)");
    let pattern = Arc::new(Re::i(&format!(r"(?<![\w'’-])(?:{alternatives})(?![\w'’-])")));
    if cache.len() >= 8 {
        cache.clear();
    }
    cache.insert(words, pattern.clone());
    pattern
}

pub(crate) fn key(word: &str) -> String {
    py::casefold(word).replace('\u{2019}', "'")
}

fn spaced(said: &str) -> String {
    re!(r"[ \t-]+").replace(&key(said), " ")
}

/// How close a heard word must be to a mark's name: "explanation point" and
/// "exclamatory mark" are "!", "presentation point" is words.
const SOUNDS_LIKE: f64 = 0.8;

fn mark_for(said: &str, aliases: &BTreeMap<String, String>) -> Option<String> {
    let word = spaced(said);
    if let Some(found) = mark_named(&word) {
        return Some(found.to_string());
    }
    if let Some(found) = aliases.get(&word).filter(|mark| !mark.is_empty()) {
        return Some(found.clone());
    }
    // Whisper mishears the long names: "explanation point" was "exclamation point".
    let (heard, noun) = word.rsplit_once(' ').unwrap_or(("", &word));
    for (name, mark) in MARKS {
        let (first, last) = name.rsplit_once(' ').unwrap_or(("", name));
        if !first.is_empty() && last == noun && difflib::ratio(heard, first) >= SOUNDS_LIKE {
            return Some(mark.to_string());
        }
    }
    None
}

/// `text` with said marks written, and the byte offset of each written mark.
fn convert(text: &str, learned: &Learned) -> (String, Vec<usize>) {
    let mut words: Vec<String> = MARKS.iter().map(|(word, _)| word.to_string()).collect();
    words.extend(learned.aliases.keys().cloned());
    let pattern = words_pattern(words);
    let (mut out, mut last, mut placed) = (String::new(), 0usize, Vec::new());
    for found in pattern.finditer(text) {
        if found.start() < last {
            continue;
        }
        let mark = mark_for(found.text(), &learned.aliases);
        // Said at the start of a line or of the text, it has nothing to follow.
        let previous = previous_word_re().search(&text[..found.start()]);
        let following = next_word_re().match_at(text, found.end());
        let Some(mark) = mark else { continue };
        let Some(previous) = previous else { continue };
        let before = key(previous.g(1));
        if NAMED.contains(&before.as_str())
            || learned.kept.contains(&format!("{before} {}", spaced(found.text())))
            // "a period of time", "the colon of"
            || following.is_some_and(|following| key(following.g(1)) == "of")
        {
            continue;
        }
        out.push_str(&text[last..found.start()]);
        out.truncate(py::rstrip_chars(&out, PAUSE).len());
        placed.push(out.len());
        out.push_str(&mark);
        last = found.end();
        let rest = &text[last..];
        last += rest.len() - py::lstrip_chars(rest, PAUSE).len();
        let rest = &text[last..];
        // A space only before words: "http colon//" stays joined.
        let Some(next) = py::first(rest) else { continue };
        if !(py::is_alnum(next) || OPENERS.contains(next)) {
            continue;
        }
        if mark == ":" && py::is_digit(next) && py::last(&out, 2).chars().next().is_some_and(py::is_digit) {
            // "3 colon 30" is a time.
            continue;
        }
        out.push(' ');
        if SENTENCE_END.contains(mark.as_str()) && py::first(py::lstrip(text)).is_some_and(|c| py::isupper(&c.to_string())) {
            out.push_str(&next.to_uppercase().to_string());
            last += next.len_utf8();
        }
    }
    out.push_str(&text[last..]);
    (out, placed)
}

/// Write each punctuation mark said as a word ("comma", "period", "bang").
pub fn apply_spoken_punctuation(text: &str, learned: Option<&Learned>) -> String {
    convert(text, learned.unwrap_or(&Learned::default())).0
}

struct Anchor {
    word: String,
    occurrence: usize,
    mark: char,
    spoken: bool,
    said_next: Option<String>,
}

/// Where `said` puts a mark that must stay: after which word, which time.
/// An exclamation mark Whisper wrote is one too, but not `spoken`.
fn anchors(said: &str, learned: &Learned) -> Vec<Anchor> {
    let (converted, placed) = convert(said, learned);
    let mut spots: BTreeMap<usize, bool> = placed.into_iter().map(|index| (index, true)).collect();
    for found in re!(r"(?<=[\w'’])!").finditer(&converted) {
        spots.entry(found.start()).or_insert(false);
    }
    let mut anchors = Vec::new();
    for (index, spoken) in spots {
        let words = word_re().findall(&converted[..index]);
        let Some(last) = words.last() else { continue };
        let word = key(last);
        let following = next_word_re().match_at(&converted, index + 1);
        anchors.push(Anchor {
            occurrence: words.iter().filter(|w| key(w) == word).count(),
            word,
            mark: converted[index..].chars().next().unwrap(),
            spoken,
            said_next: following.map(|f| f.g(1).to_string()),
        });
    }
    anchors
}

fn after_re() -> &'static Re {
    re!(format!(r"([,.;:!?]*)([{}]*)([,.;:!?]*)", py::escape(CLOSERS)))
}

/// Put back each mark `said` asked for that `text` changed or dropped.
///
/// A mark goes back after the same word, counted from the start; a word the
/// cleanup kept a different number of times is left alone. Whisper's
/// exclamation marks only replace the period cleanup flattened them to.
pub fn keep_spoken_punctuation(said: &str, text: &str, learned: Option<&Learned>) -> String {
    let empty = Learned::default();
    let learned = learned.unwrap_or(&empty);
    let anchors = anchors(said, learned);
    if anchors.is_empty() {
        return text.to_string();
    }
    let mut said_counts: HashMap<String, usize> = HashMap::new();
    for word in word_re().findall(&apply_spoken_punctuation(said, Some(learned))) {
        *said_counts.entry(key(word)).or_default() += 1;
    }
    let mut found: HashMap<String, Vec<usize>> = HashMap::new();
    for word in word_re().finditer(text) {
        found.entry(key(word.text())).or_default().push(word.end());
    }
    let capitals = py::first(py::lstrip(text)).is_some_and(|c| py::isupper(&c.to_string()));
    let mut edits = Vec::new();
    for anchor in anchors {
        let ends = found.get(&anchor.word).map_or(&[][..], Vec::as_slice);
        if ends.len() != said_counts.get(&anchor.word).copied().unwrap_or(0) || anchor.occurrence > ends.len() {
            continue;
        }
        let end = ends[anchor.occurrence - 1];
        let after = after_re().match_at(text, end).unwrap();
        let marks = format!("{}{}", after.g(1), after.g(3));
        if marks.contains(anchor.mark) {
            continue;
        }
        if !anchor.spoken && marks != "." {
            continue;
        }
        let (start, stop) = if !after.g(3).is_empty() {
            (after.start_of(3), after.end_of(3))
        } else if !after.g(1).is_empty() {
            (after.start_of(1), after.end_of(1))
        } else {
            (after.end(), after.end())
        };
        edits.push((start, stop, anchor.mark, text[start..stop].to_string(), anchor.said_next));
    }
    edits.sort_by(|a, b| b.cmp(a));
    let mut text = text.to_string();
    for (start, stop, mark, replaced, said_next) in edits {
        let mut rest = text[stop..].to_string();
        if py::first(&rest).is_some_and(py::is_alnum) {
            rest = format!(" {rest}");
        }
        if let Some(following) = next_word_re().match_(&rest) {
            let mut word = following.g(1).to_string();
            let ended = replaced.chars().any(|m| SENTENCE_END.contains(m));
            let ends = SENTENCE_END.contains(mark);
            if ends && !ended && capitals {
                word = upper_first(&word);
            } else if !ends && ended && said_next.as_deref().is_some_and(|next| key(&word) == key(next)) {
                // Lowered back only when the speaker's word was lower case.
                let next = said_next.unwrap();
                word = format!("{}{}", py::head(&next, 1), py::tail_from(&word, 1));
            }
            rest = format!("{}{word}{}", &rest[..following.start_of(1)], &rest[following.end_of(1)..]);
        }
        text = format!("{}{mark}{rest}", &text[..start]);
    }
    text
}

/// `word[:1].upper() + word[1:]`.
pub(crate) fn upper_first(word: &str) -> String {
    format!("{}{}", py::upper(py::head(word, 1)), py::tail_from(word, 1))
}

/// A period that ends a quote or parenthesis goes after it: "it's done". and
/// (see above). An ellipsis, "?" and "!" stay inside. An abbreviation keeps
/// its own period too: (and so on, etc.).
fn period_in_closers_re() -> &'static Re {
    re!(format!(r"(\w+(?:\.\w+)*)\.([{}]+)\.?(?=\s|$)", py::escape(CLOSERS)))
}

/// The writing style's abbreviations, which keep their own period.
pub(crate) const STYLE_ABBREVIATIONS: &[&str] = &["mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "approx"];

fn is_abbreviation(word: &str) -> bool {
    STYLE_ABBREVIATIONS.contains(&py::casefold(word).as_str())
        || re!(r"(?:[^\W\d_]\.)+[^\W\d_]").fullmatch(word).is_some()
}

/// `text` with each period written inside closing quotes or parentheses moved after them.
pub fn period_after_closers(text: &str) -> String {
    period_in_closers_re().sub(text, |found| {
        let word = found.g(1);
        let period = if is_abbreviation(word) { "." } else { "" };
        format!("{word}{period}{}.", found.g(2))
    })
}

// --- Learning from corrections ---------------------------------------------

/// One correction: what was written and what the speaker wanted.
#[derive(Clone, Debug)]
pub struct Example {
    pub original: String,
    pub expected: String,
    pub capture_id: String,
    pub source: String,
}

const MARK_CHARS: &str = ",.;:!?";

fn tokens(text: &str) -> Vec<String> {
    re!(r"[\w'’]+|[^\w\s]").findall(text).into_iter().map(key).collect()
}

fn is_word(token: &str) -> bool {
    word_re().fullmatch(token).is_some()
}

fn is_mark(token: &str) -> bool {
    py::len(token) == 1 && MARK_CHARS.contains(token)
}

enum Observation {
    Alias { words: String, mark: String, anchor: String },
    Keep { words: String, anchor: String },
}

/// What one correction says about said marks.
fn observations(original: &str, expected: &str) -> Vec<Observation> {
    let (old, new) = (tokens(original), tokens(expected));
    let mut found = Vec::new();
    for code in SequenceMatcher::new(&old, &new, false).get_opcodes() {
        let (a, b, c, d) = (code.i1, code.i2, code.j1, code.j2);
        if code.tag != Tag::Replace || a == 0 || !is_word(&old[a - 1]) {
            continue;
        }
        let (removed, added) = (&old[a..b], &new[c..d]);
        let removed_words: Vec<&String> = removed.iter().filter(|t| is_word(t)).collect();
        let added_words: Vec<&String> = added.iter().filter(|t| is_word(t)).collect();
        let added_marks: Vec<&String> = added.iter().filter(|t| is_mark(t)).collect();
        let removed_marks: Vec<&String> = removed.iter().filter(|t| is_mark(t)).collect();
        let anchor = old[a - 1].clone();
        if (1..=2).contains(&removed_words.len())
            && added_words.is_empty()
            && added_marks.len() == 1
            && removed.iter().filter(|t| !is_word(t)).all(|t| is_mark(t))
            && !removed_words.iter().any(|word| word.chars().any(py::is_digit))
        {
            found.push(Observation::Alias {
                words: join(&removed_words),
                mark: added_marks[0].clone(),
                anchor,
            });
        } else if (1..=2).contains(&added_words.len())
            && removed_words.is_empty()
            && removed_marks.len() == 1
            && mark_named(&join(&added_words)) == Some(removed_marks[0].as_str())
        {
            found.push(Observation::Keep { words: join(&added_words), anchor });
        }
    }
    found
}

fn join(words: &[&String]) -> String {
    words.iter().map(|w| w.as_str()).collect::<Vec<_>>().join(" ")
}

/// Learn the speaker's own words for marks, and where a mark word is a word.
///
/// A new word for a mark needs two different dictations that taught it, one
/// of them an explicit correction (a redictation only backs it up), and no
/// correction that kept the word as a word. A built-in mark word goes back to
/// being a word after the word before it ("trial period") once an explicit
/// correction says so, unless more corrections there wanted the mark.
pub fn learn(examples: &[Example]) -> Learned {
    let mut taught: BTreeMap<String, HashSet<&str>> = BTreeMap::new();
    let mut explicit: HashMap<String, bool> = HashMap::new();
    let mut marks: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut keeps: BTreeMap<String, HashSet<&str>> = BTreeMap::new();
    let mut converts: HashMap<String, HashSet<&str>> = HashMap::new();
    for example in examples {
        for observation in observations(&example.original, &example.expected) {
            match observation {
                Observation::Alias { words, mark, anchor } => {
                    if mark_named(&words).is_some() {
                        converts.entry(format!("{anchor} {words}")).or_default().insert(&example.capture_id);
                        continue;
                    }
                    taught.entry(words.clone()).or_default().insert(&example.capture_id);
                    marks.entry(words.clone()).or_default().insert(mark);
                    *explicit.entry(words).or_default() |= example.source != "redictation";
                }
                Observation::Keep { words, anchor } => {
                    if example.source != "redictation" {
                        keeps.entry(format!("{anchor} {words}")).or_default().insert(&example.capture_id);
                    }
                }
            }
        }
    }
    let mut written: HashMap<&str, usize> = HashMap::new();
    for example in examples {
        let words: Vec<String> = tokens(&example.expected).into_iter().filter(|t| is_word(t)).collect();
        let text = format!(" {} ", words.join(" "));
        for words in taught.keys() {
            *written.entry(words).or_default() += usize::from(text.contains(&format!(" {words} ")));
        }
    }
    let aliases = taught
        .iter()
        .filter(|(words, captures)| {
            captures.len() >= 2
                && explicit.get(*words).copied().unwrap_or(false)
                && marks[*words].len() == 1
                && written.get(words.as_str()).copied().unwrap_or(0) == 0
        })
        .map(|(words, _)| (words.clone(), marks[words].iter().next().unwrap().clone()))
        .collect();
    let kept = keeps
        .into_iter()
        .filter(|(context, captures)| captures.len() > converts.get(context).map_or(0, HashSet::len))
        .map(|(context, _)| context)
        .collect();
    Learned { aliases, kept }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_said_marks() {
        assert_eq!(
            apply_spoken_punctuation("I'm home comma see you soon period", None),
            "I'm home, see you soon."
        );
        assert_eq!(apply_spoken_punctuation("a period of time", None), "a period of time");
    }

    #[test]
    fn moves_a_period_after_closers() {
        assert_eq!(period_after_closers("he said \"it's done.\" ok"), "he said \"it's done\". ok");
    }
}
