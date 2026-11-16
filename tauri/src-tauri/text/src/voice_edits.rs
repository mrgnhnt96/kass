//! Edits said aloud: "fix that, Morgan not Megan" changes the text before the
//! caret.
//!
//! The backend's `voice_edits` module without its database parts
//! (`learn_from`, `report_fix`), which stay with the Dictionary and
//! corrections. Offsets in [`Word`] are byte offsets into the text they came
//! from.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::py::{self, Groups};
use crate::re;
use crate::spelling::{distance, is_spelled, shaped, sounds_like};

/// Words Whisper is told about so "Kass" is heard right.
pub const COMMAND_TERMS: &[&str] = &["Kass"];
pub const TRANSFORM_NAME: &str = "Voice edit";
pub const NOTHING_TO_FIX: &str = "Nothing to fix: Kass can't read the text before the cursor here";
pub const SAY_WHAT: &str = "Say what to fix after “fix that”";

const OPENING: &str = r"^[^\w]*(?:(?:um+|uh+|uhm|erm?|so|okay|ok|oh|hey|and)\b[^\w]*)*(?P<triggers>(?:(?:fix(?:\s+that)?|edit|[kc]a+s+)\b(?P<sep>[^\w]*)){1,2})";
/// A word, with "p.m.", "don't", "3:30" and unjoined spelling ("M-E-G") kept whole.
const WORD: &str = r"\w+(?:['’.\-]\w+|(?<=\d):\d\d\b)*";

/// Longest sides a spoken edit has: longer is a sentence being dictated.
const MAX_FIND: usize = 6;
const MAX_SIDE: usize = 4;

fn set(words: &[&'static str]) -> HashSet<&'static str> {
    words.iter().copied().collect()
}

static CHANGE: LazyLock<HashSet<&str>> =
    LazyLock::new(|| set(&["change", "replace", "swap", "switch"]));
static CHANGE_TO: LazyLock<HashSet<&str>> = LazyLock::new(|| set(&["to", "with", "into", "for"]));
static DELETE: LazyLock<HashSet<&str>> =
    LazyLock::new(|| set(&["delete", "remove", "drop", "cut", "erase", "strike"]));
/// Whisper once wrote "add" as "at".
static ADD: LazyLock<HashSet<&str>> = LazyLock::new(|| set(&["add", "insert", "put", "at"]));
static PLACES: LazyLock<HashSet<&str>> = LazyLock::new(|| set(&["after", "before"]));
/// "X is not Y" negates a verb: a sentence, not a correction.
static VERBS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    set(&[
        "am", "is", "are", "was", "were", "be", "do", "does", "did", "can", "could", "should",
        "would", "will", "has", "have", "had", "must", "may", "might",
    ])
});
/// Words around the spelled letters that aren't the word being fixed.
static SPELLING: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    set(&[
        "spelled", "spelt", "spell", "like", "as", "it's", "its", "that's", "is", "with", "a",
        "an", "the",
    ])
});
/// Kinds of word one said word replaces another of: "it's Tuesday" fixes
/// "Monday". Months and days count only capitalized in the text ("we may").
static DAYS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    set(&[
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "today",
        "tonight",
        "tomorrow",
        "yesterday",
    ])
});
static MONTHS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    set(&[
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ])
});
static NUMBER_WORDS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    set(&[
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
        "thirty",
        "forty",
        "fifty",
        "sixty",
        "seventy",
        "eighty",
        "ninety",
        "hundred",
        "noon",
        "midnight",
    ])
});
const LEAD_INS: &[&[&str]] = &[
    &["please"],
    &["it's"],
    &["it", "is"],
    &["it", "was"],
    &["it", "should", "be"],
    &["that's"],
    &["that", "is"],
    &["that", "should", "be"],
    &["should", "be"],
    &["i", "meant"],
    &["i", "mean"],
    &["i", "said"],
    &["make", "it"],
    &["the", "word"],
    &["the", "words"],
    &["word"],
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub key: String,
    /// Starts a sentence, where Whisper's capital letter says nothing.
    pub opens: bool,
    /// A comma (or other mark) follows it.
    pub marked: bool,
}

