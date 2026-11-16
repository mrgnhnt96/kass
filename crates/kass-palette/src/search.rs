//! Finding captures and commands for what's typed in the palette.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use kass_api::types::CaptureResponse;
use std::ops::Range;

/// Most captures listed at once; the Captures screen has the full list.
pub const CAPTURE_LIMIT: usize = 6;

/// Characters of context kept before the first match in a long transcript.
const LEAD_CONTEXT: usize = 48;

/// A capture that matches, with the text its row shows.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureMatch {
    /// Where it is in the list searched.
    pub index: usize,
    /// The transcript, trimmed to start a little before the match.
    pub snippet: String,
}

/// The text a capture is known by: the refined version when there is one.
pub fn capture_text(capture: &CaptureResponse) -> &str {
    match capture.transcript_refined.as_deref() {
        Some(refined) if !refined.is_empty() => refined,
        _ => &capture.transcript_raw,
    }
}

fn lower(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Where `needle` (already lowercased) matches `text` at byte `start`,
/// ignoring case: the byte just past the match.
fn match_at(text: &str, start: usize, needle: &[char]) -> Option<usize> {
    let mut want = needle.iter();
    let mut left = needle.len();
    for (offset, c) in text[start..].char_indices() {
        for l in c.to_lowercase() {
            if want.next() != Some(&l) {
                return None;
            }
            left -= 1;
        }
        if left == 0 {
            return Some(start + offset + c.len_utf8());
        }
    }
    None
}

/// The first case-insensitive match of `needle` in `text` from byte `from`.
fn find(text: &str, needle: &[char], from: usize) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    text[from..]
        .char_indices()
        .find_map(|(offset, _)| match_at(text, from + offset, needle).map(|end| from + offset..end))
}

/// Captures whose refined or raw transcript contains `query`, in the
/// order given (newest first). An empty query returns the first ones. The
/// snippet starts a little before the match so it stays visible in a
/// two-line row.
pub fn search_captures(
    captures: &[CaptureResponse],
    query: &str,
    limit: usize,
) -> Vec<CaptureMatch> {
    let needle = lower(query);
    let mut matches = Vec::new();
    for (index, capture) in captures.iter().enumerate() {
        if matches.len() >= limit {
            break;
        }
        let shown = capture_text(capture);
        if needle.is_empty() {
            matches.push(CaptureMatch {
                index,
                snippet: shown.to_string(),
            });
            continue;
        }
        // Prefer the text the user sees; fall back to the raw transcript so
        // a word the refiner rewrote is still findable.
        let (text, at) = match find(shown, &needle, 0) {
            Some(at) => (shown, at.start),
            None => match find(&capture.transcript_raw, &needle, 0) {
                Some(at) => (capture.transcript_raw.as_str(), at.start),
                None => continue,
            },
        };
        let before = text[..at].chars().count();
        // Like `lastIndexOf(' ', at - LEAD_CONTEXT) + 1`: cut at a word.
        let start = if before > LEAD_CONTEXT {
            let (lead, c) = text
                .char_indices()
                .nth(before - LEAD_CONTEXT)
                .unwrap_or((at, ' '));
            text[..lead + c.len_utf8()]
                .rfind(' ')
                .map_or(0, |space| space + 1)
        } else {
            0
        };
        let snippet = if start > 0 {
            format!("…{}", &text[start..])
        } else {
            text.to_string()
        };
        matches.push(CaptureMatch { index, snippet });
    }
    matches
}

/// The byte ranges of `text` that match `query`, ignoring case.
pub fn match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle = lower(query);
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(hit) = find(text, &needle, from) {
        from = hit.end;
        ranges.push(hit);
    }
    ranges
}

/// `m:ss` for a capture's length, or None when it wasn't recorded.
pub fn format_duration(ms: Option<i64>) -> Option<String> {
    let total = (ms? as f64 / 1000.).round() as i64;
    Some(format!("{}:{:02}", total / 60, total % 60))
}

