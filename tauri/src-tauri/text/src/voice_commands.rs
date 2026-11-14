//! Spoken commands: words in a dictation that are an instruction, not text.
//!
//! "Paste from clipboard" becomes a marker in the transcript, wherever in the
//! dictation it was said. Cleanup keeps it like any other word, and the
//! content check rejects a cleanup that loses it, so the transcript is used
//! instead. The client replaces the marker with what is on the clipboard.

use crate::py::{Re, I};
use crate::re;

pub const CLIPBOARD: &str = "[clipboard]";

fn marker() -> &'static Re {
    re!(r"\[clipboard\]", I)
}

/// `text` with each spoken command replaced by its marker.
pub fn mark_commands(text: &str) -> String {
    // Whisper spells the verb by its sound: "paste", "Pace", "pays", "paced".
    // Any short p- or b- word with an s sound before "from clipboard" is
    // taken as it: nothing else is said that way.
    re!(r"\b[pb][aeiy]{1,2}[scz]\w*,?\s+from\s+(?:the\s+|my\s+)?clipboard\b", I).replace(text, CLIPBOARD)
}

pub fn count_commands(text: &str) -> usize {
    marker().findall(text).len()
}

/// The markers, when `text` says nothing but commands: nothing to clean.
pub fn commands_alone(text: &str) -> Option<String> {
    let count = count_commands(text);
    if count > 0 && !re!(r"\w").is_match(&marker().replace(text, "")) {
        return Some(vec![CLIPBOARD; count].join(" "));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_paste_from_clipboard() {
        assert_eq!(mark_commands("Pace from clipboard please"), "[clipboard] please");
        assert_eq!(commands_alone("[clipboard]."), Some("[clipboard]".into()));
    }
}
