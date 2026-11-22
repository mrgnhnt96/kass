//! Fitting dictated text to the text around the caret
//! (docs/plans/MID_SENTENCE_DICTATION.md).
//!
//! Refinement writes every take as if it were a whole message. When the caret
//! sits next to existing text, the join needs a space or loses a doubled one,
//! and a sentence that carries on after the caret must not get a period in
//! its middle. These rules touch only the edges of the text and are pure, so
//! every insertion method applies the same ones.

/// The text on each side of the caret (or of the selection the take
/// replaces). `None` means that side could not be read, and is left alone.
/// `Some("")` means the field starts or ends there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Closing punctuation that ends a word, so a following word needs a space,
/// and that attaches to the previous word, so it never takes a space itself.
const CLOSING: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '%', '\u{201D}', '\u{2019}', '\u{2026}',
];

/// Punctuation that a dictated final period would double up against.
const FOLLOWING_PUNCTUATION: &[char] = &['.', ',', ';', ':', '!', '?'];

impl Context {
    /// `text` with its leading edge fitted to [`Context::before`]. Only the
    /// start changes, so a longer text always fits to a longer result: live
    /// text can grow through it.
    pub fn lead(&self, text: &str) -> String {
        let Some(before) = self.before.as_deref() else {
            return text.to_string();
        };
        let Some(first) = text.chars().next() else {
            return String::new();
        };
        match before.chars().last() {
            Some(last) if last.is_whitespace() => text.trim_start_matches([' ', '\t']).to_string(),
            Some(_) if ends_word(before) && !first.is_whitespace() && !CLOSING.contains(&first) => {
                format!(" {text}")
            }
            _ => text.to_string(),
        }
    }

    /// `text` fitted to both sides: [`Context::lead`], then the trailing edge
    /// fitted to [`Context::after`].
    pub fn fit(&self, text: &str) -> String {
        let text = self.lead(text);
        let Some(after) = self.after.as_deref() else {
            return text;
        };
        let Some(next) = after.chars().next() else {
            return text;
        };
        let continues = after
            .trim_start_matches([' ', '\t'])
            .chars()
            .next()
            .is_some_and(|c| c.is_lowercase() || c.is_ascii_digit());
        let mut text =
            if (continues || FOLLOWING_PUNCTUATION.contains(&next)) && ends_in_lone_period(&text) {
                text[..text.len() - 1].to_string()
            } else {
                text
            };
        if next.is_whitespace() {
            let trimmed = text.trim_end_matches([' ', '\t']).len();
            text.truncate(trimmed);
        } else if ends_word(&text) && starts_word(after) {
            text.push(' ');
        }
        text
    }
}

/// Opening quotes, which attach to the next word.
const OPENING_QUOTES: &[char] = &['\u{201C}', '\u{2018}'];

/// Whether `text` ends a word, so the next word needs a space before it.
/// Letters, digits, closing punctuation and other non-ASCII symbols (emoji)
/// do; ASCII symbols such as `/`, `@`, `(` or `-` join to what follows.
fn ends_word(text: &str) -> bool {
    let mut chars = text.chars().rev();
    match chars.next() {
        Some(c) if c.is_alphanumeric() || CLOSING.contains(&c) => true,
        Some(c) if !c.is_ascii() && !c.is_whitespace() && !OPENING_QUOTES.contains(&c) => true,
        // A straight quote closes a quotation when it follows a character.
        Some('"' | '\'') => chars.next().is_some_and(|c| !c.is_whitespace()),
        _ => false,
    }
}

/// Whether `text` starts a word that needs a space after the previous one.
fn starts_word(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !CLOSING.contains(&c))
}

