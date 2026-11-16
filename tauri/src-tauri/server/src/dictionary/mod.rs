//! The user's dictionaries: words dictation should get right
//! (docs/plans/DICTIONARIES.md). Python: `backend/services/dictionary.py`.
//!
//! An entry is a term ("Kubernetes"), which Whisper is prompted with and
//! which is written the user's way after cleanup, or a replacement, which
//! writes `written` wherever `spoken` was said. A phrase is a replacement
//! whose `written` is text to insert exactly ("my email" → an address): it
//! is never a term. Entries belong to every app ("global"), to a writing
//! style, or to one app; a dictation merges its app's, its style's and the
//! global ones, and the most specific wins.
//!
//! This module holds the rules; [`store`] reads and writes `kass.db`, and
//! [`matching`] builds the dictionary a dictation applies to its text
//! (kass-text). Dictations still run in the Python worker, which re-reads
//! the entries when this server tells it they changed.

pub mod matching;
pub mod store;

use chrono::NaiveDateTime;
use serde::Serialize;

pub const MAX_ENTRIES: usize = 1000;
pub const MAX_LENGTH: usize = 200;
/// What a phrase writes: an address or a sign-off, longer than a word.
pub const MAX_PHRASE_LENGTH: usize = 1000;
/// Whisper keeps the last 223 tokens of its prompt; terms take at most this
/// many, and the earlier text the rest.
pub const PROMPT_TOKENS: usize = 64;

/// Who added an entry: the user, a word spelled aloud to fix it, or a word
/// fixed in a correction.
pub const SOURCES: [&str; 3] = ["user", "spoken_fix", "correction"];

/// Words every user's dictionary starts with: the app's own name, which
/// Whisper otherwise writes "Cass" or "Kas".
pub const DEFAULTS: [&str; 1] = ["Kass"];

/// One row: an entry in one place.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub scope: String,
    pub scope_id: Option<String>,
    pub app_name: Option<String>,
    pub written: String,
    pub spoken: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    /// The entry this row is one place of (its own id when it applies in one).
    pub group_id: String,
    /// Off: never swapped in for a word that only sounds like it.
    pub match_sound: bool,
    /// Text inserted exactly where `spoken` is said; never a term.
    pub phrase: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Place {
    pub scope: String,
    pub scope_id: Option<String>,
    pub app_name: Option<String>,
}

impl Place {
    pub fn global() -> Self {
        Place {
            scope: "global".into(),
            scope_id: None,
            app_name: None,
        }
    }
}

/// An entry as the user edits it: one word, in every place it applies.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub written: String,
    pub spoken: Option<String>,
    pub places: Vec<Place>,
    pub created_at: Option<NaiveDateTime>,
    pub match_sound: bool,
    pub source: String,
    pub phrase: bool,
}

/// Why an edit was refused.
#[derive(Debug)]
pub enum EditError {
    /// Bad input, with the message the user sees.
    Invalid(String),
    /// The place already has an entry for the same word said.
    Duplicate(String),
}

