//! Join dictation phrases that were cut at pauses.
//!
//! Streaming dictation recognizes and refines each phrase on its own, so
//! every phrase would otherwise come back punctuated as a finished sentence.
//! A pause is a breath, not a sentence end: phrases stay open while dictation
//! continues and the seam between two phrases is decided once the next one
//! arrives.
//!
//! Whisper is given the earlier text when it recognizes a phrase, so the
//! casing of its first word says whether the speaker continued the sentence.
//! A lowercase first word reliably continues the sentence; a capital does not
//! reliably start one. [`continue_phrase`] lowercases it only for a common
//! word ([`crate::lexicon::common_word`]) or a sentence opener. Names stay
//! capitalized, including any the user capitalizes mid-sentence elsewhere.

use std::collections::BTreeSet;

use crate::lexicon::common_word;
use crate::py::{self, Groups, Re};
use crate::re;

const SENTENCE_OPENERS: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "there", "here", "it", "its", "i", "we", "you", "he", "she",
    "they", "my", "our", "your", "his", "her", "their", "and", "but", "so", "or", "yet", "also", "then", "now",
    "well", "okay", "ok", "yeah", "yes", "no", "oh", "anyway", "plus", "still", "though", "although", "because",
    "since", "if", "when", "while", "after", "before", "once", "until", "unless", "as", "maybe", "perhaps",
    "actually", "honestly", "basically", "hopefully", "apparently", "otherwise", "meanwhile", "instead", "besides",
    "however", "what", "why", "how", "who", "whom", "whose", "where", "which", "is", "are", "was", "were", "am",
    "be", "been", "do", "does", "did", "don", "doesn", "didn", "have", "has", "had", "haven", "hasn", "can",
    "can't", "cannot", "could", "couldn", "will", "won", "would", "wouldn", "should", "shouldn", "shall", "may",
    "might", "must", "let", "let's", "not", "just", "only", "even", "please", "thanks", "thank", "sure", "all",
    "some", "any", "every", "each", "both", "either", "neither", "one", "two", "first", "next", "last", "lastly",
    "finally",
];

const TERMINAL: &str = ".?!:;";

/// A set of names the speaker uses, as passed to the seam functions.
pub type Names = BTreeSet<String>;

fn opener(key: &str) -> bool {
    SENTENCE_OPENERS.contains(&key) || SENTENCE_OPENERS.contains(&key.split('\'').next().unwrap_or(""))
}

fn key(word: &str) -> String {
    py::casefold(word).replace('\u{2019}', "'")
}

fn starts_upper(word: &str) -> bool {
    py::first(word).is_some_and(|c| py::isupper(&c.to_string()))
}

fn starts_lower(word: &str) -> bool {
    py::first(word).is_some_and(|c| py::islower(&c.to_string()))
}

pub(crate) fn first_word(text: &str) -> &str {
    re!(r"\W*([^\W\d_][\w']*)").match_(text).map_or("", |found| found.g(1))
}

fn starts_sentence(raw: &str) -> bool {
    let word = first_word(raw);
    if !starts_upper(word) {
        return false;
    }
    opener(&key(word))
}

/// "I" and its contractions, acronyms, and words like "GitHub" or "iPhone".
fn keeps_capital(word: &str) -> bool {
    word == "I"
        || word.starts_with("I'")
        || word.starts_with("I\u{2019}")
        || py::tail_from(word, 1).chars().any(|c| py::isupper(&c.to_string()))
}

pub(crate) fn lower_first(text: &str) -> String {
    let word = first_word(text);
    if word.is_empty() || keeps_capital(word) {
        return text.to_string();
    }
    let index = text.find(word).unwrap();
    let first = py::first(word).unwrap();
    format!("{}{}{}", &text[..index], py::lower(&first.to_string()), &text[index + first.len_utf8()..])
}

