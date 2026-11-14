//! Python's `re` and `str` behaviour, where Rust's differs and the stages
//! depend on it.
//!
//! The text stages were written in Python; to give the same results the
//! ports match on Python's terms: `$` also matches before a final newline,
//! `match` is anchored at a position but still sees the text before it,
//! empty matches are found the way `re.finditer` finds them, `isupper` needs
//! a cased letter, and so on.

use std::sync::OnceLock;

use fancy_regex::{Captures, Regex, RegexBuilder};

/// A compiled pattern, used the way Python's `re.Pattern` is.
pub struct Re {
    pattern: String,
    search: Regex,
    anchored: OnceLock<Regex>,
    full: OnceLock<Regex>,
}

/// Python's flags that the stages use.
pub const I: u8 = 1;
pub const S: u8 = 2;

fn build(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .backtrack_limit(100_000_000)
        .build()
        .unwrap_or_else(|error| panic!("bad pattern {pattern:?}: {error}"))
}

/// `pattern` (Python syntax) in the syntax fancy-regex reads.
///
/// `$` becomes "end, or before a final newline", `\Z` becomes `\z`, and in a
/// character class `[`, `&` and `~` are literal as they are in Python.
fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    let mut class_start = false;
    while let Some(c) = chars.next() {
        let at_start = class_start;
        class_start = false;
        match c {
            '\\' => {
                let next = chars.next().expect("pattern ends in a backslash");
                if next == 'Z' && !in_class {
                    out.push_str(r"\z");
                } else {
                    out.push('\\');
                    out.push(next);
                }
            }
            '[' if !in_class => {
                in_class = true;
                class_start = true;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    out.push('^');
                    chars.next();
                }
            }
            ']' if in_class && !at_start => {
                in_class = false;
                out.push(']');
            }
            '[' | '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
            }
            '-' if in_class && out.ends_with('-') && !out.ends_with("\\-") => out.push_str(r"\-"),
            '$' if !in_class => out.push_str(r"(?:\z|(?=\n\z))"),
            _ => out.push(c),
        }
    }
    out
}

impl Re {
    pub fn new(pattern: &str) -> Re {
        Re::with(pattern, 0)
    }

    /// Case-insensitive, like `re.IGNORECASE`.
    pub fn i(pattern: &str) -> Re {
        Re::with(pattern, I)
    }

    pub fn with(pattern: &str, flags: u8) -> Re {
        let mut prefix = String::new();
        if flags & I != 0 {
            prefix.push_str("(?i)");
        }
        if flags & S != 0 {
            prefix.push_str("(?s)");
        }
        let pattern = format!("{prefix}{}", translate(pattern));
        Re {
            search: build(&pattern),
            pattern,
            anchored: OnceLock::new(),
            full: OnceLock::new(),
        }
    }

