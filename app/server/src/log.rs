//! Where the app's and this server's `eprintln!` lines go: stderr as
//! before, `logs/server.log` beside the Python worker's lines, and the
//! in-app Logs view.
//!
//! An app run from /Applications has no terminal, so stderr alone loses
//! them (the dictation timings, `[network]` and `[dictation]` lines). Both
//! crates send `eprintln!` here (the macro at the top of `lib.rs` and of the
//! app's `main.rs`), so a new line needs nothing special to be kept.
//!
//! A line is written the way the worker's `logging` writes its own,
//! `2026-11-23 10:15:42,123 - kass-server - INFO - message`, with the crate
//! in the logger's place. The file rotates as the worker's does: past
//! [`MAX_BYTES`] it moves to `server.log.1`, keeping [`BACKUPS`] old files.
//! Both processes append to whatever `server.log` is when they write, so
//! either one rotating loses nothing.

use std::cell::Cell;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// As the worker's `RotatingFileHandler`: 2 MB, three old files.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const BACKUPS: u32 = 3;

pub const FILE_NAME: &str = "server.log";

/// The `logs` folder, once the app has said where it is.
static DIR: OnceLock<PathBuf> = OnceLock::new();
/// Held while a line is appended, so two never interleave or both rotate.
static FILE: Mutex<()> = Mutex::new(());
type Forward = Box<dyn Fn(&str) + Send + Sync>;

/// What else gets each line: the in-app Logs view.
static FORWARD: OnceLock<Forward> = OnceLock::new();

thread_local! {
    /// Set while this thread forwards a line, so a line the forwarding
    /// itself prints isn't forwarded again.
    static FORWARDING: Cell<bool> = const { Cell::new(false) };
}

/// Write lines to `dir/server.log` from now on. Only the first call counts.
pub fn to_file(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// Hand every line to `forward` too, once it's written. Only the first
/// call counts.
pub fn forward(forward: impl Fn(&str) + Send + Sync + 'static) {
    let _ = FORWARD.set(Box::new(forward));
}

/// One `eprintln!` line from `source`, the crate that printed it.
pub fn write(source: &str, message: fmt::Arguments) {
    let message = message.to_string();
    std::eprintln!("{message}");
    let line = format_line(source, &message, chrono::Local::now());
    if let Some(dir) = DIR.get() {
        let _held = FILE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(e) = append(dir, &line, MAX_BYTES) {
            std::eprintln!(
                "[log] could not write {}: {e}",
                dir.join(FILE_NAME).display()
            );
        }
    }
    if let Some(forward) = FORWARD.get() {
        if !FORWARDING.replace(true) {
            forward(&line);
            FORWARDING.set(false);
        }
    }
}

fn format_line(source: &str, message: &str, at: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "{} - {source} - INFO - {message}",
        at.format("%Y-%m-%d %H:%M:%S,%3f")
    )
}

/// Append `line` to `dir/server.log`, rotating first when it would pass
/// `max_bytes`, as `RotatingFileHandler.shouldRollover` does.
fn append(dir: &Path, line: &str, max_bytes: u64) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let size = fs::metadata(&path).map_or(0, |meta| meta.len());
    let bytes = line.len() as u64 + 1;
    if size > 0 && size + bytes >= max_bytes {
        rotate(dir);
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(format!("{line}\n").as_bytes())
}

/// `server.log.2` → `.3`, `.1` → `.2`, `server.log` → `.1`, dropping the
/// oldest. A file another process already moved is skipped.
fn rotate(dir: &Path) {
    let numbered = |n: u32| dir.join(format!("{FILE_NAME}.{n}"));
    let _ = fs::remove_file(numbered(BACKUPS));
    for n in (1..BACKUPS).rev() {
        let _ = fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = fs::rename(dir.join(FILE_NAME), numbered(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-log-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lines_read_like_the_workers() {
        let at = chrono::Local
            .with_ymd_and_hms(2026, 11, 23, 10, 15, 42)
            .unwrap()
            + chrono::Duration::milliseconds(7);
        assert_eq!(
            format_line("kass-server", "[server] Dictation stream finished: x", at),
            "2026-11-23 10:15:42,007 - kass-server - INFO - [server] Dictation stream finished: x"
        );
    }

    #[test]
    fn appends_after_the_workers_lines() {
        let dir = scratch("append");
        fs::write(dir.join(FILE_NAME), "worker line\n").unwrap();
        append(&dir, "server line", MAX_BYTES).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            "worker line\nserver line\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotates_past_the_limit_keeping_three_old_files() {
        let dir = scratch("rotate");
        for n in 1..=5 {
            append(&dir, &format!("line {n}"), 10).unwrap();
        }
        let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read(FILE_NAME), "line 5\n");
        assert_eq!(read("server.log.1"), "line 4\n");
        assert_eq!(read("server.log.2"), "line 3\n");
        assert_eq!(read("server.log.3"), "line 2\n");
        assert!(!dir.join("server.log.4").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn eprintln_lands_in_server_log() {
        // The only test that sets the folder: other tests' lines land in it
        // too from here on, so look for this one among them.
        let dir = scratch("eprintln");
        to_file(dir.clone());
        let marker = uuid::Uuid::new_v4();
        eprintln!("[server] Dictation stream finished: {marker}");
        let written = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(
            written
                .lines()
                .any(|line| line.contains(" - kass-server - INFO - ")
                    && line.ends_with(&format!("[server] Dictation stream finished: {marker}"))),
            "{written}"
        );
    }

    #[test]
    fn makes_the_logs_folder() {
        let dir = scratch("folder").join("logs");
        append(&dir, "first", MAX_BYTES).unwrap();
        assert_eq!(fs::read_to_string(dir.join(FILE_NAME)).unwrap(), "first\n");
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
