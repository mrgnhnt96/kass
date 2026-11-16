//! How a capture's times, lengths and text read in the list and the detail.

use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone, Utc};
use kass_api::types::{CaptureResponse, CaptureSource};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// Kass's own bundle id, recorded on captures dictated into its window.
pub const KASS_BUNDLE_ID: &str = "com.mrgnhnt.kass";

/// The row tag: Review wins over Refined, since it asks the user to look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureTag {
    Refined,
    Review,
    Raw,
    Command,
    CommandFailed,
}

pub fn capture_tag(capture: &CaptureResponse) -> CaptureTag {
    let refined = capture
        .transcript_refined
        .as_deref()
        .is_some_and(|t| !t.is_empty());
    if capture.source == CaptureSource::Command {
        return if refined {
            CaptureTag::Command
        } else {
            CaptureTag::CommandFailed
        };
    }
    if capture.refinement_review.is_some() {
        return CaptureTag::Review;
    }
    if refined {
        CaptureTag::Refined
    } else {
        CaptureTag::Raw
    }
}

/// `m:ss`.
pub fn format_duration(ms: Option<i64>) -> String {
    let ms = ms.unwrap_or(0);
    if ms <= 0 {
        return "0:00".into();
    }
    let total = (ms as f64 / 1000.).round() as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Server timestamps are naive UTC, so one without a zone is read as UTC.
pub fn parse_server_date(date: &str) -> Option<DateTime<Local>> {
    let trimmed = date.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(parsed.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()?;
    Some(Utc.from_utc_datetime(&naive).with_timezone(&Local))
}

fn clock(date: &DateTime<Local>) -> String {
    date.format("%-I:%M %p").to_string()
}

fn is_yesterday(date: &DateTime<Local>, now: &DateTime<Local>) -> bool {
    now.date_naive()
        .pred_opt()
        .is_some_and(|day| day == date.date_naive())
}

/// Row time: "2:14 PM" today, "yest 6:21 PM" yesterday, "Sep 21" before that.
pub fn format_row_time(created_at: &str, yesterday: &str, now: DateTime<Local>) -> String {
    let Some(date) = parse_server_date(created_at) else {
        return String::new();
    };
    if date.date_naive() == now.date_naive() {
        clock(&date)
    } else if is_yesterday(&date, &now) {
        format!("{yesterday} {}", clock(&date))
    } else {
        date.format("%b %-d").to_string()
    }
}

/// "Today · 2:14 PM", "Yesterday · 6:21 PM", then "Sep 21 · 9:00 AM", with
/// the year when it isn't this one.
pub fn format_detail_stamp(
    created_at: &str,
    today: &str,
    yesterday: &str,
    now: DateTime<Local>,
) -> String {
    let Some(date) = parse_server_date(created_at) else {
        return String::new();
    };
    let time = clock(&date);
    if date.date_naive() == now.date_naive() {
        format!("{today} · {time}")
    } else if is_yesterday(&date, &now) {
        format!("{yesterday} · {time}")
    } else if date.year() == now.year() {
        format!("{} · {time}", date.format("%b %-d"))
    } else {
        format!("{} · {time}", date.format("%b %-d, %Y"))
    }
}

/// "Sep 21, 2:14 PM": when a correction was made or learning last ran.
pub fn format_when(created_at: &str) -> String {
    parse_server_date(created_at)
        .map(|date| date.format("%b %-d, %-I:%M %p").to_string())
        .unwrap_or_else(|| created_at.to_string())
}

/// Local clock time of a server timestamp: "2:14 PM".
pub fn format_time(created_at: &str) -> String {
    parse_server_date(created_at)
        .map(|date| clock(&date))
        .unwrap_or_default()
}

/// Speaking pace, or None when the recording has no length to measure.
pub fn words_per_minute(words: usize, duration_ms: Option<i64>) -> Option<u64> {
    let ms = duration_ms.filter(|&ms| ms > 0)?;
    if words == 0 {
        return None;
    }
    Some((words as f64 / (ms as f64 / 60_000.)).round() as u64)
}

/// "en" → "English"; the code itself when it isn't one Kass knows the name of.
pub fn language_name(code: &str) -> String {
    const NAMES: &[(&str, &str)] = &[
        ("ar", "Arabic"),
        ("cs", "Czech"),
        ("da", "Danish"),
        ("de", "German"),
        ("el", "Greek"),
        ("en", "English"),
        ("es", "Spanish"),
        ("fi", "Finnish"),
        ("fr", "French"),
        ("he", "Hebrew"),
        ("hi", "Hindi"),
        ("hu", "Hungarian"),
        ("id", "Indonesian"),
        ("it", "Italian"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("ms", "Malay"),
        ("nl", "Dutch"),
        ("no", "Norwegian"),
        ("pl", "Polish"),
        ("pt", "Portuguese"),
        ("ro", "Romanian"),
        ("ru", "Russian"),
        ("sk", "Slovak"),
        ("sv", "Swedish"),
        ("th", "Thai"),
        ("tr", "Turkish"),
        ("uk", "Ukrainian"),
        ("vi", "Vietnamese"),
        ("zh", "Chinese"),
    ];
    let base = code.split(['-', '_']).next().unwrap_or(code).to_lowercase();
    NAMES
        .iter()
        .find(|(c, _)| *c == base)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| code.to_string())
}

pub fn matches_search(capture: &CaptureResponse, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    [
        Some(capture.transcript_raw.as_str()),
        capture.transcript_refined.as_deref(),
        capture.command_selection.as_deref(),
        capture.app_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&q))
}

/// The text a capture delivers: the refined transcript when there is one.
pub fn delivered_text(capture: &CaptureResponse) -> &str {
    capture
        .transcript_refined
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&capture.transcript_raw)
}