/// Drop end punctuation the phrase got only because the audio paused.
///
/// A closing period always goes; a question or exclamation mark goes only
/// when Whisper, which heard the earlier text, didn't end the phrase with it.
/// A phrase that ends with a spoken line break isn't open: it stays as it is.
pub fn open_phrase(text: &str, raw: &str) -> String {
    if text[py::rstrip(text).len()..].contains('\n') {
        return text.to_string();
    }
    let text = py::rstrip(text);
    let heard = py::final_char(py::rstrip(raw));
    if re!(r#"[\w)"'”][?!]$"#).is_match(text) && py::final_char(text) != heard {
        return py::drop_last(text, 1).to_string();
    }
    re!(r#"(?<=[\w)"'”])\.$"#).replace(text, "")
}

/// Drop the ending Whisper gives a phrase only because the audio paused.
///
/// Cut off at a pause, Whisper ends the phrase as if it were finished: a
/// period, a trailing dash ("if I pause-"), or dots ("How..."). None of that
/// was said. A question or exclamation mark stays until the next phrase shows
/// whether the sentence went on ([`continue_after_seam`]). So does the period
/// of an abbreviation ("U.S."), which isn't an ending.
pub fn strip_pause_mark(text: &str) -> String {
    let stripped = py::rstrip(text);
    let last = py::split(stripped).last().copied().unwrap_or("");
    if re!(r#"[\w)"'”](?:\.{2,}|…|[-–—]+)$"#).is_match(stripped) {
        return re!(r"(?:\.{2,}|…|[-–—]+)$").replace(stripped, "");
    }
    if re!(r#"[\w)"'”]\.$"#).is_match(stripped) && !py::drop_last(last, 1).contains('.') {
        return py::drop_last(stripped, 1).to_string();
    }
    stripped.to_string()
}

/// Words `text` capitalizes where no sentence starts: names and the like.
///
/// Each line starts a sentence of its own.
pub fn mid_sentence_capitals(text: &str) -> Names {
    let mut kept = Names::new();
    for line in py::splitlines(text) {
        let words = py::split(line);
        for pair in words.windows(2) {
            let first = first_word(pair[1]);
            if starts_upper(first) && !re!(r"[.?!:…-]$").is_match(pair[0]) {
                kept.insert(first.to_string());
            }
        }
    }
    kept
}

/// Lowercase the capital Whisper gives a phrase that continues a sentence.
///
/// Whisper capitalizes a phrase cut off at a pause, or started in the middle
/// of someone's sentence, as if it began a sentence. Only a common word (or a
/// sentence opener) is lowercased. `I`, acronyms, `names` and words
/// capitalized mid-sentence in `earlier` (the text before this phrase) or
/// later in the phrase itself keep their capitals: those are names, not
/// sentence starts.
pub fn continue_phrase(phrase: &str, earlier: &str, names: &Names) -> String {
    let word = first_word(phrase);
    if !starts_upper(word) {
        return phrase.to_string();
    }
    if names.contains(word) || mid_sentence_capitals(earlier).contains(word) || mid_sentence_capitals(phrase).contains(word)
    {
        return phrase.to_string();
    }
    if opener(&key(word)) || common_word(word) {
        return lower_first(phrase);
    }
    phrase.to_string()
}

/// Whether text inserted after `before` (the field up to the caret) is mid-sentence.
///
/// An empty field, a line break, or a finished sentence (`.`, `?`, `!`, `:`)
/// start a new one. So does anything that isn't a word, a comma or a dash,
/// such as a list bullet.
pub fn continues_sentence(before: &str) -> bool {
    let trailing = &before[py::rstrip(before).len()..];
    let text = py::rstrip_chars(py::rstrip(before), "\"'\u{201d}\u{2019})]");
    let Some(last) = py::final_char(text) else { return false };
    if trailing.contains('\n') {
        return false;
    }
    py::is_alnum(last) || ",;\u{2014}\u{2013}".contains(last)
}

/// Give `text`, cleaned up from `raw`, the casing of raw's first word.
///
/// Cleanup writes every text as the start of a sentence. Where `raw`
/// continues one, the same word must stay lowercase. The word is looked for
/// among raw's first few words, since cleanup drops fillers ("um").
pub fn match_raw_start(text: &str, raw: &str) -> String {
    let word = first_word(text);
    if !starts_upper(word) {
        return text.to_string();
    }
    for heard in re!(r"[^\W\d_][\w'’]*").findall(raw).into_iter().take(4) {
        if py::casefold(heard) == py::casefold(word) {
            return if starts_lower(heard) { lower_first(text) } else { text.to_string() };
        }
    }
    text.to_string()
}

/// Join `phrase` after `before`, text that ended where the audio paused.
///
/// Returns both, adjusted. Whisper ends a phrase cut at a pause as if it
/// were finished, and a question or exclamation mark there is as often the
/// pause as the speaker (`pause? for a second`). Cleanup takes a mark it is
/// given as a sentence end, so the mark goes. The capital Whisper gave the
/// next phrase stays instead: a weaker hint that a new sentence may start,
/// which cleanup drops when the words run on. A word that is always
/// capitalized ("I", a name) can't be that hint, so there the mark stays.
/// Without such a mark the phrase continues as [`continue_phrase`] decides.
pub fn continue_after_seam(before: &str, phrase: &str, earlier: &str, names: &Names) -> (String, String) {
    if !re!(r#"[\w)"'”][?!]$"#).is_match(before) {
        return (before.to_string(), continue_phrase(phrase, earlier, names));
    }
    let word = first_word(phrase);
    if starts_upper(word) && continue_phrase(phrase, earlier, names) == phrase {
        return (before.to_string(), phrase.to_string());
    }
    (py::drop_last(before, 1).to_string(), phrase.to_string())
}

/// A period after these ends an abbreviation, not a sentence.
const ABBREVIATIONS: &[&str] = &["mr", "mrs", "ms", "dr", "st", "vs", "etc", "jr", "sr", "prof", "inc", "ltd", "approx"];

/// "I" and its contractions, as a whole word: not "i.e." or "i18n".
fn lower_i() -> &'static Re {
    re!(r"(?<![\w'’.@/-])i(?=(?:['’](?:m|d|ll|ve|re))?(?![\w'’-])(?!\.\w))")
}

