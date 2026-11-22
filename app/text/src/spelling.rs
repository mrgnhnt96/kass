//! How close spelled letters are to a word, and the case they take.
//!
//! Also puts spelled-out text back together. Whisper writes spelled letters
//! with dashes between them ("m-r-g-n") and keeps "capital" as a word
//! ("capital C"). Said in one run with numbers and spoken punctuation, as in a
//! password or a username, the pieces also come out separated by spaces
//! ("capital C-H-E-N-E-Y 0021!"). [`join_spelling`] joins them into the one
//! string the speaker spelled ("Cheney0021!").
//!
//! Ported from the backend's `spelling` module (and `dictionary._sound`).

use std::sync::LazyLock;

use crate::py::{self, Groups, Re};
use crate::re;

const LETTER: &str = r"[^\W\d_]";

/// Marks the text spelled letters became, so only runs that start from
/// spelling are joined: "CHENEY 0021" was spelled, "iPhone 15" was not.
const SPELLED: char = '\u{e000}';

const SYMBOLS: &[(&str, &str)] = &[
    ("underscore", "_"),
    ("dash", "-"),
    ("hyphen", "-"),
    ("exclamation point", "!"),
    ("exclamation mark", "!"),
    ("question mark", "?"),
    ("at sign", "@"),
    ("dollar sign", "$"),
    ("percent sign", "%"),
    ("pound sign", "#"),
    ("hash", "#"),
    ("hashtag", "#"),
    ("ampersand", "&"),
    ("asterisk", "*"),
    ("period", "."),
    ("dot", "."),
    ("slash", "/"),
    ("plus sign", "+"),
];

/// Every symbol word, longest first, as one alternation.
static SYMBOL_WORD: LazyLock<String> = LazyLock::new(|| {
    let mut words: Vec<&str> = SYMBOLS.iter().map(|(word, _)| *word).collect();
    words.sort_by_key(|word| std::cmp::Reverse(word.len()));
    words
        .iter()
        .map(|word| py::escape(word))
        .collect::<Vec<_>>()
        .join("|")
});

fn symbol_words() -> &'static Re {
    re!(format!(r"\b(?:{})\b", *SYMBOL_WORD), py::I)
}

/// Three or more single letters joined by dashes. Two letters stay dashed,
/// since "A-B testing" and "T-shirt"-style words are written that way on
/// purpose.
fn spelled_letters() -> &'static Re {
    re!(format!(
        r"(?<![\w-]){SPELLED}?{LETTER}(?:-{LETTER}){{2,}}(?![\w-])"
    ))
}

/// "capital" said before a letter becomes that letter in upper case:
/// "capital c" is "C". Before spelled-out letters it is the first one only,
/// the way a name is spelled: "capital C-H-E-N-E-Y" is "Cheney". Before a
/// word it's left alone, and so is a letter that is itself a word going on
/// into more words: "the capital I visited" is the city, not the letter. A
/// symbol word after the letter ("capital A dash") means the speaker is still
/// spelling. "with a capital L" asks for the case of a word already said, so
/// it is kept for `apply_spoken_case`.
fn capital_letter() -> &'static Re {
    re!(format!(
        r"(?<![Ww]ith a )(?<![Ww]ith an )(?<![Ww]ith )\b[Cc]apital\s+(?![AaIiOo]\s+(?!(?i:{})\b){LETTER})({LETTER})((?:-{LETTER})*)(?![\w-])",
        *SYMBOL_WORD
    ))
}

/// One piece of a spelled run: spelled letters, a number, or a symbol said as
/// a word or written as itself, with any punctuation Whisper stuck on.
/// Sentence punctuation ends the run, so "C-A-T, 3 dogs" doesn't become
/// "CAT,3 dogs".
fn run() -> &'static Re {
    re!({
        let piece = format!(
            r"(?:{SPELLED}{LETTER}+|\d+|(?i:{})\b|[^\w\s{SPELLED}])",
            *SYMBOL_WORD
        );
        format!(
            r"(?<![\w{SPELLED}])(?:{piece}[^\w\s,;:.?!{SPELLED}]*[ \t]+)+{piece}[^\w\s{SPELLED}]*"
        )
    })
}

fn join_run(found: &fancy_regex::Captures) -> String {
    let run = found.text();
    if !run.contains(SPELLED) {
        return run.to_string();
    }
    let run = symbol_words().sub(run, |word| {
        let word = py::lower(word.text());
        SYMBOLS
            .iter()
            .find(|(name, _)| *name == word)
            .map_or(word.clone(), |(_, symbol)| symbol.to_string())
    });
    re!(r"[ \t]+").replace(&run, "")
}

/// Join spelled letters, and the numbers and punctuation said with them.
pub fn join_spelling(text: &str) -> String {
    let marked = capital_letter().sub(text, |found| {
        format!(
            "{SPELLED}{}{}",
            py::upper(found.g(1)),
            py::lower(found.g(2))
        )
    });
    let marked = spelled_letters().sub(&marked, |found| {
        let letters = found.text().replace('-', "");
        format!("{SPELLED}{}", letters.trim_start_matches(SPELLED))
    });
    if !marked.contains(SPELLED) {
        return text.to_string();
    }
    run().sub(&marked, join_run).replace(SPELLED, "")
}

/// The digraphs and letters spelled differently that sound the same.
const SOUNDS: &[(&str, &str)] = &[
    ("ph", "f"),
    ("ck", "k"),
    ("q", "k"),
    ("c", "k"),
    ("z", "s"),
    ("y", "i"),
];