/// Server timestamps are naive UTC, so one without a zone is read as UTC.
fn parse_server_date(date: &str) -> Option<DateTime<Local>> {
    let trimmed = date.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(parsed.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()?;
    Some(Utc.from_utc_datetime(&naive).with_timezone(&Local))
}

/// "Sep 21, 2:14 PM".
pub fn format_date(created_at: &str) -> Option<String> {
    parse_server_date(created_at).map(|date| date.format("%b %-d, %-I:%M %p").to_string())
}

/// A capture row's meta line: when, from where and how long.
pub fn capture_meta(capture: &CaptureResponse) -> String {
    [
        format_date(&capture.created_at),
        Some(capture.source.as_str().to_string()),
        format_duration(capture.duration_ms),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

/// What the palette lists for `query`.
#[derive(Clone, Debug, PartialEq)]
pub struct Results {
    /// The query without a leading `>`, trimmed: what's highlighted.
    pub term: String,
    pub captures: Vec<CaptureMatch>,
    /// Indices into the commands searched.
    pub commands: Vec<usize>,
}

impl Results {
    /// A leading `>` lists only commands.
    pub fn of(query: &str, captures: &[CaptureResponse], commands: &[&str]) -> Self {
        let (commands_only, rest) = match query.strip_prefix('>') {
            Some(rest) => (true, rest),
            None => (false, query),
        };
        let term = rest.trim().to_string();
        let captures = if commands_only {
            Vec::new()
        } else {
            search_captures(captures, &term, CAPTURE_LIMIT)
        };
        let wanted = term.to_lowercase();
        let commands = commands
            .iter()
            .enumerate()
            .filter(|(_, label)| label.to_lowercase().contains(&wanted))
            .map(|(i, _)| i)
            .collect();
        Self {
            term,
            captures,
            commands,
        }
    }

    pub fn len(&self) -> usize {
        self.captures.len() + self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The row highlighted when `selected` was asked for: the last when
    /// the list got shorter.
    pub fn active(&self, selected: usize) -> usize {
        selected.min(self.len().saturating_sub(1))
    }
}

/// One step up or down from `active`, wrapping at the ends.
pub fn step(active: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (active + 1) % len
    } else {
        (active + len - 1) % len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(raw: &str, refined: Option<&str>) -> CaptureResponse {
        serde_json::from_value(serde_json::json!({
            "id": raw,
            "audio_path": "",
            "source": "dictation",
            "duration_ms": 61_400,
            "transcript_raw": raw,
            "transcript_refined": refined,
            "created_at": "2026-09-21T14:14:00",
        }))
        .unwrap()
    }

    #[test]
    fn an_empty_query_lists_the_newest() {
        let captures: Vec<_> = (0..8).map(|i| capture(&format!("c{i}"), None)).collect();
        let found = search_captures(&captures, "", 6);
        assert_eq!(found.len(), 6);
        assert_eq!(found[0].index, 0);
        assert_eq!(found[0].snippet, "c0");
    }

    #[test]
    fn matches_ignore_case_and_prefer_the_refined_text() {
        let captures = [
            capture("hello world", Some("Hello, World!")),
            capture("nothing here", None),
        ];
        let found = search_captures(&captures, "WORLD", 6);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].snippet, "Hello, World!");
    }

    #[test]
    fn a_word_the_refiner_rewrote_is_still_found() {
        let captures = [capture("gonna go", Some("Going to go."))];
        let found = search_captures(&captures, "gonna", 6);
        assert_eq!(found[0].snippet, "gonna go");
    }

    #[test]
    fn a_late_match_starts_the_snippet_at_a_word_before_it() {
        let lead = "word ".repeat(20);
        let text = format!("{lead}needle at the end");
        let found = search_captures(&[capture(&text, None)], "needle", 6);
        let snippet = &found[0].snippet;
        assert!(snippet.starts_with("…word "), "{snippet}");
        assert!(snippet.ends_with("needle at the end"));
        let kept = snippet.trim_start_matches('…').find("needle").unwrap();
        assert_eq!(kept, 50);
    }

    #[test]
    fn multibyte_text_never_splits_a_character() {
        let lead = "é ".repeat(40);
        let text = format!("{lead}café");
        let found = search_captures(&[capture(&text, None)], "CAFÉ", 6);
        assert!(found[0].snippet.ends_with("café"));
        assert_eq!(match_ranges("Café café", "café"), vec![0..5, 6..11]);
    }

    #[test]
    fn ranges_cover_each_match() {
        assert_eq!(match_ranges("abcABCab", "ab"), vec![0..2, 3..5, 6..8]);
        assert!(match_ranges("abc", "").is_empty());
        assert!(match_ranges("abc", "x").is_empty());
    }

    #[test]
    fn durations_read_as_minutes_and_seconds() {
        assert_eq!(format_duration(Some(61_400)).as_deref(), Some("1:01"));
        assert_eq!(format_duration(Some(0)).as_deref(), Some("0:00"));
        assert_eq!(format_duration(None), None);
    }

    #[test]
    fn meta_joins_date_source_and_length() {
        let meta = capture_meta(&capture("x", None));
        assert!(meta.ends_with(" · dictation · 1:01"), "{meta}");
    }

    #[test]
    fn a_leading_arrow_lists_only_commands() {
        let captures = [capture("go to models", None)];
        let commands = ["Go to Captures", "Go to Models", "Settings: General"];
        let all = Results::of("models", &captures, &commands);
        assert_eq!(all.captures.len(), 1);
        assert_eq!(all.commands, vec![1]);
        let only = Results::of("> Models ", &captures, &commands);
        assert!(only.captures.is_empty());
        assert_eq!(only.term, "Models");
        assert_eq!(only.commands, vec![1]);
        assert_eq!(Results::of(">", &captures, &commands).commands.len(), 3);
    }

    #[test]
    fn selection_wraps_and_stays_in_range() {
        assert_eq!(step(0, 3, false), 2);
        assert_eq!(step(2, 3, true), 0);
        assert_eq!(step(0, 0, true), 0);
        let results = Results::of(">captures", &[], &["Go to Captures"]);
        assert_eq!(results.active(5), 0);
    }
}