/// What to change: `find` in the take becomes `write` (empty deletes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instruction {
    pub find: Vec<Word>,
    pub write: Vec<Word>,
    /// `write` is one word spelled out letter by letter: its case follows the word it replaces.
    pub spelled: bool,
    /// "X, no, Y": whichever of the two the take has is the wrong one.
    pub either: bool,
    /// One word said alone: it replaces the word like it in the take.
    pub alike: bool,
    /// Add `write` "before" or "after" `find` instead of replacing it.
    pub place: Option<String>,
    pub said: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declined {
    pub message: String,
}

fn declined(message: impl Into<String>) -> Declined {
    Declined {
        message: message.into(),
    }
}

/// The take's text before and after the edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub before: String,
    pub after: String,
    pub instruction: String,
    /// The wrong word, and the letters spelled for it (spelled edits only).
    pub replaced: String,
    pub spelled: Option<String>,
}

fn key(text: &str) -> String {
    re!(r"[.\-]").replace(&py::casefold(&text.replace('’', "'")), "")
}

fn opens(text: &str, at: usize) -> bool {
    let before = py::rstrip(&text[..at]);
    before.is_empty() || py::in_chars(py::final_char(before), ".!?:\n")
}

/// Upper-cases (or lower-cases) the first character of `text`.
fn with_first(text: &str, upper: bool) -> String {
    let first = py::head(text, 1);
    let first = if upper {
        py::upper(first)
    } else {
        py::lower(first)
    };
    first + py::tail_from(text, 1)
}

fn first_upper(text: &str) -> bool {
    py::isupper(py::head(text, 1))
}

/// The words of `text`; a time said as "3 p.m." is one word, like "3pm".
pub fn words(text: &str) -> Vec<Word> {
    let mut merged: Vec<Word> = Vec::new();
    for found in re!(WORD).finditer(text) {
        let (start, end) = (found.start(), found.end());
        let word = Word {
            text: found.text().to_string(),
            start,
            end,
            key: key(found.text()),
            opens: opens(text, start),
            marked: !py::strip(py::head(&text[end..], 1)).is_empty(),
        };
        match merged.last_mut() {
            Some(last)
                if (word.key == "am" || word.key == "pm")
                    && re!(r"\d{1,2}(?::\d\d)?").fullmatch(&last.text).is_some()
                    && py::strip(&text[last.end..word.start]).is_empty() =>
            {
                *last = Word {
                    text: text[last.start..word.end].to_string(),
                    start: last.start,
                    end: word.end,
                    key: last.key.clone() + &word.key,
                    opens: last.opens,
                    marked: word.marked,
                };
            }
            _ => merged.push(word),
        }
    }
    merged
}

/// What follows an opening "fix that", "fix", "edit" or "Kass", whether a
/// mark (or the end) set it apart, and whether it was a bare "fix"; `None`
/// when the text doesn't open with one.
pub fn opening(text: &str) -> Option<(String, bool, bool)> {
    let found = re!(OPENING, py::I).match_(text)?;
    let rest = &text[found.end()..];
    let bare = re!(r"fix\W*", py::I)
        .fullmatch(found.n("triggers"))
        .is_some();
    let marked = !py::strip(found.n("sep")).is_empty() || py::strip(rest).is_empty();
    Some((rest.to_string(), marked, bare))
}

/// Whether `text` opens an edit: a trigger set apart by a mark ("Fix
/// that, ..."), or followed by words that read as one ("fix that add ...").
pub fn starts_edit(text: &str) -> bool {
    match opening(text) {
        None => false,
        Some((_, marked, _)) => marked || parse(text).is_some(),
    }
}

fn strip_lead_ins(mut found: &[Word]) -> &[Word] {
    let mut changed = true;
    while changed {
        changed = false;
        for lead in LEAD_INS {
            if found.len() > lead.len()
                && found[..lead.len()]
                    .iter()
                    .zip(lead.iter())
                    .all(|(w, k)| w.key == *k)
            {
                found = &found[lead.len()..];
                changed = true;
            }
        }
    }
    found
}

fn one_spelled(found: &[Word]) -> bool {
    found.len() == 1 && is_spelled(&found[0].text)
}

