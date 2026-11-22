//! "Paste from clipboard", said anywhere in a dictation. The server replaces
//! the phrase with [`MARKER`] before cleanup; the clipboard goes where the
//! marker ended up.

/// Kept in step with `CLIPBOARD` in `backend/services/voice_commands.py`.
pub const MARKER: &str = "[clipboard]";

#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// No command: insert the text as usual.
    Text(String),
    /// Nothing but the command: paste the clipboard as it is, every format.
    Clipboard,
    /// The command among words: the text on each side of every marker, in order.
    Around(Vec<String>),
}

pub fn contains_marker(text: &str) -> bool {
    text.to_ascii_lowercase().contains(MARKER)
}

pub fn plan(text: &str) -> Plan {
    let parts = split(text);
    if parts.len() == 1 {
        return Plan::Text(text.to_string());
    }
    if parts
        .iter()
        .all(|part| part.chars().all(|c| !c.is_alphanumeric()))
    {
        return Plan::Clipboard;
    }
    Plan::Around(parts)
}

/// `parts` joined by the clipboard's text, its surrounding whitespace dropped:
/// it goes into a sentence.
pub fn fill(parts: &[String], clipboard: &str) -> String {
    parts.join(clipboard.trim())
}

/// The text between markers. Matches any case, and a marker the cleanup
/// formatted as code (`` `[clipboard]` ``) takes its backticks with it.
fn split(text: &str) -> Vec<String> {
    // ASCII lowercasing keeps every byte offset valid in `text`.
    let lower = text.to_ascii_lowercase();
    let mut parts = Vec::new();
    let mut start = 0;
    while let Some(found) = lower[start..].find(MARKER) {
        let at = start + found;
        let end = at + MARKER.len();
        let quoted = text[..at].ends_with('`') && text[end..].starts_with('`');
        let (at, end) = if quoted { (at - 1, end + 1) } else { (at, end) };
        parts.push(text[start..at].to_string());
        start = end;
    }
    parts.push(text[start..].to_string());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(parts: &[&str]) -> Plan {
        Plan::Around(parts.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn text_without_the_command_is_inserted_as_is() {
        assert_eq!(plan("Hello there."), Plan::Text("Hello there.".into()));
    }

    #[test]
    fn the_command_alone_pastes_the_clipboard_itself() {
        assert_eq!(plan("[clipboard]"), Plan::Clipboard);
        // Whisper's period, or a cleanup's, is not text to add.
        assert_eq!(plan("[clipboard]."), Plan::Clipboard);
        assert_eq!(plan(" `[clipboard]` "), Plan::Clipboard);
    }

    #[test]
    fn the_command_mid_sentence_splits_the_text_around_it() {
        assert_eq!(
            plan("Here's the link: [clipboard]. Let me know."),
            parts(&["Here's the link: ", ". Let me know."])
        );
        assert_eq!(
            plan("Try `[clipboard]` or [Clipboard]."),
            parts(&["Try ", " or ", "."])
        );
    }

    #[test]
    fn clipboard_text_fills_the_sentence() {
        let Plan::Around(around) = plan("Here's the link: [clipboard]. Let me know.") else {
            panic!("expected text around the command");
        };
        assert_eq!(
            fill(&around, "https://example.com\n"),
            "Here's the link: https://example.com. Let me know."
        );
    }

    #[test]
    fn non_ascii_text_splits_on_character_boundaries() {
        assert_eq!(plan("Café [clipboard] naïve"), parts(&["Café ", " naïve"]));
        assert!(contains_marker("é [Clipboard]"));
        assert!(!contains_marker("the clipboard"));
    }
}