/// The capture was dictated into a field in Kass's own window.
pub fn went_into_kass(capture: &CaptureResponse) -> bool {
    capture.app_bundle_id.as_deref() == Some(KASS_BUNDLE_ID)
}

/// A run of a row's snippet; `changed` marks a word the cleanup added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetPart {
    pub text: String,
    pub changed: bool,
}

/// Words as the content check counts them (backend/services/content_check.py).
static SNIPPET_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[+-]?\d+(?:[.,]\d+)*%?|[\p{L}\p{N}_'’./-]*[\p{L}\p{N}_]")
        .expect("the snippet word pattern is valid")
});

fn check_token(word: &str) -> String {
    let normalized: String = word
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .replace('’', "'");
    normalized
        .trim_matches(|c| matches!(c, '.' | '/' | '\'' | '-'))
        .to_string()
}

/// A change past this many characters would fall below a two-line row.
const VISIBLE_CHARS: usize = 70;
/// Words kept before a change that is pulled into view.
const LEAD_WORDS: usize = 6;

/// A row's snippet with the words the cleanup added marked. When the first
/// one would sit past the row's two lines, the snippet starts a few words
/// before it, after an ellipsis, so the change is always in view.
pub fn snippet_parts(text: &str, added: &[String]) -> Vec<SnippetPart> {
    let wanted: HashSet<String> = added.iter().map(|w| check_token(w)).collect();
    let words: Vec<regex::Match> = SNIPPET_WORD.find_iter(text).collect();
    let Some(first) = words
        .iter()
        .position(|m| wanted.contains(&check_token(m.as_str())))
    else {
        return vec![SnippetPart {
            text: text.to_string(),
            changed: false,
        }];
    };

    let mut start = 0;
    let mut prefix = "";
    if text[..words[first].start()].chars().count() > VISIBLE_CHARS && first > LEAD_WORDS {
        start = words[first - LEAD_WORDS].start();
        prefix = "…";
    }

    let mut parts = Vec::new();
    let mut cursor = start;
    for found in &words[first..] {
        if !wanted.contains(&check_token(found.as_str())) {
            continue;
        }
        if found.start() > cursor {
            parts.push(SnippetPart {
                text: text[cursor..found.start()].to_string(),
                changed: false,
            });
        }
        parts.push(SnippetPart {
            text: found.as_str().to_string(),
            changed: true,
        });
        cursor = found.end();
    }
    if cursor < text.len() {
        parts.push(SnippetPart {
            text: text[cursor..].to_string(),
            changed: false,
        });
    }
    if let Some(first) = parts.first_mut() {
        first.text.insert_str(0, prefix);
    }
    parts
}