/// `text`'s letters as they sound: no spaces or marks, one of each doubled letter.
///
/// Digits are kept as they are: "7" and "77" are different words.
pub fn sound(text: &str) -> String {
    let mut key = re!(r"[\W_]+").replace(&py::casefold(text), "");
    for (spelled, sounds) in SOUNDS {
        key = key.replace(spelled, sounds);
    }
    re!(r"([^\W\d_])\1+").sub(&key, |found| found.g(1).to_string())
}

/// Edit distance with swapped neighbours as one edit, or `limit + 1` past it.
pub fn distance(a: &str, b: &str, limit: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > limit {
        return limit + 1;
    }
    let mut before: Vec<usize> = Vec::new();
    let mut previous: Vec<usize>;
    let mut current: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        previous = std::mem::replace(&mut current, vec![0; b.len() + 1]);
        current[0] = i;
        for j in 1..=b.len() {
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(before[j - 2] + 1);
            }
        }
        if current.iter().min().copied().unwrap_or(0) > limit {
            return limit + 1;
        }
        before = previous;
    }
    *current.last().unwrap()
}

/// How many sounds spelled `letters` are off `word`, when few enough for
/// `word` to be them misheard: the first the same, and two in five of the
/// longer of the two ("VOVA" for "vulva", "MEGHAN" for "Megan"); `None`
/// when `word` is another word.
pub fn sounds_like(letters: &str, word: &str) -> Option<usize> {
    let (a, b) = (sound(letters), sound(word));
    if a.is_empty() || b.is_empty() || py::first(&a) != py::first(&b) {
        return None;
    }
    let limit = 2 * py::len(&a).max(py::len(&b)) / 5;
    let off = distance(&a, &b, limit);
    (off <= limit).then_some(off)
}

/// Spelled letters in the case of the word they replace: they carry none.
pub fn shaped(letters: &str, like: &str) -> String {
    if py::isupper(like) && py::len(like) > 1 {
        return py::upper(letters);
    }
    if py::isupper(py::head(like, 1)) {
        return py::upper(py::head(letters, 1)) + &py::lower(py::tail_from(letters, 1));
    }
    py::lower(letters)
}

/// Whether `text` reads as letters spelled aloud: three or more joined in
/// capitals ("MEGHAN"), or two Whisper left dashed ("B-O").
pub fn is_spelled(text: &str) -> bool {
    let letters = text.replace('-', "");
    if letters.is_empty() || !letters.chars().all(py::is_alpha) || !py::isupper(&letters) {
        return false;
    }
    let count = py::len(&letters);
    count >= 3 || (count == 2 && text.contains('-'))
}

/// A word, then the same word spelled: joined letters, which Whisper writes
/// in capitals ("Megan, MEGHAN"), or two letters it leaves dashed ("Bo, B-O").
fn respelled() -> &'static Re {
    re!(format!(
        r"(?<![\w'\u2019-])(?P<word>{LETTER}+),?[ \t]+(?:(?i:spelled|spelt)[ \t]+)?(?P<letters>{LETTER}{{2,}}|{LETTER}-{LETTER})(?![\w'\u2019-])"
    ))
}

fn respelled_word(word: &str, spelled: &str) -> Option<String> {
    let letters = spelled.replace('-', "");
    if !is_spelled(spelled) || sounds_like(&letters, word).is_none() {
        return None;
    }
    Some(if py::casefold(word) == py::casefold(&letters) {
        word.to_string()
    } else {
        shaped(&letters, word)
    })
}

/// A word said and then spelled is that word, as spelled: the spelling
/// corrects how Whisper heard it ("Megan, M-E-G-H-A-N" is "Meghan"). Run
/// after [`join_spelling`].
pub fn respell(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let (mut done, mut at) = (0, 0);
    while let Some(found) = respelled().search_from(text, at) {
        let Some(written) = respelled_word(found.n("word"), found.n("letters")) else {
            // The word after may be one said and then spelled itself.
            at = found.name("letters").unwrap().start();
            continue;
        };
        kept.push_str(&text[done..found.start()]);
        kept.push_str(&written);
        done = found.end();
        at = done;
    }
    kept.push_str(&text[done..]);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_sounds() {
        assert_eq!(sound("Phillip"), "filip");
        assert_eq!(distance("abcd", "abdc", 2), 1);
        assert_eq!(sounds_like("MEGHAN", "Megan"), Some(1));
        assert_eq!(sounds_like("VOVA", "vulva"), Some(2));
        assert_eq!(sounds_like("MEGHAN", "Tuesday"), None);
        assert_eq!(shaped("meghan", "Megan"), "Meghan");
        assert!(
            is_spelled("MEGHAN") && is_spelled("B-O") && !is_spelled("BO") && !is_spelled("Megan")
        );
    }

    #[test]
    fn joins_spelled_text() {
        assert_eq!(join_spelling("capital C-H-E-N-E-Y 0021!"), "Cheney0021!");
        assert_eq!(join_spelling("m-r-g-n at sign k-a-s-s"), "mrgn@kass");
        assert_eq!(join_spelling("iPhone 15"), "iPhone 15");
        assert_eq!(
            join_spelling("the capital I visited"),
            "the capital I visited"
        );
    }

    #[test]
    fn respells_a_word_said_then_spelled() {
        assert_eq!(respell("Megan, MEGHAN is here"), "Meghan is here");
        assert_eq!(respell("I said Bo, B-O"), "I said Bo");
        assert_eq!(respell("Tuesday, NASA"), "Tuesday, NASA");
    }
}