    /// `re.search`: the first match anywhere.
    pub fn search<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        self.search_from(text, 0)
    }

    /// `pattern.search(text, pos)`: the first match at or after `pos`.
    pub fn search_from<'t>(&self, text: &'t str, pos: usize) -> Option<Captures<'t>> {
        self.search.captures_from_pos(text, pos).expect("regex failed")
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.search(text).is_some()
    }

    /// `re.match`: a match starting at the start of `text`.
    pub fn match_<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        self.match_at(text, 0)
    }

    /// `pattern.match(text, pos)`: a match starting at `pos`; lookbehinds and
    /// `\b` still see the text before it.
    pub fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<Captures<'t>> {
        let anchored = self
            .anchored
            .get_or_init(|| build(&format!(r"\G(?:{})", self.pattern)));
        anchored.captures_from_pos(text, pos).expect("regex failed")
    }

    /// `re.fullmatch`.
    pub fn fullmatch<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        let full = self
            .full
            .get_or_init(|| build(&format!(r"\A(?:{})\z", self.pattern)));
        full.captures(text).expect("regex failed")
    }

    /// `re.finditer`, with Python's handling of empty matches.
    pub fn finditer<'t>(&self, text: &'t str) -> Vec<Captures<'t>> {
        self.finditer_from(text, 0)
    }

    /// `pattern.finditer(text, pos)`.
    pub fn finditer_from<'t>(&self, text: &'t str, pos: usize) -> Vec<Captures<'t>> {
        let mut found = Vec::new();
        let mut pos = pos;
        let mut last_empty: Option<usize> = None;
        while pos <= text.len() {
            let Some(caps) = self.search_from(text, pos) else { break };
            let whole = caps.get(0).unwrap();
            if whole.start() == whole.end() && last_empty == Some(whole.start()) {
                match next_boundary(text, whole.start()) {
                    Some(next) => {
                        pos = next;
                        continue;
                    }
                    None => break,
                }
            }
            let (start, end) = (whole.start(), whole.end());
            found.push(caps);
            last_empty = (start == end).then_some(end);
            pos = end;
        }
        found
    }

    /// `re.findall` for a pattern without groups: the matched texts.
    pub fn findall<'t>(&self, text: &'t str) -> Vec<&'t str> {
        self.finditer(text)
            .into_iter()
            .map(|caps| caps.get(0).unwrap().as_str())
            .collect()
    }

    /// `re.sub` with a function.
    pub fn sub(&self, text: &str, replace: impl FnMut(&Captures) -> String) -> String {
        self.subn(text, 0, replace)
    }

    /// `re.sub(..., count=count)`; 0 replaces every match.
    pub fn subn(&self, text: &str, count: usize, mut replace: impl FnMut(&Captures) -> String) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (index, caps) in self.finditer(text).into_iter().enumerate() {
            if count != 0 && index >= count {
                break;
            }
            let whole = caps.get(0).unwrap();
            out.push_str(&text[last..whole.start()]);
            out.push_str(&replace(&caps));
            last = whole.end();
        }
        out.push_str(&text[last..]);
        out
    }

    /// `re.sub` with a fixed replacement (no group references).
    pub fn replace(&self, text: &str, with: &str) -> String {
        self.sub(text, |_| with.to_string())
    }

    /// `re.split`: the pieces between matches, with any groups' texts between
    /// them (a group that took no part gives `None`). `maxsplit` 0 splits at
    /// every match.
    pub fn split_groups<'t>(&self, text: &'t str, maxsplit: usize) -> Vec<Option<&'t str>> {
        let mut out = Vec::new();
        let mut last = 0;
        for (index, caps) in self.finditer(text).into_iter().enumerate() {
            if maxsplit != 0 && index >= maxsplit {
                break;
            }
            let whole = caps.get(0).unwrap();
            out.push(Some(&text[last..whole.start()]));
            for group in 1..caps.len() {
                out.push(caps.get(group).map(|m| m.as_str()));
            }
            last = whole.end();
        }
        out.push(Some(&text[last..]));
        out
    }

    /// `re.split` for a pattern whose groups always take part.
    pub fn split<'t>(&self, text: &'t str) -> Vec<&'t str> {
        self.splitn(text, 0)
    }

    pub fn splitn<'t>(&self, text: &'t str, maxsplit: usize) -> Vec<&'t str> {
        self.split_groups(text, maxsplit)
            .into_iter()
            .map(|part| part.unwrap_or(""))
            .collect()
    }
}

fn next_boundary(text: &str, at: usize) -> Option<usize> {
    text[at..].chars().next().map(|c| at + c.len_utf8())
}

/// `re.escape`.
pub fn escape(text: &str) -> String {
    fancy_regex::escape(text).into_owned()
}

/// What a match's group gave, the way Python's `match.group(n)` is used.
pub trait Groups<'t> {
    /// The group's text, or `""` when it took no part.
    fn g(&self, group: usize) -> &'t str;
    /// The group's text, or `None` when it took no part.
    fn opt(&self, group: usize) -> Option<&'t str>;
    /// A named group's text, or `""`.
    fn n(&self, name: &str) -> &'t str;
    fn named(&self, name: &str) -> Option<&'t str>;
    fn start(&self) -> usize;
    fn end(&self) -> usize;
    fn start_of(&self, group: usize) -> usize;
    fn end_of(&self, group: usize) -> usize;
    /// `match.group()`.
    fn text(&self) -> &'t str {
        self.g(0)
    }
}