/// A count with thousands separators: 12,345.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Time saved: "2h 21m", "47m", "0m".
pub fn format_saved(ms: i64) -> String {
    let minutes = (ms.unsigned_abs() as f64 / 60_000.).round() as u64;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h {}m", minutes / 60, minutes % 60)
    }
}

/// "Mon, Sep 28".
pub fn format_day(date: chrono::NaiveDate) -> String {
    date.format("%a, %b %-d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn server(date: DateTime<Local>) -> String {
        date.with_timezone(&Utc)
            .naive_utc()
            .format("%Y-%m-%dT%H:%M:%S%.6f")
            .to_string()
    }

    #[test]
    fn durations_read_as_minutes_and_seconds() {
        assert_eq!(format_duration(None), "0:00");
        assert_eq!(format_duration(Some(4_400)), "0:04");
        assert_eq!(format_duration(Some(125_000)), "2:05");
    }

    #[test]
    fn row_times_say_today_yesterday_or_the_day() {
        let now = local(2026, 11, 16, 15, 0);
        assert_eq!(
            format_row_time(&server(local(2026, 11, 16, 14, 14)), "yest", now),
            "2:14 PM"
        );
        assert_eq!(
            format_row_time(&server(local(2026, 11, 15, 18, 21)), "yest", now),
            "yest 6:21 PM"
        );
        assert_eq!(
            format_row_time(&server(local(2026, 9, 21, 9, 0)), "yest", now),
            "Sep 21"
        );
    }

    #[test]
    fn detail_stamps_add_the_year_when_it_differs() {
        let now = local(2026, 11, 16, 15, 0);
        assert_eq!(
            format_detail_stamp(
                &server(local(2026, 11, 16, 9, 5)),
                "Today",
                "Yesterday",
                now
            ),
            "Today · 9:05 AM"
        );
        assert_eq!(
            format_detail_stamp(&server(local(2025, 9, 21, 9, 0)), "Today", "Yesterday", now),
            "Sep 21, 2025 · 9:00 AM"
        );
    }

    #[test]
    fn zoned_timestamps_parse_too() {
        assert!(parse_server_date("2026-11-16T10:00:00Z").is_some());
        assert!(parse_server_date("2026-11-16T10:00:00+02:00").is_some());
        assert!(parse_server_date("nonsense").is_none());
    }

    #[test]
    fn pace_needs_words_and_a_length() {
        assert_eq!(words_per_minute(150, Some(60_000)), Some(150));
        assert_eq!(words_per_minute(0, Some(60_000)), None);
        assert_eq!(words_per_minute(10, None), None);
    }

    #[test]
    fn snippets_mark_added_words() {
        let parts = snippet_parts("We meet on Friday.", &["Friday".into()]);
        assert_eq!(
            parts,
            vec![
                SnippetPart {
                    text: "We meet on ".into(),
                    changed: false
                },
                SnippetPart {
                    text: "Friday".into(),
                    changed: true
                },
                SnippetPart {
                    text: ".".into(),
                    changed: false
                },
            ]
        );
    }

    #[test]
    fn a_late_change_is_pulled_into_view() {
        let text = format!("{} target word", "lorem ipsum ".repeat(10));
        let parts = snippet_parts(&text, &["target".into()]);
        assert!(parts[0].text.starts_with('…'));
        assert!(parts.iter().any(|p| p.changed && p.text == "target"));
    }

    #[test]
    fn counts_and_savings_read_short() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_saved(45_000), "1m");
        assert_eq!(format_saved(720_000), "12m");
        assert_eq!(format_saved(8_460_000), "2h 21m");
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("xx"), "xx");
    }
}
