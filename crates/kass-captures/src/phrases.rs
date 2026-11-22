//! Turning what a capture shows into dictionary entries: a selected word, a
//! correction's change, or a phrase the dictionary wrote into it.

use crate::word_diff::{DiffHunk, is_word_char};
use kass_api::types::{DictionaryEntry, DictionaryEntryCreate, DictionaryPlaceInput};

/// Matches the server's limit on either side of an entry.
pub const MAX_LENGTH: usize = 200;
/// A word or a short phrase; a selected sentence is not a dictionary entry.
const MAX_WORDS: usize = 8;

fn trim_non_word(text: &str) -> &str {
    text.trim_matches(|c: char| !is_word_char(c))
}

/// Selected transcript text as a dictionary phrase: one line, without the
/// punctuation the selection picked up around it ("box," is "box"). Empty
/// when the selection is too long to be a word or phrase.
pub fn selection_phrase(selected: &str) -> String {
    let words: Vec<&str> = selected.split_whitespace().collect();
    let joined = words.join(" ");
    let phrase = trim_non_word(&joined);
    if phrase.is_empty()
        || phrase.chars().count() > MAX_LENGTH
        || phrase.split(' ').count() > MAX_WORDS
    {
        return String::new();
    }
    phrase.to_string()
}

/// A word Kass heard, and how the user wants it written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryWord {
    pub said: String,
    pub written: String,
}

/// A correction's change as a dictionary word. None for words only added or
/// removed, or a rewrite too long to be a word or phrase.
pub fn dictionary_word(hunk: &DiffHunk) -> Option<DictionaryWord> {
    let said = selection_phrase(&hunk.removed);
    let written = selection_phrase(&hunk.added);
    (!said.is_empty() && !written.is_empty()).then_some(DictionaryWord { said, written })
}

fn same(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// The entry for a word picked from a capture, spelled the user's way, in
/// `places`. Spelled as it was written, it only teaches the spelling;
/// spelled otherwise, what was written is also replaced.
pub fn spelling_entry(
    said: &str,
    written: &str,
    places: Vec<DictionaryPlaceInput>,
) -> DictionaryEntryCreate {
    let spoken = if same(said, written) || said.trim().is_empty() {
        None
    } else {
        Some(said.trim().to_string())
    };
    DictionaryEntryCreate {
        written: written.trim().to_string(),
        spoken,
        places,
        match_sound: None,
        phrase: None,
    }
}

/// A new phrase: `written` is inserted exactly wherever `spoken` is said.
pub fn new_phrase(
    spoken: &str,
    written: &str,
    places: Vec<DictionaryPlaceInput>,
) -> DictionaryEntryCreate {
    DictionaryEntryCreate {
        written: written.to_string(),
        spoken: Some(spoken.trim().to_string()),
        places,
        match_sound: None,
        phrase: Some(true),
    }
}

/// `text` with the phrase between byte offsets `start`..`end` spelled
/// `written`, keeping the punctuation around it.
pub fn respell_selection(text: &str, start: usize, end: usize, written: &str) -> String {
    let selected = &text[start..end];
    let lead_len = selected.len()
        - selected
            .trim_start_matches(|c: char| !is_word_char(c))
            .len();
    let rest = &selected[lead_len..];
    let core = rest.trim_end_matches(|c: char| !is_word_char(c));
    let trail = &rest[core.len()..];
    format!(
        "{}{}{}{}{}",
        &text[..start],
        &selected[..lead_len],
        written.trim(),
        trail,
        &text[end..]
    )
}

/// A word or phrase selected in the text being corrected, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    /// The selection as a dictionary phrase.
    pub phrase: String,
    /// The text it was selected in.
    pub text: String,
    /// The selection's byte offsets in `text`.
    pub start: usize,
    pub end: usize,
}

/// The selection `start..end` of `text` as a word to add, when it is a word
/// or short phrase.
pub fn pick(text: &str, start: usize, end: usize) -> Option<Picked> {
    if start >= end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    let phrase = selection_phrase(&text[start..end]);
    (!phrase.is_empty()).then(|| Picked {
        phrase,
        text: text.to_string(),
        start,
        end,
    })
}

/// The fix to save once `written` is in the dictionary for `picked`: `current`
/// with the pick spelled `written`, or as it is when spelled as picked. None
/// when the text changed since it was picked, so the offsets no longer hold.
pub fn respelled_fix(current: &str, picked: &Picked, written: &str) -> Option<String> {
    if current != picked.text {
        return None;
    }
    Some(if written.trim() == picked.phrase {
        current.to_string()
    } else {
        respell_selection(current, picked.start, picked.end, written)
    })
}