fn is_name(word: &str, names: &Names) -> bool {
    if names.contains(word) || keeps_capital(word) {
        return true;
    }
    !opener(&key(word)) && !common_word(word)
}

/// Whether `token` ends a sentence: not "...", "U.S." or "etc.".
fn ends_sentence(token: &str) -> bool {
    if !re!(r#"[^\W\d_]["')\]”’]*[.?!]["')\]”’]*$"#).is_match(token)
        || re!(r"\.\.|…").is_match(token)
    {
        return false;
    }
    let word = py::rstrip_chars(token, "\"')]\u{201d}\u{2019}.?!");
    !word.contains('.') && !ABBREVIATIONS.contains(&py::casefold(word).as_str())
}

/// Undo lowercasing that cleanup or a seam did against `raw`, the transcript.
///
/// Told a text that continues a sentence, cleanup may write the rest of it in
/// lowercase, "i" included, and after a sentence it ends itself. Whatever the
/// cause: "I" stays capitalized wherever the transcript has it, a name the
/// transcript capitalizes mid-sentence keeps its capital everywhere, and a
/// sentence after `.`, `?` or `!` starts with a capital. A style that starts
/// sentences in lowercase applies after this.
pub fn keep_raw_capitals(text: &str, raw: &str, names: &Names) -> String {
    let mut text = text.to_string();
    if re!(r"(?<![\w'’])I(?![\w-])").is_match(raw) && !lower_i().is_match(raw) {
        text = lower_i().replace(&text, "I");
    }
    for name in mid_sentence_capitals(raw) {
        if name != "I" && !name.starts_with("I'") && !name.starts_with("I\u{2019}") && is_name(&name, names) {
            let lower = py::escape(&py::lower(&name));
            text = Re::new(&format!(r"(?<![\w'’.@/-]){lower}(?![\w-])(?!\.\w)")).replace(&text, &name);
        }
    }
    let mut parts: Vec<String> = re!(r"(\s+)").split(&text).into_iter().map(String::from).collect();
    for index in (2..parts.len()).step_by(2) {
        let token = parts[index].clone();
        let word = first_word(&token);
        if !word.is_empty()
            && py::islower(word)
            && py::lstrip_chars(&token, "\"'(\u{201c}\u{2018}").starts_with(word)
            && ends_sentence(&parts[index - 2])
        {
            let capital = crate::spoken_punctuation::upper_first(word);
            parts[index] = token.replacen(word, &capital, 1);
        }
    }
    parts.concat()
}

/// End a finished dictation that was left open at its last pause.
///
/// A spoken line break at the end stays, after the period.
pub fn close_phrase(text: &str) -> String {
    let stripped = py::rstrip(text);
    if re!(r#"[\w)"'”]$"#).is_match(stripped) {
        let trailing = &text[stripped.len()..];
        return format!("{stripped}.{}", if trailing.contains('\n') { trailing } else { "" });
    }
    text.to_string()
}

/// Attach `phrase` to `previous` across a pause.
///
/// `raw_phrase` is what Whisper heard for `phrase`; its casing decides
/// whether the speaker continued the sentence or began a new one.
pub fn join_phrases(previous: &str, phrase: &str, raw_phrase: &str, style: &str) -> String {
    let before = py::rstrip(previous);
    if before.is_empty() {
        return phrase.to_string();
    }
    if phrase.is_empty() {
        return previous.to_string();
    }
    // Spoken formatting may end a phrase with a line or paragraph break.
    let trailing = &previous[before.len()..];
    let whitespace = if trailing.contains('\n') { trailing } else { " " };
    if phrase.starts_with('\n') {
        // The phrase starts with a spoken line break: it is the whole seam.
        return format!("{before}{}{phrase}", py::strip_chars(whitespace, " "));
    }
    let new_sentence = starts_sentence(raw_phrase);
    let casual = style == "casual";
    let last = py::final_char(before).unwrap();

    if whitespace != " " {
        return format!("{before}{whitespace}{phrase}");
    }
    if TERMINAL.contains(last) {
        if casual && new_sentence && last == '.' && !before.ends_with("..") {
            return format!("{}, {}", py::drop_last(before, 1), lower_first(phrase));
        }
        return format!("{before} {phrase}");
    }
    if last == ',' {
        let phrase = if casual || !new_sentence { lower_first(phrase) } else { phrase.to_string() };
        return format!("{before} {phrase}");
    }
    if !re!(r#"[\w)"'”]$"#).is_match(before) {
        return format!("{before} {phrase}");
    }
    if !new_sentence {
        let phrase = if starts_lower(first_word(raw_phrase)) { lower_first(phrase) } else { phrase.to_string() };
        return format!("{before} {phrase}");
    }
    if casual {
        return format!("{before}, {}", lower_first(phrase));
    }
    format!("{before}. {phrase}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seams() {
        assert_eq!(open_phrase("I went home.", "I went home."), "I went home");
        assert_eq!(strip_pause_mark("if I pause-"), "if I pause");
        assert_eq!(strip_pause_mark("the U.S."), "the U.S.");
        assert_eq!(close_phrase("done\n"), "done.\n");
        assert!(continues_sentence("so I said,"));
        assert!(!continues_sentence("Done."));
        assert_eq!(join_phrases("I went home", "Then I slept", "Then I slept", "standard"), "I went home. Then I slept");
        assert_eq!(join_phrases("I went home.", "Then I slept", "Then I slept", "casual"), "I went home, then I slept");
    }
}
