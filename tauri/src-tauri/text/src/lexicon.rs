//! Which words are common words rather than names.
//!
//! Whisper's vocabulary says which casing of a word is the more frequent one
//! after a space; macOS's word list lists common words in lowercase and names
//! capitalized. Both are read once, on first use, unless [`install`] gave
//! them first.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::py;

/// macOS's word list: common words in lowercase, names capitalized.
const WORD_LIST: &str = "/usr/share/dict/words";
/// Whisper's models, in the order the vocabulary is looked for.
const WHISPER_REPOS: &[&str] = &[
    "openai/whisper-base",
    "openai/whisper-small",
    "openai/whisper-medium",
    "openai/whisper-large-v3",
    "openai/whisper-large-v3-turbo",
];
const SUFFIXES: &[&str] = &["'s", "\u{2019}s", "ies", "es", "s", "ed", "d", "ing", "ly"];
/// The byte-level BPE marker for a leading space.
const SPACE: char = '\u{0120}';

/// Whisper's vocabulary and the system word list.
#[derive(Default)]
pub struct WordData {
    /// Whisper's BPE vocabulary, whose ids rise roughly as tokens get rarer.
    vocabulary: HashMap<String, i64>,
    /// Words the word list has in lowercase.
    lower: HashSet<String>,
    /// Words the word list has capitalized, casefolded.
    capitalized: HashSet<String>,
}

impl WordData {
    /// From a vocabulary (token to id) and a word list's text.
    pub fn new(vocabulary: HashMap<String, i64>, word_list: &str) -> WordData {
        let entries = py::split(word_list);
        WordData {
            vocabulary,
            lower: entries.iter().filter(|w| py::islower(w)).map(|w| w.to_string()).collect(),
            capitalized: entries
                .iter()
                .filter(|w| py::first(w).is_some_and(|c| py::isupper(&c.to_string())))
                .map(|w| py::casefold(w))
                .collect(),
        }
    }

    /// Whisper's vocabulary from the Hugging Face cache (empty when no
    /// Whisper model is downloaded) and macOS's word list.
    pub fn load() -> WordData {
        let word_list = std::fs::read_to_string(WORD_LIST).unwrap_or_default();
        WordData::new(load_vocabulary(), &word_list)
    }

    /// Whether `word` is mostly written in lowercase mid-sentence, so not a name.
    ///
    /// Whisper's vocabulary says which casing is the more frequent word after
    /// a space: " move" comes long before " Move", and " Sarah" has no
    /// lowercase token at all. A word with neither is looked up in the word
    /// list (and "tests" as "test"), which lists names capitalized.
    pub fn common_word(&self, word: &str) -> bool {
        let capital = format!("{SPACE}{}{}", py::upper(py::head(word, 1)), py::lower(py::tail_from(word, 1)));
        let lower = self.vocabulary.get(&format!("{SPACE}{}", py::lower(word)));
        let capital = self.vocabulary.get(&capital);
        if lower.is_some() || capital.is_some() {
            return match (lower, capital) {
                (Some(lower), Some(capital)) => lower < capital,
                (Some(_), None) => true,
                _ => false,
            };
        }
        let key = py::casefold(word);
        let mut stems = vec![key.clone()];
        for suffix in SUFFIXES {
            if let Some(stem) = key.strip_suffix(suffix) {
                if py::len(&key) > py::len(suffix) + 2 {
                    stems.push(if *suffix == "ies" { format!("{stem}y") } else { stem.to_string() });
                }
            }
        }
        stems.iter().any(|stem| self.lower.contains(stem) && !self.capitalized.contains(stem))
    }
}

fn hub_cache() -> Option<PathBuf> {
    if let Some(cache) = std::env::var_os("HF_HUB_CACHE") {
        return Some(cache.into());
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        return Some(PathBuf::from(home).join("hub"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/huggingface/hub"))
}

fn load_vocabulary() -> HashMap<String, i64> {
    let Some(cache) = hub_cache() else { return HashMap::new() };
    for repo in WHISPER_REPOS {
        let model = cache.join(format!("models--{}", repo.replace('/', "--")));
        let revision = std::fs::read_to_string(model.join("refs/main")).unwrap_or_else(|_| "main".into());
        let path = model.join("snapshots").join(revision.trim()).join("vocab.json");
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Ok(vocabulary) = serde_json::from_str(&text) {
            return vocabulary;
        }
    }
    HashMap::new()
}

static WORD_DATA: OnceLock<WordData> = OnceLock::new();

/// Use `data` instead of reading the vocabulary and word list. Fails when
/// they were already read.
pub fn install(data: WordData) -> Result<(), WordData> {
    WORD_DATA.set(data)
}

/// Read the vocabulary and word list now, so the first dictation doesn't wait.
pub fn word_data() -> &'static WordData {
    WORD_DATA.get_or_init(WordData::load)
}

/// [`WordData::common_word`] with the installed or loaded data.
pub fn common_word(word: &str) -> bool {
    word_data().common_word(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_by_vocabulary_then_word_list() {
        let vocabulary = HashMap::from([("\u{0120}move".to_string(), 5), ("\u{0120}Move".to_string(), 900)]);
        let data = WordData::new(vocabulary, "test\nSarah\nberry\n");
        assert!(data.common_word("Move"));
        assert!(data.common_word("Tests"));
        assert!(data.common_word("berries"));
        assert!(!data.common_word("Sarah"));
    }
}