/// A phrase to make from a correction: the words Kass heard, and the text the user wants there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhraseDraft {
    pub said: String,
    pub written: String,
}

/// A correction's change as a phrase: words Kass heard that the user
/// replaced with other text. None for words only added or removed.
pub fn phrase_from_hunk(hunk: &DiffHunk) -> Option<PhraseDraft> {
    let said = selection_phrase(&hunk.removed).to_lowercase();
    let written = hunk.added.trim().to_string();
    (!said.is_empty() && !written.is_empty()).then_some(PhraseDraft { said, written })
}

/// A phrase from the dictionary that wrote its text into a capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FiredPhrase {
    pub id: String,
    pub spoken: String,
    pub written: String,
}

fn joins_word(c: char) -> bool {
    is_word_char(c) || matches!(c, '_' | '\'' | '’' | '-')
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || c == '-'
}

/// `spoken` said in `text`: whole words, any case, spaces or hyphens between
/// them, as the server matches.
fn said(text: &str, spoken: &str) -> bool {
    let words: Vec<Vec<char>> = spoken
        .split(is_separator)
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase().chars().collect())
        .collect();
    if words.is_empty() {
        return false;
    }
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let matches_at = |mut at: usize| -> bool {
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                let gap = text[at..].iter().take_while(|&&c| is_separator(c)).count();
                if gap == 0 {
                    return false;
                }
                at += gap;
            }
            if !text[at..].starts_with(word) {
                return false;
            }
            at += word.len();
        }
        text.get(at).is_none_or(|&c| !joins_word(c))
    };
    (0..text.len()).any(|at| (at == 0 || !joins_word(text[at - 1])) && matches_at(at))
}

/// The dictionary's phrases a capture used: said in what Kass heard, and
/// written in what it delivered. One per text written, newest entry first.
pub fn fired_phrases(raw: &str, delivered: &str, entries: &[DictionaryEntry]) -> Vec<FiredPhrase> {
    let mut fired: Vec<FiredPhrase> = Vec::new();
    for entry in entries {
        let Some(spoken) = entry.spoken.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        if !entry.phrase || entry.written.is_empty() {
            continue;
        }
        if fired.iter().any(|f| f.written == entry.written) {
            continue;
        }
        if delivered.contains(&entry.written) && said(raw, spoken) {
            fired.push(FiredPhrase {
                id: entry.id.clone(),
                spoken: spoken.to_string(),
                written: entry.written.clone(),
            });
        }
    }
    fired
}

/// A run of delivered text, and the phrase that wrote it, if one did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhraseRun {
    pub text: String,
    pub phrase: Option<FiredPhrase>,
}

/// Text with where each phrase wrote into it marked, in order; the longest
/// wins where two overlap.
pub fn mark_phrases(text: &str, phrases: &[FiredPhrase]) -> Vec<PhraseRun> {
    let mut by_length: Vec<&FiredPhrase> = phrases.iter().collect();
    by_length.sort_by_key(|p| std::cmp::Reverse(p.written.len()));
    let mut runs = Vec::new();
    let mut plain = String::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if let Some(phrase) = by_length.iter().find(|p| rest.starts_with(&p.written)) {
            if !plain.is_empty() {
                runs.push(PhraseRun {
                    text: std::mem::take(&mut plain),
                    phrase: None,
                });
            }
            runs.push(PhraseRun {
                text: phrase.written.clone(),
                phrase: Some((*phrase).clone()),
            });
            at += phrase.written.len();
        } else {
            let c = rest.chars().next().expect("not at the end");
            plain.push(c);
            at += c.len_utf8();
        }
    }
    if !plain.is_empty() {
        runs.push(PhraseRun {
            text: plain,
            phrase: None,
        });
    }
    runs
}