impl<'t> Groups<'t> for Captures<'t> {
    fn g(&self, group: usize) -> &'t str {
        self.get(group).map_or("", |m| m.as_str())
    }
    fn opt(&self, group: usize) -> Option<&'t str> {
        self.get(group).map(|m| m.as_str())
    }
    fn n(&self, name: &str) -> &'t str {
        self.name(name).map_or("", |m| m.as_str())
    }
    fn named(&self, name: &str) -> Option<&'t str> {
        self.name(name).map(|m| m.as_str())
    }
    fn start(&self) -> usize {
        self.get(0).unwrap().start()
    }
    fn end(&self) -> usize {
        self.get(0).unwrap().end()
    }
    /// `match.start(n)`; -1 in Python when the group took no part, here the
    /// match's start.
    fn start_of(&self, group: usize) -> usize {
        self.get(group).map_or(usize::MAX, |m| m.start())
    }
    fn end_of(&self, group: usize) -> usize {
        self.get(group).map_or(usize::MAX, |m| m.end())
    }
}

/// A lazily compiled static pattern.
#[macro_export]
macro_rules! re {
    ($pattern:expr) => {{
        static RE: std::sync::LazyLock<$crate::py::Re> = std::sync::LazyLock::new(|| $crate::py::Re::new(&$pattern));
        &*RE
    }};
    ($pattern:expr, $flags:expr) => {{
        static RE: std::sync::LazyLock<$crate::py::Re> =
            std::sync::LazyLock::new(|| $crate::py::Re::with(&$pattern, $flags));
        &*RE
    }};
}

fn cased(c: char) -> bool {
    c.is_lowercase() || c.is_uppercase()
}

/// `str.isupper`: has a cased letter and none is lowercase.
pub fn isupper(text: &str) -> bool {
    text.chars().any(cased) && !text.chars().any(char::is_lowercase)
}

/// `str.islower`: has a cased letter and none is uppercase.
pub fn islower(text: &str) -> bool {
    text.chars().any(cased) && !text.chars().any(char::is_uppercase)
}

/// `str.isalpha` for one character.
pub fn is_alpha(c: char) -> bool {
    c.is_alphabetic()
}

/// `str.isdigit` for one character.
pub fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && re!(r"^\d$").is_match(c.encode_utf8(&mut [0; 4])))
}

/// `str.isalnum` for one character.
pub fn is_alnum(c: char) -> bool {
    c.is_alphanumeric()
}

/// Python's `\w` for one character.
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\x1c'..='\x1f')
}

/// `str.casefold`.
pub fn casefold(text: &str) -> String {
    caseless::default_case_fold_str(text)
}

/// `str.lower`.
pub fn lower(text: &str) -> String {
    text.to_lowercase()
}

/// `str.upper`.
pub fn upper(text: &str) -> String {
    text.to_uppercase()
}

/// `str.capitalize`: the first character in title case, the rest lowercase.
pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => title_char(first) + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn title_char(c: char) -> String {
    match c {
        'ǆ' | 'ǅ' | 'Ǆ' => "ǅ".into(),
        'ǉ' | 'ǈ' | 'Ǉ' => "ǈ".into(),
        'ǌ' | 'ǋ' | 'Ǌ' => "ǋ".into(),
        'ǳ' | 'ǲ' | 'Ǳ' => "ǲ".into(),
        _ => c.to_uppercase().collect(),
    }
}

/// `str.title`: each run of letters starts in title case, the rest lowercase.
pub fn title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_cased = false;
    for c in text.chars() {
        if previous_cased {
            out.extend(c.to_lowercase());
        } else {
            out.push_str(&title_char(c));
        }
        previous_cased = cased(c);
    }
    out
}

