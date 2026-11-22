//! Re-dictation cleanup (docs/plans/MID_SENTENCE_DICTATION.md, section 5).
//!
//! The user puts the caret mid-sentence and says the new words, often
//! running on into the words already after the caret to lead back into
//! them: `Let's meet | at noon tomorrow.` + "on Friday at noon tomorrow".
//! The repeated words are dropped from the dictated text, never from the
//! field, so the field keeps its own spelling, case and punctuation.
//!
//! Every rule is general (no word lists):
//!
//! 1. The caret continues a sentence (the same rule as the backend's
//!    `phrase_seams.continues_sentence`) and is not inside a word. A side
//!    that can't be read trims nothing. A repeat of the whole rest of the
//!    sentence (see 4) may also follow a sentence end or the field's start.
//! 2. Only the rest of the caret's sentence counts: the text after the caret
//!    up to `.?!…` before a capital or the end, or up to a line break. It
//!    must start right at the caret, after nothing but whitespace.
//! 3. The overlap is the longest end of the dictation that equals a start of
//!    that text. Words compare ignoring case and punctuation, and split at
//!    whitespace, hyphens, slashes and digit/letter changes
//!    (`3pm` = `3 pm` = `3 p.m.`, curly and straight apostrophes alike).
//! 4. At least two field words overlap, and at least one dictated word stays.
//!    One word is enough when it is the whole rest of the field's sentence
//!    and the dictation ends its sentence on it too: `Thanks. | Morgan.` +
//!    "See you Tuesday, Morgan."
//! 5. A word may differ by one edit (Levenshtein) when both are at least five
//!    characters, start with the same letter and hold no digits: at most one
//!    such word in three, and never all of them.
//! 6. The overlap may not cross a sentence end inside the dictation. Emoji
//!    must match exactly.

/// Punctuation, which words compare without.
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '\u{2018}'
                | '\u{2019}'
                | '\u{201C}'
                | '\u{201D}'
                | '\u{2026}'
                | '\u{2013}'
                | '\u{2014}'
        )
}

/// Punctuation that splits a word in two (`follow-up` = `follow up`).
fn splits(c: char) -> bool {
    matches!(c, '-' | '/' | ':' | '\u{2013}' | '\u{2014}')
}

/// Zero-width joiner, variation selectors and skin tones: part of an emoji.
fn is_joiner(c: char) -> bool {
    matches!(
        c,
        '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{1F3FB}'..='\u{1F3FF}'
    )
}

const TERMINATORS: &[char] = &['.', '?', '!', '\u{2026}'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Letter,
    Digit,
    /// Anything else that isn't punctuation: emoji and other symbols.
    Symbol,
}

/// One compared unit of a whitespace-separated word.
#[derive(Debug, Clone)]
struct Token {
    /// Lowercased, without punctuation.
    norm: String,
    class: Class,
    /// Byte offset of the token, or of its whole word for a word's first token.
    start: usize,
    /// Index of its whitespace-separated word.
    word: usize,
    /// Starts or ends its word, so a cut there is on a word boundary.
    word_start: bool,
    word_end: bool,
    /// Its word ends a sentence: `noon.` before a capital or the end, or
    /// any word before a line break.
    ends_sentence: bool,
}

impl Token {
    fn alnum(&self) -> bool {
        self.class != Class::Symbol
    }
}

/// Byte ranges of the whitespace-separated words in `text`.
fn words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                words.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push((s, text.len()));
    }
    words
}

fn tokenize(text: &str) -> Vec<Token> {
    let words = words(text);
    let mut tokens: Vec<Token> = Vec::new();
    for (index, &(start, end)) in words.iter().enumerate() {
        let word = &text[start..end];
        let terminated = word
            .trim_end_matches(|c: char| is_punct(c) && !TERMINATORS.contains(&c))
            .ends_with(TERMINATORS);
        let next = words.get(index + 1);
        let next_starts_sentence = next.is_none_or(|&(s, e)| {
            text[s..e]
                .chars()
                .find(|c| !is_punct(*c))
                .is_some_and(char::is_uppercase)
        });
        let line_break = text[end..next.map_or(text.len(), |w| w.0)].contains('\n');
        let ends_sentence = (terminated && next_starts_sentence) || line_break;

        let first = tokens.len();
        let mut current: Option<Token> = None;
        for (offset, c) in word.char_indices() {
            let class = if c.is_alphabetic() {
                Class::Letter
            } else if c.is_numeric() {
                Class::Digit
            } else if is_punct(c) {
                // Dropped inside a word: `don't` = `dont`, `p.m.` = `pm`.
                if splits(c) {
                    tokens.extend(current.take());
                }
                continue;
            } else if is_joiner(c) {
                if let Some(token) = current.as_mut() {
                    token.norm.push(c);
                }
                continue;
            } else {
                Class::Symbol
            };
            if current.as_ref().is_none_or(|t| t.class != class) {
                tokens.extend(current.take());
                current = Some(Token {
                    norm: String::new(),
                    class,
                    start: start + offset,
                    word: index,
                    word_start: false,
                    word_end: false,
                    ends_sentence: false,
                });
            }
            if let Some(token) = current.as_mut() {
                token.norm.extend(c.to_lowercase());
            }
        }
        tokens.extend(current);
        if tokens.len() > first {
            let last = tokens.len() - 1;
            tokens[first].word_start = true;
            tokens[first].start = start;
            tokens[last].word_end = true;
            tokens[last].ends_sentence = ends_sentence;
        }
    }
    tokens
}