/// `side` with a word said and then spelled ("Megan, MEGHAN") as the
/// spelled word alone.
fn respelled(side: &[Word]) -> Vec<Word> {
    let mut kept: Vec<Word> = Vec::new();
    for word in side {
        match kept.last_mut() {
            Some(last) if is_spelled(&word.text) && sounds_like(&word.key, &last.key).is_some() => {
                *last = word.clone()
            }
            _ => kept.push(word.clone()),
        }
    }
    kept
}

fn index(found: &[Word], keys: &HashSet<&str>, last: bool) -> Option<usize> {
    let mut hits = (0..found.len())
        .filter(|&i| keys.contains(found[i].key.as_str()) && 0 < i && i + 1 < found.len());
    if last {
        hits.next_back()
    } else {
        hits.next()
    }
}

/// The edit `found` (the words after the trigger) says, by its general
/// forms; with `alone`, also by one word on its own.
fn instruction(found: &[Word], said: &str, alone: bool) -> Option<Instruction> {
    let head = found.first()?.key.as_str();
    let said = said.to_string();
    if CHANGE.contains(head) {
        if let Some(at) = index(found, &CHANGE_TO, false) {
            let (find, write) = (respelled(&found[1..at]), respelled(&found[at + 1..]));
            if find.len() <= MAX_FIND && write.len() <= MAX_FIND {
                let spelled = one_spelled(&write);
                return Some(Instruction {
                    find: strip_lead_ins(&find).to_vec(),
                    write,
                    spelled,
                    said,
                    ..Default::default()
                });
            }
            return None;
        }
    }
    if DELETE.contains(head) {
        let find = strip_lead_ins(&found[1..]);
        return (!find.is_empty() && find.len() <= MAX_FIND).then(|| Instruction {
            find: find.to_vec(),
            said,
            ..Default::default()
        });
    }
    if ADD.contains(head) {
        if let Some(at) = index(found, &PLACES, true) {
            let (write, find) = (&found[1..at], &found[at + 1..]);
            if write.len() <= MAX_FIND && find.len() <= MAX_SIDE {
                return Some(Instruction {
                    find: find.to_vec(),
                    write: write.to_vec(),
                    place: Some(found[at].key.clone()),
                    said,
                    ..Default::default()
                });
            }
            return None;
        }
    }
    let keys: Vec<&str> = found.iter().map(|w| w.key.as_str()).collect();
    if head == "not" {
        // "Not Megan, Morgan": the wrong word first, up to its comma.
        let comma = (1..found.len().saturating_sub(1)).find(|&i| found[i].marked)?;
        return either_side(&found[comma + 1..], &found[1..comma + 1], said, false);
    }
    for i in 1..found.len().saturating_sub(1) {
        if keys[i] == "instead" && keys[i + 1] == "of" {
            return either_side(&found[..i], &found[i + 2..], said, false);
        }
    }
    if let Some(at) = index(found, &set(&["not"]), false) {
        if VERBS.contains(keys[at - 1]) || keys[at - 1].ends_with("n't") {
            return None;
        }
        return either_side(&found[..at], &found[at + 1..], said, false);
    }
    if let Some(at) = index(found, &set(&["no"]), false) {
        return either_side(&found[..at], &found[at + 1..], said, true);
    }
    let last = &found[found.len() - 1];
    if is_spelled(&last.text) {
        let hint: Vec<Word> = found[..found.len() - 1]
            .iter()
            .filter(|w| !SPELLING.contains(w.key.as_str()))
            .cloned()
            .collect();
        if hint.len() <= 2 {
            return Some(Instruction {
                find: hint,
                write: vec![last.clone()],
                spelled: true,
                said,
                ..Default::default()
            });
        }
    }
    if alone && found.len() == 1 {
        return Some(Instruction {
            write: vec![found[0].clone()],
            alike: true,
            said,
            ..Default::default()
        });
    }
    None
}

fn either_side(right: &[Word], wrong: &[Word], said: String, either: bool) -> Option<Instruction> {
    let (right, wrong) = (respelled(strip_lead_ins(right)), respelled(wrong));
    if right.is_empty() || wrong.is_empty() || right.len() > MAX_SIDE || wrong.len() > MAX_SIDE {
        return None;
    }
    let spelled = one_spelled(&right);
    Some(Instruction {
        find: wrong,
        write: right,
        spelled,
        either,
        said,
        ..Default::default()
    })
}

