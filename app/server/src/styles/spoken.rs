//! A dictation that opens by asking for a style by name ("use formal mode",
//! "make this more formal", "I want this to be personal") is written in that
//! style instead of its app's. Python: `spoken_style` in
//! `backend/services/styles.py`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use kass_text::py::{self, Re};

use super::{Snapshot, Style};

/// Words that ask for the same register: "make this professional" picks a
/// style called Formal, "more informal" one called Casual.
const SYNONYMS: [&[&str]; 2] = [
    &["formal", "professional"],
    &["casual", "informal", "relaxed"],
];
const FILLER: &str = r"(?:(?:um|uh|okay|ok|so|please)\W+)*";
const MORE: &str = r"(?:a\W+)?(?:little\W+)?(?:bit\W+)?(?:more\W+)?";
const MODE: &str = r"(?:\W+(?:mode|style))";
/// Before the text, after the ask: spaces, punctuation and dashes.
const AFTER: &str = "[\\s.,:;!?\\-\u{2013}\u{2014}\u{2026}]*";
/// A sentence break, or the end of the dictation.
const BREAK: &str = "(?=\\s*(?:[.,:;!?\u{2026}]|$))";

/// Ways to ask for a style at the start of a dictation, and whether a
/// sentence break must follow. "Use formal mode" is never text, so it needs
/// none; the rest could open a real sentence ("formal mode is off") unless
/// they stand alone.
fn lead_ins() -> [(String, bool); 5] {
    [
        (format!(r"use\W+(?:the\W+)?{{name}}{MODE}\b"), false),
        (
            format!(r"(?:switch|change)\W+(?:over\W+)?to\W+(?:the\W+)?{{name}}{MODE}?"),
            true,
        ),
        (format!(r"(?:in\W+)?{{name}}{MODE}"), true),
        (
            format!(
                r"(?:let'?s\W+)?(?:make|write)\W+(?:this|it)\W+(?:sound\W+)?{MORE}{{name}}{MODE}?"
            ),
            true,
        ),
        (
            format!(
                r"i\W*(?:want|would\W+like|d\W+like)\W+(?:this|it)\W+to\W+(?:be|sound)\W+{MORE}{{name}}{MODE}?"
            ),
            true,
        ),
    ]
}

/// A pattern for `name` as Whisper may write it, with its synonyms. A name
/// that already ends in "mode" or "style" isn't said twice.
fn spoken_names(name: &str) -> Option<String> {
    let mut words: Vec<String> = kass_text::re!(r"\w+")
        .findall(name)
        .into_iter()
        .map(py::casefold)
        .collect();
    if words.len() > 1 && matches!(words.last().map(String::as_str), Some("mode" | "style")) {
        words.pop();
    }
    if words.is_empty() {
        return None;
    }
    let variants: Vec<String> = words
        .iter()
        .map(|word| {
            let mut alike: Vec<&str> = SYNONYMS
                .iter()
                .find(|group| group.contains(&word.as_str()))
                .map(|group| group.to_vec())
                .unwrap_or_else(|| vec![word.as_str()]);
            alike.sort_unstable();
            let alternatives: Vec<String> = alike.iter().map(|each| py::escape(each)).collect();
            format!("(?:{})", alternatives.join("|"))
        })
        .collect();
    Some(variants.join(r"\W+"))
}

/// Compiled patterns by style name; styles are few and rarely renamed.
static PATTERNS: LazyLock<Mutex<HashMap<String, Option<Arc<Re>>>>> =
    LazyLock::new(Default::default);

fn spoken_pattern(name: &str) -> Option<Arc<Re>> {
    let mut cache = PATTERNS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = cache.get(name) {
        return found.clone();
    }
    let pattern = spoken_names(name).map(|said| {
        let lead_ins: Vec<String> = lead_ins()
            .iter()
            .map(|(lead_in, needs_break)| {
                let lead_in = lead_in.replace("{name}", &said);
                format!("(?:{lead_in}{})", if *needs_break { BREAK } else { "" })
            })
            .collect();
        Arc::new(Re::i(&format!(
            r"^\W*{FILLER}(?:{}){AFTER}",
            lead_ins.join("|")
        )))
    });
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(name.to_string(), pattern.clone());
    pattern
}

/// The style `text` asks for in its opening words, and the rest of `text`;
/// else `None` and `text` unchanged.
///
/// Longer names are tried first, so "use work email mode" isn't taken as a
/// style called Work.
pub fn spoken_style<'s>(text: &str, styles: &'s Snapshot) -> (Option<&'s Style>, String) {
    let mut by_length: Vec<&Style> = styles.styles.iter().collect();
    by_length.sort_by_key(|style| std::cmp::Reverse(py::len(&style.name)));
    for style in by_length {
        let Some(pattern) = spoken_pattern(&style.name) else {
            continue;
        };
        let Some(found) = pattern.match_(text) else {
            continue;
        };
        let end = found.get(0).map_or(0, |whole| whole.end());
        let rest = &text[end..];
        // "Use formal mode, dear team" starts the text at "dear".
        let first = py::split(rest).first().copied().unwrap_or("");
        if py::islower(first) {
            let mut chars = rest.chars();
            let head = chars
                .next()
                .map(|c| py::upper(&c.to_string()))
                .unwrap_or_default();
            return (Some(style), format!("{head}{}", chars.as_str()));
        }
        return (Some(style), rest.to_string());
    }
    (None, text.to_string())
}
