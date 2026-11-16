//! Inline markup in formatted strings (`<strong>…</strong>`, `<keys/>`).

/// A piece of a formatted string: plain text, or text wrapped in a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The text. Empty for a self-closing tag such as `<keys/>`, which marks
    /// where the UI inserts an element of its own.
    pub text: String,
    /// The tag name, or `None` for plain text.
    pub tag: Option<String>,
}

impl Run {
    fn plain(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            tag: None,
        }
    }
}

/// Splits a formatted string into runs of plain and tagged text.
///
/// Recognises `<tag>…</tag>` and `<tag/>` (or `<tag />`) for the names in
/// `tags`, the way React's `<Trans components={{ … }}>` did. Anything else —
/// a tag not in `tags`, an opening tag with no closing tag, a stray `<` —
/// stays in the text literally. Tags do not nest: the text of a tagged run is
/// everything up to its closing tag.
///
/// ```
/// use kass_i18n::{Run, parse_markup};
///
/// let runs = parse_markup("Delete <strong>Whisper</strong>?", &["strong"]);
/// assert_eq!(runs, vec![
///     Run { text: "Delete ".into(), tag: None },
///     Run { text: "Whisper".into(), tag: Some("strong".into()) },
///     Run { text: "?".into(), tag: None },
/// ]);
/// ```
pub fn parse_markup(text: &str, tags: &[&str]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut plain_start = 0;
    let mut pos = 0;
    while let Some(offset) = text[pos..].find('<') {
        let lt = pos + offset;
        match parse_tag(text, lt, tags) {
            Some((tag, inner, end)) => {
                if plain_start < lt {
                    runs.push(Run::plain(&text[plain_start..lt]));
                }
                runs.push(Run {
                    text: inner.to_owned(),
                    tag: Some(tag.to_owned()),
                });
                pos = end;
                plain_start = end;
            }
            None => pos = lt + 1,
        }
    }
    if plain_start < text.len() {
        runs.push(Run::plain(&text[plain_start..]));
    }
    runs
}

/// Parses a known tag starting at `lt` (which is a `<`). Returns the tag
/// name, its inner text and the byte offset just past the tag.
fn parse_tag<'a>(text: &'a str, lt: usize, tags: &[&str]) -> Option<(&'a str, &'a str, usize)> {
    let rest = &text[lt + 1..];
    let name_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..name_len];
    if name.is_empty() || !tags.contains(&name) {
        return None;
    }
    let after_name = lt + 1 + name_len;
    let tail = &text[after_name..];
    if tail.starts_with('>') {
        let open_end = after_name + 1;
        let close = format!("</{name}>");
        let close_at = open_end + text[open_end..].find(&close)?;
        Some((name, &text[open_end..close_at], close_at + close.len()))
    } else {
        let trimmed = tail.trim_start_matches(' ');
        trimmed
            .starts_with("/>")
            .then(|| (name, "", text.len() - trimmed.len() + 2))
    }
}
