//! Which sentences of a streaming cleanup are final.
//!
//! Streaming dictation cleans the open tail: the raw words since the last
//! final sentence. A pause is often the speaker thinking mid-sentence, so the
//! last sentence of a cleanup stays open; the next words may continue it.
//! Every sentence before it is final and never cleaned again.
//!
//! [`settle`] finds that boundary in the cleaned text and the raw word it
//! corresponds to, by aligning the two word for word. It settles nothing
//! unless both sides of the boundary line up with words the speaker said: a
//! boundary placed on the wrong raw word would repeat or lose words.

use std::collections::HashMap;

use crate::difflib::SequenceMatcher;
use crate::py::{self, Groups};
use crate::re;

/// A tail that grows past this many raw words without a sentence ending is
/// settled anyway, so the cleanup after release stays about one phrase long.
/// 99% of the user's cleaned sentences are shorter (p50 7, p95 21, p99 32).
pub const MAX_OPEN_WORDS: usize = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// Final cleaned text, ending with its sentence's punctuation.
    pub committed: String,
    /// The whitespace the cleanup put after it.
    pub gap: String,
    /// The cleaned last sentence, still open.
    pub open_cleaned: String,
    /// The raw words the open sentence came from.
    pub open_raw: String,
    /// The raw words before them.
    pub committed_raw: String,
}

fn key(word: &str) -> String {
    py::lower(&re!(r"[^\w']").replace(&word.replace('\u{2019}', "'"), ""))
}

/// Words with their start offsets; punctuation-only tokens are skipped.
fn words(text: &str) -> Vec<(String, usize)> {
    re!(r"\S+")
        .finditer(text)
        .iter()
        .map(|found| (key(found.text()), found.start()))
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

/// Split `cleaned` into final sentences and the open last one, or `None`.
pub fn settle(raw: &str, cleaned: &str) -> Option<Settled> {
    // A sentence ends after a word, not a number or list marker ("1.",
    // "$5."), with its mark inside or after a closing quote.
    let ending = re!(r#"[^\W\d_]["')\]”’]*[.?!]["')\]”’]*$"#);
    let mut boundary = None;
    for gap in re!(r"\s+").finditer(cleaned) {
        if gap.end() < cleaned.len() && (gap.text().contains('\n') || ending.is_match(&cleaned[..gap.start()])) {
            boundary = Some((gap.start(), gap.end()));
        }
    }
    let (gap_start, gap_end) = boundary?;
    let raw_words = py::split(raw);
    let said: Vec<String> = raw_words.iter().map(|word| key(word)).collect();
    let written = words(cleaned);
    let written_keys: Vec<String> = written.iter().map(|(key, _)| key.clone()).collect();
    let matcher = SequenceMatcher::new(&said, &written_keys, false);
    let mut aligned = HashMap::new();
    for block in matcher.get_matching_blocks() {
        for offset in 0..block.size {
            aligned.insert(block.b + offset, block.a + offset);
        }
    }
    // Python raises here when no word follows the boundary; nothing settles.
    let first_open = written.iter().position(|(_, start)| *start >= gap_end)?;
    let last_committed = first_open.checked_sub(1)?;
    let (Some(&before), Some(&after)) = (aligned.get(&last_committed), aligned.get(&first_open)) else {
        return None;
    };
    let start = before + 1;
    if after < start {
        return None;
    }
    Some(Settled {
        committed: cleaned[..gap_start].to_string(),
        gap: cleaned[gap_start..gap_end].to_string(),
        open_cleaned: cleaned[gap_end..].to_string(),
        open_raw: raw_words[start..].join(" "),
        committed_raw: raw_words[..start].join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settles_the_finished_sentences() {
        let settled = settle("so I went home and then I slept", "So I went home. And then I slept").unwrap();
        assert_eq!(settled.committed, "So I went home.");
        assert_eq!(settled.open_cleaned, "And then I slept");
        assert_eq!(settled.open_raw, "and then I slept");
        assert_eq!(settled.committed_raw, "so I went home");
        assert_eq!(settle("one two", "One two"), None);
    }
}