/// Whether `a` and `b` are at most one insertion, deletion or substitution
/// apart.
fn one_edit_apart(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (long, short) = if a.len() >= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if long.len() - short.len() > 1 {
        return false;
    }
    let same_start = long
        .iter()
        .zip(short.iter())
        .take_while(|(x, y)| x == y)
        .count();
    // Skip the one differing character in the longer word, or in both.
    let rest = if long.len() == short.len() {
        &short[(same_start + 1).min(short.len())..]
    } else {
        &short[same_start..]
    };
    long[(same_start + 1).min(long.len())..] == *rest
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Same {
    Exact,
    Fuzzy,
    No,
}

fn compare(a: &Token, b: &Token) -> Same {
    if a.norm == b.norm {
        return Same::Exact;
    }
    let spelled = |t: &Token| t.class == Class::Letter && t.norm.chars().count() >= 5;
    if spelled(a)
        && spelled(b)
        && a.norm.chars().next() == b.norm.chars().next()
        && one_edit_apart(&a.norm, &b.norm)
    {
        Same::Fuzzy
    } else {
        Same::No
    }
}

/// Whether text inserted after `before` (the field up to the caret) is
/// mid-sentence. The backend's `phrase_seams.continues_sentence`.
fn continues_sentence(before: &str) -> bool {
    let trimmed = before.trim_end();
    let text = trimmed.trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')', ']']);
    let Some(last) = text.chars().last() else {
        return false;
    };
    !before[trimmed.len()..].contains('\n')
        && (last.is_alphanumeric() || matches!(last, ',' | ';' | '\u{2014}' | '\u{2013}'))
}

/// How many bytes of `text` to keep: less than all of it when its last
/// words repeat the words right after the caret. `before` and `after` are
/// the field on each side of the caret, `None` where unreadable.
fn kept(before: Option<&str>, text: &str, after: Option<&str>) -> Option<usize> {
    let (before, after) = (before?, after?);
    let mid_sentence = continues_sentence(before);
    // A caret inside a word is not where a phrase is said again from.
    if before.chars().last().is_some_and(char::is_alphanumeric)
        && after.chars().next().is_some_and(char::is_alphanumeric)
    {
        return None;
    }
    let said = tokenize(text);
    let mut field = Vec::new();
    for token in tokenize(after) {
        let ends = token.ends_sentence;
        field.push(token);
        if ends {
            break;
        }
    }
    if !after[..field.first()?.start].trim().is_empty() {
        return None;
    }
    for k in (1..=said.len().min(field.len())).rev() {
        let (said_end, field_start) = (&said[said.len() - k..], &field[..k]);
        if !said_end[0].word_start || !field_start[k - 1].word_end {
            continue;
        }
        // Its last word may end a sentence; no other may.
        if said_end[..k - 1].iter().any(|t| t.ends_sentence) {
            continue;
        }
        let mut fuzzy = 0;
        let matched = said_end
            .iter()
            .zip(field_start)
            .all(|(a, b)| match compare(a, b) {
                Same::Exact => true,
                Same::Fuzzy => {
                    fuzzy += 1;
                    true
                }
                Same::No => false,
            });
        if !matched {
            continue;
        }
        let mut field_words: Vec<usize> = field_start
            .iter()
            .filter(|t| t.alnum())
            .map(|t| t.word)
            .collect();
        field_words.dedup();
        let adds_a_word = said[..said.len() - k].iter().any(Token::alnum);
        // The whole rest of the field's sentence, said to the end of one.
        let whole = field_start[k - 1].ends_sentence && said_end[k - 1].ends_sentence;
        let enough = whole || (mid_sentence && field_words.len() >= 2);
        // A shorter overlap would match fewer words: the longest decides.
        if !enough || !adds_a_word || fuzzy * 3 > k + 1 || fuzzy == k {
            return None;
        }
        return Some(text[..said_end[0].start].trim_end().len());
    }
    None
}