/// The edit `said` asks for; `Err` when it is one that can't be done as
/// heard; `None` when `said` isn't an edit.
pub fn parse(said: &str) -> Option<Result<Instruction, Declined>> {
    let (rest, marked, bare) = opening(said)?;
    let offset = said.len() - rest.len();
    let told: Vec<Word> = words(said)
        .into_iter()
        .filter(|w| w.start >= offset)
        .collect();
    let stripped = strip_lead_ins(&told);
    let led = stripped.len() < told.len();
    if bare && !marked && !led {
        // "Fix the login bug": a sentence, whatever its words look like.
        return None;
    }
    // A word alone is an edit only where it can't be the start of a sentence.
    let Some(found) = instruction(stripped, said, marked || led) else {
        return (marked && py::strip_chars(&rest, " \t.,!?;:").is_empty())
            .then(|| Err(declined(SAY_WHAT)));
    };
    let same = found.find.len() == found.write.len()
        && found
            .find
            .iter()
            .zip(&found.write)
            .all(|(a, b)| a.key == b.key);
    if !found.spelled && !found.write.is_empty() && same {
        // Whisper hears names that sound alike as one ("Meghan" as "Megan").
        return Some(Err(declined(format!(
            "Heard “{}” twice. Try spelling it out",
            joined(&found.write, said)
        ))));
    }
    Some(Ok(found))
}

fn joined<'t>(found: &[Word], said: &'t str) -> &'t str {
    match (found.first(), found.last()) {
        (Some(first), Some(last)) => &said[first.start..last.end],
        _ => "",
    }
}

// -- matching in the text ------------------------------------------------------

type Span = Option<Result<(usize, usize), Declined>>;

/// How `heard` in the take matches `said`: exactly (0), but for case (1), or
/// spelled a letter off (2, words of five letters or more).
fn level(said: &Word, heard: &Word) -> Option<usize> {
    if said.text == heard.text {
        return Some(0);
    }
    if said.key == heard.key {
        return Some(1);
    }
    let (a, b) = (&said.key, &heard.key);
    if py::len(a) >= 5 && py::len(b) >= 5 && py::first(a) == py::first(b) && distance(a, b, 1) <= 1
    {
        return Some(2);
    }
    None
}

/// How close a spelled word is to `heard`: the same sounds (0) or a few off
/// (1, by how many; see `sounds_like`); `None` for the word it already is.
fn spelled_level(letters: &str, heard: &Word) -> Option<Vec<usize>> {
    if heard.key == letters {
        return None;
    }
    let off = sounds_like(letters, &heard.key)?;
    Some(if off == 0 { vec![0, 0] } else { vec![1, off] })
}

/// The span with the lowest rank; spans rank equal only when nothing tells them apart.
fn best(mut candidates: Vec<(Vec<usize>, usize, usize)>) -> Span {
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    if candidates.len() > 1 && candidates[0].0 == candidates[1].0 {
        return Some(Err(declined("Found it twice, so nothing changed")));
    }
    Some(Ok((candidates[0].1, candidates[0].2)))
}

/// Where `phrase` is in the take: the closest match, then the one nearest
/// the end (the caret).
fn find(phrase: &[Word], take: &str, found: &[Word]) -> Span {
    let n = phrase.len();
    if n == 0 || found.len() < n {
        return best(Vec::new());
    }
    let mut candidates = Vec::new();
    for i in 0..=found.len() - n {
        let levels: Option<Vec<usize>> = phrase
            .iter()
            .zip(&found[i..i + n])
            .map(|(s, h)| level(s, h))
            .collect();
        let Some(levels) = levels else { continue };
        let (start, end) = (found[i].start, found[i + n - 1].end);
        candidates.push((
            vec![levels.into_iter().max().unwrap(), take.len() - end],
            start,
            end,
        ));
    }
    best(candidates)
}

fn find_spelled(letters: &str, take: &str, found: &[Word]) -> Span {
    let mut candidates = Vec::new();
    for heard in found {
        if let Some(mut level) = spelled_level(letters, heard) {
            level.push(take.len() - heard.end);
            candidates.push((level, heard.start, heard.end));
        }
    }
    best(candidates)
}