/// Whether `text` ends in exactly one period, not an ellipsis.
fn ends_in_lone_period(text: &str) -> bool {
    text.ends_with('.') && !text.ends_with("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(before: &str, after: &str) -> Context {
        Context {
            before: Some(before.into()),
            after: Some(after.into()),
        }
    }

    /// The whole field after inserting `text` between `before` and `after`.
    fn joined(before: &str, text: &str, after: &str) -> String {
        format!("{before}{}{after}", ctx(before, after).fit(text))
    }

    #[test]
    fn unknown_context_leaves_text_alone() {
        let text = "Move it.";
        assert_eq!(Context::default().fit(text), text);
    }

    #[test]
    fn empty_field_leaves_text_alone() {
        assert_eq!(joined("", "Move the meeting.", ""), "Move the meeting.");
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(ctx("word", "word").fit(""), "");
    }

    #[test]
    fn adds_a_space_after_a_word() {
        assert_eq!(
            joined("I think we should", "move it.", ""),
            "I think we should move it."
        );
    }

    #[test]
    fn adds_a_space_after_a_sentence() {
        assert_eq!(
            joined("Done.", "Next we ship it.", ""),
            "Done. Next we ship it."
        );
        assert_eq!(joined("Really?", "Yes.", ""), "Really? Yes.");
    }

    #[test]
    fn adds_a_space_after_a_closing_quote() {
        assert_eq!(
            joined("He said \"hi\"", "and left.", ""),
            "He said \"hi\" and left."
        );
        assert_eq!(
            joined("He said \u{201C}hi\u{201D}", "and left.", ""),
            "He said \u{201C}hi\u{201D} and left."
        );
    }

    #[test]
    fn no_space_after_an_opening_bracket_or_quote() {
        assert_eq!(joined("(", "see below)", ""), "(see below)");
        assert_eq!(joined("said \"", "hello", ""), "said \"hello");
        assert_eq!(joined("said \u{201C}", "hello", ""), "said \u{201C}hello");
    }

    #[test]
    fn no_space_before_punctuation() {
        assert_eq!(joined("Hello", ", everyone.", ""), "Hello, everyone.");
    }

    #[test]
    fn no_space_after_a_path_or_mention_marker() {
        assert_eq!(joined("docs/", "plans", ""), "docs/plans");
        assert_eq!(joined("@", "morgan", ""), "@morgan");
    }

    #[test]
    fn drops_a_doubled_space_at_the_start() {
        assert_eq!(joined("I think ", " we should.", ""), "I think we should.");
    }

    #[test]
    fn keeps_a_leading_line_break() {
        assert_eq!(joined("Intro. ", "\n- One", ""), "Intro. \n- One");
    }

    #[test]
    fn caret_after_a_line_break_needs_no_space() {
        assert_eq!(joined("Hi,\n", "Thanks.", ""), "Hi,\nThanks.");
    }

    #[test]
    fn drops_the_period_when_the_sentence_continues() {
        assert_eq!(
            joined("Can you ", "send the report.", " before lunch?"),
            "Can you send the report before lunch?"
        );
    }

    #[test]
    fn adds_a_space_before_following_words() {
        assert_eq!(
            joined("Can you ", "send the report.", "before lunch?"),
            "Can you send the report before lunch?"
        );
    }

    #[test]
    fn drops_the_period_before_following_punctuation() {
        assert_eq!(
            joined("We need ", "more time.", ", honestly."),
            "We need more time, honestly."
        );
        assert_eq!(joined("It is ", "done.", "."), "It is done.");
    }

    #[test]
    fn keeps_the_period_before_a_new_sentence() {
        assert_eq!(
            joined("", "First point.", " Second point."),
            "First point. Second point."
        );
        assert_eq!(
            joined("", "First point.", "Second point."),
            "First point. Second point."
        );
    }

    #[test]
    fn keeps_questions_and_exclamations() {
        assert_eq!(joined("", "Why not?", " we said"), "Why not? we said");
    }

    #[test]
    fn keeps_an_ellipsis() {
        assert_eq!(joined("", "Well...", " maybe"), "Well... maybe");
    }

    #[test]
    fn drops_a_doubled_space_at_the_end() {
        assert_eq!(joined("", "Move it ", " now"), "Move it now");
    }

    #[test]
    fn end_of_field_keeps_the_ending() {
        assert_eq!(joined("We should ", "move it.", ""), "We should move it.");
    }

    #[test]
    fn caret_before_a_line_break_keeps_the_ending() {
        assert_eq!(joined("", "Move it.", "\nNext line"), "Move it.\nNext line");
    }

    #[test]
    fn counts_emoji_as_words() {
        assert_eq!(joined("Nice 👍", "thanks.", ""), "Nice 👍 thanks.");
        assert_eq!(joined("Nice", "👍", ""), "Nice 👍");
    }

    #[test]
    fn lead_never_changes_what_follows_the_first_character() {
        let c = ctx("word", "more");
        let short = c.lead("The first");
        let long = c.lead("The first part.");
        assert!(long.starts_with(&short));
        assert_eq!(long, " The first part.");
    }
}