/// `text` without the words at its end that repeat the words right after
/// the caret. `before` and `after` are the field on each side of the caret
/// (`after` up to the end of its sentence at least), `None` where
/// unreadable; then `text` is returned whole.
pub fn without_repeat<'a>(before: Option<&str>, text: &'a str, after: Option<&str>) -> &'a str {
    match kept(before, text, after) {
        Some(keep) => &text[..keep],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::Context;

    /// The whole field after dictating `text` at the caret between `before`
    /// and `after`, with the repeat dropped and the join fitted.
    fn dictated(before: Option<&str>, text: &str, after: Option<&str>) -> String {
        let context = Context {
            before: before.map(Into::into),
            after: after.map(Into::into),
        };
        let fitted = context.fit(without_repeat(before, text, after));
        format!("{}{fitted}{}", before.unwrap_or(""), after.unwrap_or(""))
    }

    fn trims(before: &str, text: &str, after: &str, want: &str) {
        let trimmed = without_repeat(Some(before), text, Some(after));
        assert_ne!(
            trimmed, text,
            "expected a trim: {before:?} + {text:?} + {after:?}"
        );
        assert_eq!(dictated(Some(before), text, Some(after)), want);
    }

    fn keeps(before: &str, text: &str, after: &str, want: &str) {
        assert_eq!(
            without_repeat(Some(before), text, Some(after)),
            text,
            "expected no trim: {before:?} + {text:?} + {after:?}"
        );
        assert_eq!(dictated(Some(before), text, Some(after)), want);
    }

    #[test]
    fn drops_the_repeat_whichever_side_of_the_space_the_caret_is() {
        trims(
            "Let's meet ",
            "on Friday at noon tomorrow.",
            "at noon tomorrow.",
            "Let's meet on Friday at noon tomorrow.",
        );
        trims(
            "Let's meet",
            "on Friday at noon tomorrow.",
            " at noon tomorrow.",
            "Let's meet on Friday at noon tomorrow.",
        );
    }

    #[test]
    fn keeps_the_fields_case_and_punctuation() {
        trims(
            "Let's meet ",
            "on Friday At Noon tomorrow.",
            "at noon tomorrow.",
            "Let's meet on Friday at noon tomorrow.",
        );
        trims(
            "Can we meet ",
            "on Friday at noon tomorrow?",
            "at noon tomorrow.",
            "Can we meet on Friday at noon tomorrow.",
        );
        trims(
            "Let's meet ",
            "on Friday at noon tomorrow.",
            "at noon, tomorrow.",
            "Let's meet on Friday at noon, tomorrow.",
        );
    }

    #[test]
    fn the_fields_sentence_may_go_on() {
        trims(
            "We ship ",
            "on Friday at noon tomorrow.",
            "at noon tomorrow and celebrate.",
            "We ship on Friday at noon tomorrow and celebrate.",
        );
    }

    #[test]
    fn the_dictation_may_stop_partway_through_the_fields_words() {
        trims(
            "Let's meet ",
            "on Friday at noon.",
            "at noon tomorrow.",
            "Let's meet on Friday at noon tomorrow.",
        );
    }

    #[test]
    fn one_repeated_word_is_kept() {
        keeps(
            "Let's meet ",
            "on Friday at.",
            "at noon tomorrow.",
            "Let's meet on Friday at at noon tomorrow.",
        );
        keeps(
            "He said ",
            "that.",
            "that was fine.",
            "He said that that was fine.",
        );
        keeps(
            "By then she ",
            "had.",
            "had enough.",
            "By then she had had enough.",
        );
    }

    #[test]
    fn a_doubled_word_said_again_is_dropped() {
        trims(
            "By then she ",
            "said she had had.",
            "had had enough.",
            "By then she said she had had enough.",
        );
    }

    #[test]
    fn stops_at_the_end_of_the_fields_sentence() {
        keeps(
            "Let's meet ",
            "on Friday at noon tomorrow.",
            "at noon. Tomorrow we ship.",
            "Let's meet on Friday at noon tomorrow at noon. Tomorrow we ship.",
        );
        trims(
            "Let's meet ",
            "on Friday at noon.",
            "at noon. Tomorrow we ship.",
            "Let's meet on Friday at noon. Tomorrow we ship.",
        );
        trims(
            "Let's meet ",
            "on Friday at noon.",
            "at noon\nNext item",
            "Let's meet on Friday at noon\nNext item",
        );
    }

    #[test]
    fn never_crosses_a_sentence_end_in_the_dictation() {
        keeps(
            "Let's meet ",
            "on Friday at noon. Tomorrow.",
            "at noon tomorrow.",
            "Let's meet on Friday at noon. Tomorrow at noon tomorrow.",
        );
    }

    #[test]
    fn only_mid_sentence() {
        keeps(
            "Done. ",
            "We meet at noon tomorrow.",
            "At noon tomorrow we ship.",
            "Done. We meet at noon tomorrow. At noon tomorrow we ship.",
        );
    }

    #[test]
    fn a_repeat_of_the_whole_rest_of_the_sentence_is_dropped() {
        // One word, after a sentence end: the dictation was said through it.
        trims(
            "Thanks. ",
            "See you on Tuesday, Morgan.",
            "Morgan.",
            "Thanks. See you on Tuesday, Morgan.",
        );
        trims(
            "",
            "We meet at noon tomorrow.",
            "at noon tomorrow.",
            "We meet at noon tomorrow.",
        );
        trims(
            "He said ",
            "it's done, thanks.",
            "thanks.",
            "He said it's done, thanks.",
        );
        // The dictation goes on past the word, or the field's sentence does.
        keeps(
            "Thanks. ",
            "See you, Morgan",
            "Morgan.",
            "Thanks. See you, Morgan Morgan.",
        );
        keeps(
            "Thanks. ",
            "See you, Morgan.",
            "Morgan will call.",
            "Thanks. See you, Morgan. Morgan will call.",
        );
    }

    #[test]
    fn the_repeat_must_start_right_at_the_caret() {
        keeps(
            "Let's meet ",
            "on Friday at noon.",
            "tomorrow at noon.",
            "Let's meet on Friday at noon tomorrow at noon.",
        );
        keeps(
            "Let's meet ",
            "on Friday at noon tomorrow.",
            "sharp at noon tomorrow.",
            "Let's meet on Friday at noon tomorrow sharp at noon tomorrow.",
        );
        keeps(
            "Let's meet",
            "on Friday, at noon tomorrow.",
            ", at noon tomorrow.",
            "Let's meet on Friday, at noon tomorrow, at noon tomorrow.",
        );
    }

    #[test]
    fn one_misspelled_name_in_three_words_matches() {
        trims(
            "I talked to ",
            "Sarah and Meghan about the budget.",
            "Megan about the budget.",
            "I talked to Sarah and Megan about the budget.",
        );
        trims(
            "Please ask ",
            "Tom and Meghan today.",
            "Megan today.",
            "Please ask Tom and Megan today.",
        );
        trims(
            "I think ",
            "honestly the naive plan works.",
            "na\u{ef}ve plan works.",
            "I think honestly the na\u{ef}ve plan works.",
        );
    }

    #[test]
    fn near_words_that_differ_too_much_are_kept() {
        keeps(
            "Please ask ",
            "Tom and Meghan Brown.",
            "Megan Browne.",
            "Please ask Tom and Meghan Brown. Megan Browne.",
        );
        keeps(
            "Let's meet ",
            "on Friday at noon tomorrow.",
            "at midday tomorrow.",
            "Let's meet on Friday at noon tomorrow at midday tomorrow.",
        );
        keeps(
            "Please send the ",
            "files from today.",
            "form today.",
            "Please send the files from today form today.",
        );
        keeps(
            "I ",
            "really don't need it.",
            "do not need it.",
            "I really don't need it do not need it.",
        );
    }

    #[test]
    fn apostrophes_match_either_way() {
        trims(
            "I ",
            "really don't need it.",
            "don\u{2019}t need it.",
            "I really don\u{2019}t need it.",
        );
    }

    #[test]
    fn times_match_however_they_are_written() {
        trims(
            "Let's meet ",
            "on Friday at 3pm tomorrow.",
            "at 3 pm tomorrow.",
            "Let's meet on Friday at 3 pm tomorrow.",
        );
        trims(
            "Let's meet ",
            "on Friday at 3 p.m. tomorrow.",
            "at 3pm tomorrow.",
            "Let's meet on Friday at 3pm tomorrow.",
        );
        keeps(
            "Let's meet ",
            "on Friday at 3 tomorrow.",
            "at three tomorrow.",
            "Let's meet on Friday at 3 tomorrow at three tomorrow.",
        );
    }

    #[test]
    fn numbers_are_never_near_matches() {
        keeps(
            "Go to ",
            "the lobby and room 1235 now.",
            "room 1234 now.",
            "Go to the lobby and room 1235 now room 1234 now.",
        );
    }

    #[test]
    fn a_hyphen_matches_a_space() {
        trims(
            "Set up ",
            "a quick follow up call.",
            "follow-up call.",
            "Set up a quick follow-up call.",
        );
    }

    #[test]
    fn emoji_match_exactly() {
        trims(
            "Great job, ",
            "everyone \u{1F389} see you.",
            "\u{1F389} see you at noon.",
            "Great job, everyone \u{1F389} see you at noon.",
        );
        trims(
            "Nice work ",
            "and we will see you at noon.",
            "see you at noon \u{1F389} bye",
            "Nice work and we will see you at noon \u{1F389} bye",
        );
        keeps(
            "Great job, ",
            "everyone \u{1F469}\u{200D}\u{1F4BB} see you.",
            "\u{1F468}\u{200D}\u{1F4BB} see you.",
            "Great job, everyone \u{1F469}\u{200D}\u{1F4BB} see you. \u{1F468}\u{200D}\u{1F4BB} see you.",
        );
        // An emoji before the caret doesn't continue a sentence.
        keeps(
            "Nice \u{1F44D} ",
            "we will see you at noon.",
            "see you at noon \u{1F389} bye",
            "Nice \u{1F44D} we will see you at noon see you at noon \u{1F389} bye",
        );
    }

    #[test]
    fn a_caret_inside_a_word_trims_nothing() {
        keeps(
            "Let's meet a",
            "on Friday at noon.",
            "t noon tomorrow.",
            "Let's meet a on Friday at noon t noon tomorrow.",
        );
    }

    #[test]
    fn an_unreadable_side_trims_nothing() {
        let text = "on Friday at noon tomorrow.";
        assert_eq!(without_repeat(None, text, Some("at noon tomorrow.")), text);
        assert_eq!(
            dictated(None, text, Some("at noon tomorrow.")),
            "on Friday at noon tomorrow at noon tomorrow."
        );
        assert_eq!(without_repeat(Some("Let's meet "), text, None), text);
        assert_eq!(
            dictated(Some("Let's meet "), text, None),
            "Let's meet on Friday at noon tomorrow."
        );
    }

    #[test]
    fn the_dictation_must_add_a_word() {
        keeps(
            "Let's meet ",
            "at noon tomorrow.",
            "at noon tomorrow.",
            "Let's meet at noon tomorrow at noon tomorrow.",
        );
    }

    #[test]
    fn common_words_and_names_match_alike() {
        trims(
            "I talked to ",
            "Tom and Sarah yesterday.",
            "Sarah yesterday.",
            "I talked to Tom and Sarah yesterday.",
        );
        trims(
            "I sent it ",
            "back to the.",
            "to the team.",
            "I sent it back to the team.",
        );
        trims(
            "When you're done, ",
            "review the draft and send it.",
            "send it to me.",
            "When you're done, review the draft and send it to me.",
        );
    }

    /// Known limits of a general rule, kept so a change to them is deliberate.
    #[test]
    fn known_limits() {
        // Emphasis said twice reads as a repeat.
        trims(
            "He said ",
            "no way, no way.",
            "no way.",
            "He said no way, no way.",
        );
        // A chant that would leave nothing new is kept.
        keeps(
            "And then ",
            "bye bye.",
            "bye bye everyone.",
            "And then bye bye bye bye everyone.",
        );
        // A list item that ends like the next one.
        trims(
            "Invite ",
            "Mary Smith, John Smith.",
            "John Smith and Jane Doe.",
            "Invite Mary Smith, John Smith and Jane Doe.",
        );
        // An abbreviation's period ends the field's sentence early.
        keeps(
            "Please call ",
            "the office and Dr. Smith.",
            "Dr. Smith today.",
            "Please call the office and Dr. Smith. Dr. Smith today.",
        );
    }

    #[test]
    fn one_edit_apart_counts_one_change() {
        assert!(one_edit_apart("meghan", "megan"));
        assert!(one_edit_apart("megan", "meghan"));
        assert!(one_edit_apart("browne", "brown"));
        assert!(one_edit_apart("naive", "na\u{ef}ve"));
        assert!(one_edit_apart("same", "same"));
        assert!(!one_edit_apart("midday", "noon"));
        assert!(!one_edit_apart("abcde", "badce"));
        assert!(!one_edit_apart("meghan", "megann"));
    }
}