/// What replaces `take[start..end]`, cased for where it goes.
fn written(instruction: &Instruction, take: &str, start: usize, end: usize) -> String {
    let target = &take[start..end];
    let Some(first) = instruction.write.first() else {
        return String::new();
    };
    if instruction.spelled {
        return shaped(&first.text.replace('-', ""), target);
    }
    let mut text = joined(&instruction.write, &instruction.said).to_string();
    // Whisper's capital says nothing where a sentence began, or where it
    // wrote the whole edit in lower case ("fix that morgan not megan").
    let uncased = first.opens || !instruction.said.chars().any(char::is_uppercase);
    if uncased && !(py::isupper(&first.text) && py::len(&first.text) > 1) && first.text != "I" {
        let upper = instruction.place.is_none() && first_upper(target);
        text = with_first(&text, upper);
    }
    if opens(take, start) && first_upper(target) && instruction.place.is_none() {
        text = with_first(&text, true);
    }
    let half_day = re!(r"\s*[ap]\.?m\.?$", py::I);
    if instruction.alike && !half_day.is_match(&text) {
        if let Some(half) = half_day.search(target) {
            // "It's 3:30" keeps the "p.m." of the time it fixes.
            if py::final_char(&text).is_some_and(py::is_digit) {
                text += half.text();
            } else {
                text = text + " " + py::lstrip(half.text());
            }
        }
    }
    text
}

/// `take` without `take[start..end]`, its comma and one space, and a
/// sentence it began starting at the next word.
fn delete(take: &str, start: usize, end: usize) -> String {
    let (mut before, mut after) = (&take[..start], &take[end..]);
    if after.starts_with(',') && (py::strip(before).is_empty() || before.ends_with(' ')) {
        after = &after[1..];
    }
    if before.ends_with(' ')
        && (after.starts_with(' ') || after.starts_with(['.', ',', '!', '?', ';', ':']))
    {
        before = &before[..before.len() - 1];
    } else if py::strip(before).is_empty() || before.ends_with('\n') {
        after = after.trim_start_matches(' ');
    }
    let mut after = after.to_string();
    if opens(take, start) && first_upper(&take[start..]) {
        let lead = after.len() - py::lstrip(&after).len();
        after = after[..lead].to_string() + &with_first(&after[lead..], true);
    }
    before.to_string() + &after
}

fn apply(instruction: &Instruction, take: &str, start: usize, end: usize) -> String {
    match instruction.place.as_deref() {
        Some("after") => format!(
            "{} {}{}",
            &take[..end],
            written(instruction, take, start, end),
            &take[end..]
        ),
        Some("before") => {
            let mut text = written(instruction, take, start, end);
            if opens(take, start) {
                text = with_first(&text, true);
            }
            format!("{}{} {}", &take[..start], text, &take[start..])
        }
        _ if instruction.write.is_empty() => delete(take, start, end),
        _ => format!(
            "{}{}{}",
            &take[..start],
            written(instruction, take, start, end),
            &take[end..]
        ),
    }
}

/// The kind of word `word` is, for one said alone to replace.
fn kind(word: &Word) -> Option<&'static str> {
    let key = word.key.as_str();
    if DAYS.contains(key) {
        Some("day")
    } else if MONTHS.contains(key) {
        Some("month")
    } else if NUMBER_WORDS.contains(key)
        || re!(r"\d+(?:[:,]\d+)*(?:am|pm|st|nd|rd|th)?")
            .fullmatch(key)
            .is_some()
    {
        Some("number")
    } else {
        None
    }
}

