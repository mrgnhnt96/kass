//! Write a laugh as one word.
//!
//! Whisper writes each "ha" or "he" of a laugh as its own word ("ha ha ha",
//! "Ha, ha, ha.", "he he he"). A laugh is typed as one word, spelled the way
//! it was heard: "hahaha", "hehehe".

use crate::py::{self, Groups, Re, I};
use crate::re;

const SYLLABLES: &str = r"(?:ha|he)+";

/// Two or more laugh syllables (each may already be joined, "haha ha") with
/// only spaces, commas, periods or dashes between them.
fn laugh() -> &'static Re {
    re!(format!(r"(?<![\w-]){SYLLABLES}(?:[ \t,.\-]+{SYLLABLES})+(?![\w-])"), I)
}

/// Whether `text` is nothing but laugh syllables: "hahaha", "HeHe".
pub fn is_laughter(text: &str) -> bool {
    re!(SYLLABLES, I).fullmatch(text).is_some()
}

/// `text` with every spaced-out laugh written as one word ("hahaha").
pub fn join_laughter(text: &str) -> String {
    laugh().sub(text, |found| {
        let laugh = re!(r"[^a-zA-Z]").replace(found.text(), "");
        // "he" is also a word, and a restart repeats it ("he, he went home").
        // Only "he"s that run into another word must be three or more to be a laugh.
        if !py::lower(&laugh).contains('a') && laugh.len() < 6 && re!(r"[ \t]*\w").match_at(text, found.end()).is_some() {
            return found.text().to_string();
        }
        if py::isupper(&laugh) {
            return laugh;
        }
        format!("{}{}", &laugh[..1], py::lower(&laugh[1..]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_a_laugh() {
        assert_eq!(join_laughter("Ha, ha, ha. That's funny"), "Hahaha. That's funny");
        assert_eq!(join_laughter("he, he went home"), "he, he went home");
        assert!(is_laughter("HeHe"));
    }
}