impl EditError {
    pub fn message(&self) -> &str {
        match self {
            EditError::Invalid(message) | EditError::Duplicate(message) => message,
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, EditError> {
    Err(EditError::Invalid(message.into()))
}

// -- text, as Python reads it -------------------------------------------------

/// Python's `str.isspace`: Unicode whitespace and the ASCII separators.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `str.strip()`.
fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Python's `" ".join(text.split())`.
fn squeeze(text: &str) -> String {
    text.split(is_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Python's `str.casefold()`.
fn casefold(text: &str) -> String {
    caseless::default_case_fold_str(text)
}

/// Python's `\w`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The lines of `text`, split where Python's `str.splitlines()` splits.
fn lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let breaks = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !breaks {
            continue;
        }
        lines.push(&text[start..at]);
        start = at + c.len_utf8();
        if c == '\r' {
            if let Some(&(next, '\n')) = chars.peek() {
                chars.next();
                start = next + 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The entry's key: the same word said, whatever its case or spacing.
pub fn key(written: &str, spoken: Option<&str>) -> String {
    casefold(&squeeze(spoken.unwrap_or(written)))
}

/// `text` with any run of spaces or hyphens between words as one space,
/// lowercased: "voice box" and "Voice-Box" are the same.
pub fn normal(text: &str) -> String {
    let mut pieces: Vec<&str> = Vec::new();
    let mut piece = 0;
    let mut in_gap = false;
    let text = strip(text);
    for (at, c) in text.char_indices() {
        let gap = is_space(c) || c == '-';
        if gap && !in_gap {
            pieces.push(&text[piece..at]);
        }
        if !gap && in_gap {
            piece = at;
        }
        in_gap = gap;
    }
    pieces.push(if in_gap { "" } else { &text[piece..] });
    casefold(&pieces.join(" "))
}

// -- what the user typed ------------------------------------------------------

fn clean(text: &str, what: &str) -> Result<String, EditError> {
    let cleaned = squeeze(text);
    if cleaned.is_empty() {
        return invalid(format!("{what} can't be empty"));
    }
    if cleaned.chars().count() > MAX_LENGTH {
        return invalid(format!("{what} can be at most {MAX_LENGTH} characters"));
    }
    Ok(cleaned)
}

/// What a phrase writes: its lines kept, each without stray spaces, and no
/// blank lines at either end.
fn clean_phrase_text(text: &str) -> Result<String, EditError> {
    let lines: Vec<String> = lines(text).into_iter().map(squeeze).collect();
    let first = lines.iter().position(|line| !line.is_empty());
    let last = lines.iter().rposition(|line| !line.is_empty());
    let cleaned = match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => return invalid("What to write can't be empty"),
    };
    if cleaned.chars().count() > MAX_PHRASE_LENGTH {
        return invalid(format!(
            "What to write can be at most {MAX_PHRASE_LENGTH} characters"
        ));
    }
    Ok(cleaned)
}

pub fn clean_written(text: &str, phrase: bool) -> Result<String, EditError> {
    if phrase {
        clean_phrase_text(text)
    } else {
        clean(text, "What to write")
    }
}

pub fn clean_spoken(spoken: Option<&str>) -> Result<Option<String>, EditError> {
    let Some(spoken) = spoken.filter(|s| !strip(s).is_empty()) else {
        return Ok(None);
    };
    let cleaned = clean(spoken, "What you say")?;
    if !cleaned.chars().any(is_word_char) {
        return invalid("What you say needs at least one word");
    }
    Ok(Some(cleaned))
}

// -- what a dictation uses ----------------------------------------------------

/// The entries a dictation in `bundle_id` uses, most specific first, and
/// whether a more specific one overrides each (the same word said).
pub fn resolve(
    entries: &[Entry],
    bundle_id: Option<&str>,
    style_id: Option<&str>,
) -> Vec<(Entry, bool)> {
    let rank = |scope: &str| match scope {
        "app" => 0,
        "style" => 1,
        _ => 2,
    };
    let applies_to = |entry: &&Entry| match entry.scope.as_str() {
        "global" => true,
        "style" => {
            style_id.is_some_and(|id| !id.is_empty() && entry.scope_id.as_deref() == Some(id))
        }
        "app" => {
            bundle_id.is_some_and(|id| !id.is_empty() && entry.scope_id.as_deref() == Some(id))
        }
        _ => false,
    };
    let mut applies: Vec<&Entry> = entries.iter().filter(applies_to).collect();
    // Newest first within a scope: a word just added is likely the one in use.
    applies.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
    applies.sort_by_key(|entry| rank(&entry.scope));
    let mut seen = std::collections::HashSet::new();
    applies
        .into_iter()
        .map(|entry| {
            let key = key(&entry.written, entry.spoken.as_deref());
            let overridden = !seen.insert(key);
            (entry.clone(), overridden)
        })
        .collect()
}

/// Whisper prompt candidates for `resolved`, most specific first: terms,
/// then what replacements write, which Whisper needs less since the
/// replacement fixes those whatever it hears. What a phrase writes is text
/// to insert, not a word Whisper hears.
pub fn terms(resolved: &[(Entry, bool)]) -> Vec<String> {
    let mut active: Vec<&Entry> = resolved
        .iter()
        .filter(|(entry, overridden)| !overridden && !entry.phrase)
        .map(|(entry, _)| entry)
        .collect();
    active.sort_by_key(|entry| entry.spoken.as_deref().is_some_and(|s| !s.is_empty()));
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();
    for entry in active {
        let written = strip(&entry.written);
        if seen.insert(normal(written)) {
            terms.push(written.to_string());
        }
    }
    terms
}

/// The terms that fit Whisper's prompt in order, and the rest.
///
/// `count` is the tokens of " <term>"; each term also takes a separator.
/// The first that doesn't fit ends the list, so a lower scope never
/// displaces a higher one.
pub fn fit_terms(
    terms: &[String],
    count: impl Fn(&str) -> usize,
    budget: usize,
) -> (Vec<String>, Vec<String>) {
    let (mut fit, mut dropped, mut used) = (Vec::new(), Vec::new(), 1);
    for term in terms {
        let cost = count(term) + 1;
        if !dropped.is_empty() || used + cost > budget {
            dropped.push(term.clone());
            continue;
        }
        fit.push(term.clone());
        used += cost;
    }
    (fit, dropped)
}

/// A term's size when Whisper's tokenizer isn't loaded: about three
/// characters a token.
pub fn estimated_tokens(term: &str) -> usize {
    term.chars().count().div_ceil(3).max(1)
}

// -- words spelled aloud to fix them -------------------------------------------

fn is_upper(text: &str) -> bool {
    text.chars().any(char::is_uppercase) && !text.chars().any(char::is_lowercase)
}

fn is_lower(text: &str) -> bool {
    text.chars().any(char::is_lowercase) && !text.chars().any(char::is_uppercase)
}

/// How to write a word spelled aloud, from its joined letters.
///
/// Whisper's case for spelled letters is arbitrary ("M-E-G-H-A-N",
/// "m-r-g-n"), so letters all in one case are written as a name ("Meghan"),
/// unless the word heard alongside them is the same letters in capitals of
/// its own ("NASA", "iPhone"). Letters in mixed case were spelled that way,
/// and a word with digits or marks ("mrgnhnt96") is left as spelled.
pub fn spelled_word(letters: &str, heard: Option<&str>) -> String {
    let word = squeeze(letters);
    let word = word.trim_end_matches(['.', ',', ';', ':', '?', '!']);
    let heard = squeeze(heard.unwrap_or(""));
    let heard = heard.trim_matches(['.', ',', ';', ':', '?', '!', '"', '\'', '“', '”']);
    if !heard.is_empty() && casefold(heard) == casefold(word) && heard != heard.to_lowercase() {
        return heard.to_string();
    }
    let alpha = !word.is_empty() && word.chars().all(char::is_alphabetic);
    if alpha && (is_upper(word) || is_lower(word)) {
        let mut chars = word.chars();
        let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
        return first.unwrap_or_default() + &chars.as_str().to_lowercase();
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        scope: &str,
        scope_id: Option<&str>,
        written: &str,
        spoken: Option<&str>,
        at: u32,
    ) -> Entry {
        Entry {
            id: format!("{scope}-{written}-{at}"),
            scope: scope.into(),
            scope_id: scope_id.map(Into::into),
            app_name: None,
            written: written.into(),
            spoken: spoken.map(Into::into),
            created_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, at),
            group_id: format!("{scope}-{written}-{at}"),
            match_sound: true,
            phrase: false,
        }
    }

    const ZED: &str = "dev.zed.Zed";

    #[test]
    fn the_most_specific_scope_wins_for_the_same_word_said() {
        let entries = vec![
            entry("global", None, "Voicebox", None, 1),
            entry("style", Some("formal"), "VoiceBox", Some("voicebox"), 2),
            entry("app", Some(ZED), "voicebox", None, 3),
            entry("app", Some("com.other"), "VOICEBOX", None, 4),
        ];
        let resolved = resolve(&entries, Some(ZED), Some("formal"));
        let shown: Vec<(&str, bool)> = resolved
            .iter()
            .map(|(e, overridden)| (e.written.as_str(), *overridden))
            .collect();
        assert_eq!(
            shown,
            [("voicebox", false), ("VoiceBox", true), ("Voicebox", true)]
        );
        assert_eq!(terms(&resolved), ["voicebox"]);
    }

    #[test]
    fn terms_come_most_specific_first_and_newest_first_within_a_scope() {
        let entries = vec![
            entry("global", None, "Older", None, 1),
            entry("global", None, "Newer", None, 2),
            entry("app", Some(ZED), "Zedword", None, 0),
            entry("global", None, "k8s", Some("kates"), 3),
        ];
        let resolved = resolve(&entries, Some(ZED), None);
        assert_eq!(terms(&resolved), ["Zedword", "Newer", "Older", "k8s"]);
        assert_eq!(
            terms(&resolve(&entries, None, None)),
            ["Newer", "Older", "k8s"]
        );
    }

    #[test]
    fn a_phrase_is_never_a_term() {
        let mut phrase = entry("global", None, "Thanks,\nMorgan", Some("sign off"), 1);
        phrase.phrase = true;
        assert!(terms(&resolve(&[phrase], None, None)).is_empty());
    }

    #[test]
    fn terms_fill_the_prompt_budget_in_order() {
        let terms: Vec<String> = ["aaaa", "bbbbbbbbbb", "cc", "dd"].map(String::from).into();
        let (fit, dropped) = fit_terms(&terms, |term| term.len(), 19);
        assert_eq!(fit, ["aaaa", "bbbbbbbbbb"]);
        assert_eq!(dropped, ["cc", "dd"]);
        assert_eq!(estimated_tokens("Kubernetes"), 4);
        assert_eq!(estimated_tokens(""), 1);
    }

    #[test]
    fn keys_and_normal_forms_match_python() {
        assert_eq!(key("Voicebox", None), "voicebox");
        assert_eq!(key("k8s", Some("  Kates   Cluster ")), "kates cluster");
        assert_eq!(key("Straße", None), "strasse");
        assert_eq!(normal(" Voice-Box  app "), "voice box app");
        assert_eq!(normal("-abc-"), " abc ");
    }

    #[test]
    fn cleaning_matches_python() {
        assert_eq!(clean_written("  Kass \t app ", false).unwrap(), "Kass app");
        assert_eq!(
            clean_written("\n\n Thanks,  \r\n\n  Morgan \n\n", true).unwrap(),
            "Thanks,\n\nMorgan"
        );
        assert_eq!(
            clean_written("   ", false).unwrap_err().message(),
            "What to write can't be empty"
        );
        assert_eq!(
            clean_written(&"a".repeat(201), false)
                .unwrap_err()
                .message(),
            "What to write can be at most 200 characters"
        );
        assert_eq!(clean_spoken(Some("  ")).unwrap(), None);
        assert_eq!(clean_spoken(None).unwrap(), None);
        assert_eq!(
            clean_spoken(Some("?!")).unwrap_err().message(),
            "What you say needs at least one word"
        );
    }

    #[test]
    fn a_spelled_word_is_written_as_a_name() {
        let cases = [
            ("MEGHAN", None, "Meghan"),
            ("meghan.", None, "Meghan"),
            ("NASA", Some("NASA"), "NASA"),
            ("IPHONE", Some("iPhone,"), "iPhone"),
            ("McKay", None, "McKay"),
            ("mrgnhnt96", None, "mrgnhnt96"),
            ("MEGHAN", Some("megan"), "Meghan"),
        ];
        for (letters, heard, written) in cases {
            assert_eq!(spelled_word(letters, heard), written, "{letters}");
        }
    }
}