/// The word in the take that `said` replaces: the one of its kind nearest
/// the caret, or else the one that sounds most like it, when no other sounds
/// as close.
fn find_alike(said: &Word, take: &str, found: &[Word]) -> Span {
    if let Some(kind_of) = kind(said) {
        let candidates = found
            .iter()
            .filter(|heard| {
                kind(heard) == Some(kind_of)
                    && heard.key != said.key
                    && (kind_of == "number" || first_upper(&heard.text))
            })
            .map(|heard| (vec![take.len() - heard.end], heard.start, heard.end))
            .collect();
        return best(candidates);
    }
    if py::len(&said.key) < 3 || !said.key.chars().all(py::is_alpha) {
        return None;
    }
    let mut candidates: Vec<(Vec<usize>, usize, usize)> = found
        .iter()
        .filter_map(|heard| {
            spelled_level(&said.key, heard).map(|level| (level, heard.start, heard.end))
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    if candidates.len() > 1 && candidates[0].0 == candidates[1].0 {
        return Some(Err(declined(format!(
            "More than one word sounds like “{}”, so nothing changed",
            said.text
        ))));
    }
    candidates.first().map(|c| Ok((c.1, c.2)))
}

/// Where the edit goes in the take, and whether an "X, no, Y" edit found its
/// second side there (so that side is the wrong one).
fn target(
    instruction: &Instruction,
    take: &str,
    found: &[Word],
) -> Result<(usize, usize, bool), Declined> {
    let missing = || {
        declined(format!(
            "Couldn't find “{}” in the text before the cursor",
            joined(&instruction.find, &instruction.said)
        ))
    };
    if instruction.either {
        let sides = [
            find(&instruction.find, take, found),
            find(&instruction.write, take, found),
        ];
        if let Some(Some(Err(declined))) = sides.iter().find(|side| matches!(side, Some(Err(_)))) {
            return Err(declined.clone());
        }
        let hits: Vec<((usize, usize), bool)> = sides
            .iter()
            .enumerate()
            .filter_map(|(swap, side)| match side {
                Some(Ok(span)) => Some((*span, swap == 1)),
                _ => None,
            })
            .collect();
        return match hits.as_slice() {
            [((start, end), swap)] => Ok((*start, *end, *swap)),
            [] => Err(missing()),
            _ => Err(declined("Heard both words, so nothing changed")),
        };
    }
    let span = if instruction.alike {
        let said = &instruction.write[0];
        match find_alike(said, take, found) {
            Some(span) => span,
            None => {
                if found.iter().any(|heard| heard.key == said.key) {
                    return Err(declined(format!(
                        "“{}” is already written that way",
                        said.text
                    )));
                }
                return Err(declined(match kind(said) {
                    Some(like) => {
                        format!(
                            "No {} in the text before the cursor",
                            if like == "number" {
                                "number or time"
                            } else {
                                like
                            }
                        )
                    }
                    None => format!("Nothing like “{}” in the text before the cursor", said.text),
                }));
            }
        }
    } else if instruction.spelled {
        let letters = &instruction.write[0].key;
        let mut span = if instruction.find.is_empty() {
            None
        } else {
            find(&instruction.find, take, found)
        };
        if let Some(Ok((start, end))) = span {
            if py::casefold(&take[start..end]) == *letters {
                // The word said is already spelled that way: find the one that isn't.
                span = None;
            }
        }
        match span.or_else(|| find_spelled(letters, take, found)) {
            Some(span) => span,
            None => {
                return Err(declined(format!(
                    "Nothing close to “{}” in the text before the cursor",
                    shaped(letters, "Aa")
                )))
            }
        }
    } else {
        find(&instruction.find, take, found).unwrap_or_else(|| Err(missing()))
    };
    span.map(|(start, end)| (start, end, false))
}

/// The edit `said` makes to `take`, the text before the caret; `None` when
/// `said` isn't an edit.
pub fn plan(said: &str, take: Option<&str>) -> Option<Result<Planned, Declined>> {
    let mut instruction = match parse(said)? {
        Ok(instruction) => instruction,
        Err(declined) => return Some(Err(declined)),
    };
    let take = match take {
        Some(take) if !py::strip(take).is_empty() => take,
        _ => return Some(Err(declined(NOTHING_TO_FIX))),
    };
    let (start, end, swap) = match target(&instruction, take, &words(take)) {
        Ok(target) => target,
        Err(declined) => return Some(Err(declined)),
    };
    if instruction.place.is_some() && instruction.write.is_empty() {
        // "Add after X" says nothing to add (the backend raises here).
        return Some(Err(declined(SAY_WHAT)));
    }
    if swap {
        instruction = Instruction {
            find: instruction.write,
            write: instruction.find,
            spelled: instruction.spelled,
            said: said.to_string(),
            ..Default::default()
        };
    }
    let after = apply(&instruction, take, start, end);
    let replaced = &take[start..end];
    if after == take {
        return Some(Err(declined(format!(
            "“{replaced}” is already written that way"
        ))));
    }
    let written = written(&instruction, take, start, end);
    let described = match &instruction.place {
        Some(place) => format!("Add “{written}” {place} “{replaced}”"),
        None if written.is_empty() => format!("Delete “{replaced}”"),
        None => format!("“{replaced}” → “{written}”"),
    };
    let spelled = instruction
        .spelled
        .then(|| instruction.write[0].text.replace('-', ""));
    Some(Ok(Planned {
        before: take.to_string(),
        after,
        instruction: described,
        replaced: replaced.to_string(),
        spelled,
    }))
}

/// Whether the edit changes only the last `chars` characters of the text:
/// the part Kass's last take wrote.
pub fn changes_end(planned: &Planned, chars: usize) -> bool {
    let before: Vec<char> = planned.before.chars().collect();
    let after: Vec<char> = planned.after.chars().collect();
    let mut start = 0;
    while start < before.len().min(after.len()) && before[start] == after[start] {
        start += 1;
    }
    // Whole words: a change that starts mid-word starts at the word.
    while start > 0 && py::is_alnum(before[start - 1]) {
        start -= 1;
    }
    chars > 0 && start + chars >= before.len()
}

const CONTEXT: usize = 16;

/// `output` with the change that turned `before` into `after`.
///
/// `before` is the take as the field showed it, which may differ from the
/// capture's output at its edges (spacing, a dropped final period), so the
/// change is found by its own words and a little of the text around it, and
/// applied only where that occurs once.
pub fn corrected(output: &str, before: &str, after: &str) -> Option<String> {
    let before: Vec<char> = before.chars().collect();
    let after: Vec<char> = after.chars().collect();
    let shorter = before.len().min(after.len());
    let mut start = 0;
    while start < shorter && before[start] == after[start] {
        start += 1;
    }
    let mut end = 0;
    while end < shorter - start && before[before.len() - 1 - end] == after[after.len() - 1 - end] {
        end += 1;
    }
    // Whole words: widen the change to the words it touches.
    while start > 0 && py::is_alnum(before[start - 1]) {
        start -= 1;
    }
    while end > 0 && py::is_alnum(before[before.len() - end]) {
        end -= 1;
    }
    let text = |chars: &[char]| chars.iter().collect::<String>();
    let old = text(&before[start..before.len() - end]);
    let new = text(&after[start..after.len() - end]);
    let left = text(&before[start.saturating_sub(CONTEXT)..start]);
    let right = text(&before[before.len() - end..(before.len() - end + CONTEXT).min(before.len())]);
    let (left, right) = (left.as_str(), right.as_str());
    for (context_left, context_right) in [(left, right), (left, ""), ("", right), ("", "")] {
        let needle = format!("{context_left}{old}{context_right}");
        if !needle.is_empty() && py::count(output, &needle) == 1 {
            let at = output.find(&needle).unwrap() + context_left.len();
            return Some(format!(
                "{}{}{}",
                &output[..at],
                new,
                &output[at + old.len()..]
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(said: &str, take: &str) -> String {
        match plan(said, Some(take)) {
            Some(Ok(planned)) => planned.after,
            Some(Err(declined)) => declined.message,
            None => "not an edit".into(),
        }
    }

    #[test]
    fn plans_edits() {
        assert_eq!(
            planned("Fix that, Morgan not Megan", "Hi Megan."),
            "Hi Morgan."
        );
        assert_eq!(
            planned("Fix that, delete really", "It was really good."),
            "It was good."
        );
        assert_eq!(
            planned("Fix that, add very before good", "It was good."),
            "It was very good."
        );
        assert_eq!(planned("Fix the login bug", "Hi Megan."), "not an edit");
        assert_eq!(planned("Fix that, add after Megan", "Hi Megan."), SAY_WHAT);
    }

    #[test]
    fn merges_times_and_finds_the_change() {
        let found = words("at 3 p.m. today");
        assert_eq!(found[1].text, "3 p.m");
        assert_eq!(found[1].key, "3pm");
        assert_eq!(
            corrected("Thanks Megan.", " Thanks Megan", " Thanks Morgan").as_deref(),
            Some("Thanks Morgan.")
        );
    }
}