/// Letters and digits only, for telling whether a change wrote a phrase's text.
pub fn phrase_key(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|&c| is_word_char(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kass_api::types::{DictionaryPlace, DictionaryScope, DictionarySource};

    fn phrase(id: &str, spoken: &str, written: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: id.into(),
            written: written.into(),
            spoken: Some(spoken.into()),
            places: vec![DictionaryPlace {
                scope: DictionaryScope::Global,
                scope_id: None,
                app_name: None,
            }],
            created_at: None,
            match_sound: true,
            source: DictionarySource::User,
            phrase: true,
        }
    }

    #[test]
    fn a_selection_drops_surrounding_punctuation() {
        assert_eq!(selection_phrase(" box, "), "box");
        assert_eq!(selection_phrase("“post\n grass”"), "post grass");
        assert_eq!(selection_phrase("..."), "");
        assert_eq!(
            selection_phrase("one two three four five six seven eight nine"),
            ""
        );
    }

    #[test]
    fn a_hunk_becomes_a_word_only_when_both_sides_have_words() {
        assert_eq!(
            dictionary_word(&DiffHunk::new("post grass", "Postgres")),
            Some(DictionaryWord {
                said: "post grass".into(),
                written: "Postgres".into()
            })
        );
        assert_eq!(dictionary_word(&DiffHunk::new("", "added")), None);
    }

    #[test]
    fn a_spelling_spelled_the_same_has_no_spoken_side() {
        let entry = spelling_entry("Kass", " Kass ", vec![DictionaryPlaceInput::global()]);
        assert_eq!(entry.written, "Kass");
        assert_eq!(entry.spoken, None);
        let entry = spelling_entry("cast", "Kass", vec![DictionaryPlaceInput::global()]);
        assert_eq!(entry.spoken.as_deref(), Some("cast"));
    }

    #[test]
    fn respelling_keeps_the_punctuation_around_the_selection() {
        let text = "Ask Sagar, then go.";
        assert_eq!(
            respell_selection(text, 4, 10, "Saggar"),
            "Ask Saggar, then go."
        );
    }

    #[test]
    fn a_pick_is_a_word_or_short_phrase() {
        let text = "Ask Sagar, then go.";
        let picked = pick(text, 4, 10).unwrap();
        assert_eq!(picked.phrase, "Sagar");
        assert_eq!((picked.start, picked.end), (4, 10));
        assert_eq!(pick(text, 4, 4), None, "nothing selected");
        assert_eq!(pick(text, 9, 11), None, "only punctuation");
        assert_eq!(pick(text, 0, 99), None, "past the end");
        assert_eq!(pick("café au lait", 0, 4), None, "inside a character");
        let long = "one two three four five six seven eight nine";
        assert_eq!(pick(long, 0, long.len()), None, "a sentence");
    }

    #[test]
    fn the_fix_respells_the_pick_while_the_text_is_unchanged() {
        let picked = pick("Ask Sagar, then go.", 4, 10).unwrap();
        assert_eq!(
            respelled_fix("Ask Sagar, then go.", &picked, "Saggar").as_deref(),
            Some("Ask Saggar, then go.")
        );
        assert_eq!(
            respelled_fix("Ask Sagar, then go.", &picked, " Sagar ").as_deref(),
            Some("Ask Sagar, then go."),
            "spelled as picked keeps the text"
        );
        assert_eq!(
            respelled_fix("Ask Sagar now.", &picked, "Saggar"),
            None,
            "the text moved on"
        );
    }

    #[test]
    fn phrases_fire_when_said_and_written() {
        let entries = vec![
            phrase("1", "my email", "me@example.com"),
            phrase("2", "sign off", "Thanks,\nMorgan"),
        ];
        let fired = fired_phrases(
            "send it to My-Email please",
            "Send it to me@example.com please",
            &entries,
        );
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, "1");
        assert!(fired_phrases("my emails", "me@example.com", &entries).is_empty());
    }

    #[test]
    fn marking_takes_the_longest_phrase() {
        let short = FiredPhrase {
            id: "a".into(),
            spoken: "x".into(),
            written: "abc".into(),
        };
        let long = FiredPhrase {
            id: "b".into(),
            spoken: "y".into(),
            written: "abcdef".into(),
        };
        let runs = mark_phrases("é abcdef abc", &[short, long]);
        let texts: Vec<_> = runs
            .iter()
            .map(|r| (r.text.as_str(), r.phrase.as_ref().map(|p| p.id.as_str())))
            .collect();
        assert_eq!(
            texts,
            vec![
                ("é ", None),
                ("abcdef", Some("b")),
                (" ", None),
                ("abc", Some("a"))
            ]
        );
    }

    #[test]
    fn phrase_from_hunk_lowercases_what_was_said() {
        assert_eq!(
            phrase_from_hunk(&DiffHunk::new("My Sig", "Thanks, M")),
            Some(PhraseDraft {
                said: "my sig".into(),
                written: "Thanks, M".into()
            })
        );
        assert_eq!(phrase_key("Thanks, M!"), "thanksm");
    }
}