/// `str.split()`: runs of whitespace separate the words.
pub fn split(text: &str) -> Vec<&str> {
    text.split(is_space).filter(|part| !part.is_empty()).collect()
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

pub fn lstrip(text: &str) -> &str {
    text.trim_start_matches(is_space)
}

pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// `str.strip(chars)`.
pub fn strip_chars<'t>(text: &'t str, chars: &str) -> &'t str {
    text.trim_matches(|c| chars.contains(c))
}

pub fn lstrip_chars<'t>(text: &'t str, chars: &str) -> &'t str {
    text.trim_start_matches(|c| chars.contains(c))
}

pub fn rstrip_chars<'t>(text: &'t str, chars: &str) -> &'t str {
    text.trim_end_matches(|c| chars.contains(c))
}

/// `str.splitlines()`.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let breaks = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if breaks {
            lines.push(&text[start..index]);
            let mut end = index + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The number of characters, Python's `len(str)`.
pub fn len(text: &str) -> usize {
    text.chars().count()
}

/// `text[start:end]` by character, with Python's clamping (no negatives).
pub fn slice(text: &str, start: usize, end: usize) -> &str {
    let from = byte_at(text, start);
    let to = byte_at(text, end.max(start));
    &text[from..to]
}

/// `text[:n]` by character.
pub fn head(text: &str, n: usize) -> &str {
    &text[..byte_at(text, n)]
}

/// `text[n:]` by character.
pub fn tail_from(text: &str, n: usize) -> &str {
    &text[byte_at(text, n)..]
}

/// `text[-n:]` by character (`n` 0 gives the whole text, as in Python).
pub fn last(text: &str, n: usize) -> &str {
    if n == 0 {
        return text;
    }
    let count = len(text);
    &text[byte_at(text, count.saturating_sub(n))..]
}

/// `text[:-n]` by character (`n` 0 gives "", as in Python).
pub fn drop_last(text: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    let count = len(text);
    &text[..byte_at(text, count.saturating_sub(n))]
}

/// The byte offset of character `n` (the end when past it).
pub fn byte_at(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(index, _)| index)
}

/// The first character, Python's `text[:1]`, as a `char`.
pub fn first(text: &str) -> Option<char> {
    text.chars().next()
}

/// The last character, Python's `text[-1:]`, as a `char`.
pub fn final_char(text: &str) -> Option<char> {
    text.chars().next_back()
}

/// `text.count(part)` for a non-empty part.
pub fn count(text: &str, part: &str) -> usize {
    text.matches(part).count()
}

/// `a in b` for a single character given as text: Python's `c in "..."` is
/// true for `""`.
pub fn in_chars(c: Option<char>, chars: &str) -> bool {
    c.is_none_or(|c| chars.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollar_matches_before_a_final_newline() {
        let re = Re::new(r"end$");
        assert!(re.is_match("the end\n"));
        assert!(!re.is_match("the end\n\n"));
        assert!(Re::new(r"[\w$%]+").fullmatch("$5").is_some());
    }

    #[test]
    fn match_at_sees_the_text_before() {
        let re = Re::new(r"\bcat");
        assert!(re.match_at("a cat", 2).is_some());
        assert!(re.match_at("acat", 1).is_none());
        assert!(re.match_at("a cat", 0).is_none());
    }

    #[test]
    fn empty_matches_follow_python() {
        let re = Re::new(r"x*");
        assert_eq!(re.replace("abxd", "-"), "-a-b--d-");
        assert_eq!(Re::new(r"(\s+)").split("a b  c"), vec!["a", " ", "b", "  ", "c"]);
    }

    #[test]
    fn string_predicates() {
        assert!(isupper("ABC1"));
        assert!(!isupper("123"));
        assert!(islower("abc'"));
        assert_eq!(casefold("Straße"), "strasse");
        assert_eq!(capitalize("hELLO"), "Hello");
        assert_eq!(title("it's o'neil"), "It'S O'Neil");
        assert_eq!(splitlines("a\r\nb\n\nc\n"), vec!["a", "b", "", "c"]);
    }
}
