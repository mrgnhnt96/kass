//! The server's output, kept from launch for Settings › Logs: the last
//! [`MAX_LINES`] lines of the `server-log` event, each with its time and level.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Local};
use gpui_kit::{App, AppContext as _, Entity, Global};
use serde::Deserialize;

use crate::host::{self, AppHandle, Listener};

pub const MAX_LINES: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub time: DateTime<Local>,
    pub line: String,
    pub level: Level,
}

#[derive(Default)]
struct Buffer {
    entries: VecDeque<Entry>,
}

static BUFFER: Mutex<Buffer> = Mutex::new(Buffer {
    entries: VecDeque::new(),
});

/// Set while a redraw of the logs is already on its way to the main thread.
static NOTIFYING: AtomicBool = AtomicBool::new(false);

/// What views observe to redraw when lines arrive or are cleared.
pub struct Feed;

struct FeedGlobal(Entity<Feed>);

impl Global for FeedGlobal {}

pub fn feed(cx: &App) -> Entity<Feed> {
    cx.global::<FeedGlobal>().0.clone()
}

#[derive(Deserialize)]
struct LogEvent {
    line: String,
}

/// Start keeping the server's lines. Called once, before the server starts.
pub fn init(cx: &mut App) {
    let feed = cx.new(|_| Feed);
    cx.set_global(FeedGlobal(feed));
    AppHandle.listen("server-log", |event| {
        if let Ok(LogEvent { line }) = serde_json::from_str(event.payload()) {
            push(line);
        }
    });
}

fn push(line: String) {
    {
        let mut buffer = BUFFER.lock().unwrap();
        let level = level_of(&line);
        buffer.entries.push_back(Entry {
            time: Local::now(),
            line,
            level,
        });
        while buffer.entries.len() > MAX_LINES {
            buffer.entries.pop_front();
        }
    }
    if !NOTIFYING.swap(true, Ordering::AcqRel) {
        host::with_app(|cx| {
            NOTIFYING.store(false, Ordering::Release);
            if let Some(feed) = cx.try_global::<FeedGlobal>().map(|g| g.0.clone()) {
                feed.update(cx, |_, cx| cx.notify());
            }
        });
    }
}

/// Every line kept, oldest first.
pub fn entries() -> Vec<Entry> {
    BUFFER.lock().unwrap().entries.iter().cloned().collect()
}

pub fn clear(cx: &mut App) {
    BUFFER.lock().unwrap().entries.clear();
    feed(cx).update(cx, |_, cx| cx.notify());
}

/// The server writes most of its logging to stderr, so the stream says
/// nothing about severity: read the level from the line itself.
pub fn level_of(line: &str) -> Level {
    let error_word = ["ERROR", "CRITICAL", "FATAL", "Traceback"]
        .iter()
        .any(|word| has_word(line, word));
    if error_word || has_named_error(line) {
        Level::Error
    } else if has_word(line, "WARN") || has_word(line, "WARNING") {
        Level::Warn
    } else {
        Level::Info
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `\bword\b`.
fn has_word(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        (start == 0 || !is_word_byte(bytes[start - 1]))
            && (end == bytes.len() || !is_word_byte(bytes[end]))
    })
}

/// `\w+(Error|Exception):`, as in `ValueError:`.
fn has_named_error(line: &str) -> bool {
    let bytes = line.as_bytes();
    ["Error:", "Exception:"].iter().any(|suffix| {
        line.match_indices(suffix)
            .any(|(start, _)| start > 0 && is_word_byte(bytes[start - 1]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_errors_from_the_line() {
        assert_eq!(level_of("2026-01-01 ERROR something broke"), Level::Error);
        assert_eq!(level_of("Traceback (most recent call last):"), Level::Error);
        assert_eq!(level_of("ValueError: bad value"), Level::Error);
        assert_eq!(level_of("  raise RuntimeException: x"), Level::Error);
        assert_eq!(level_of("[FATAL] out of memory"), Level::Error);
    }

    #[test]
    fn reads_warnings_from_the_line() {
        assert_eq!(level_of("WARNING: model is large"), Level::Warn);
        assert_eq!(level_of("[WARN] slow"), Level::Warn);
    }

    #[test]
    fn needs_whole_words() {
        assert_eq!(level_of("ERRORS are counted"), Level::Info);
        assert_eq!(level_of("no errors here"), Level::Info);
        assert_eq!(level_of("WARNINGS off"), Level::Info);
        assert_eq!(level_of("Error: on its own"), Level::Info);
        assert_eq!(level_of("INFO: started"), Level::Info);
    }
}
