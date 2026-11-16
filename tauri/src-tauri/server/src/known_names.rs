//! Words the user writes capitalized in the middle of a sentence: their
//! names. Python: `backend/services/known_names.py`.
//!
//! Dictation that continues a sentence lowercases a common first word
//! (`phrase_seams::continue_phrase`). A word that is common in general can
//! still be a name to this user ("Slack", "Mark"), and their own text shows
//! it: recent cleaned dictations and the corrections they typed. Names from
//! dictations that history retention deleted are kept in `known_names` and
//! still count while they are that recent.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kass_text::phrase_seams::{mid_sentence_capitals, Names};
use rusqlite::Connection;

/// Enough history to cover the names in regular use, read in a few ms.
const RECENT: i64 = 500;
const TTL: Duration = Duration::from_secs(300);

/// Reports that teach the writing style: explicit ones, not redictations.
const EXPLICIT_SOURCES: &str = "('manual', 'voice_fix')";

/// The user's names, read again every few minutes or when a correction or
/// a kept name changes (the worker files corrections too).
#[derive(Default)]
pub struct KnownNames {
    cached: Mutex<Option<Cached>>,
}

struct Cached {
    at: Instant,
    version: Version,
    names: Arc<Names>,
}

/// What changes when a correction is filed or withdrawn, or history
/// retention keeps a name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    corrections: i64,
    last_correction: i64,
    kept: i64,
}

fn version(conn: &Connection) -> rusqlite::Result<Version> {
    conn.query_row(
        "SELECT (SELECT count(*) FROM capture_feedback), \
         (SELECT coalesce(max(rowid), 0) FROM capture_feedback), \
         (SELECT count(*) FROM known_names)",
        [],
        |row| {
            Ok(Version {
                corrections: row.get(0)?,
                last_correction: row.get(1)?,
                kept: row.get(2)?,
            })
        },
    )
}

impl KnownNames {
    /// The user's names, from recent captures and corrections.
    pub fn get(&self, conn: &Connection) -> rusqlite::Result<Arc<Names>> {
        let now = version(conn)?;
        {
            let cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cached.as_ref() {
                if cached.version == now && cached.at.elapsed() < TTL {
                    return Ok(Arc::clone(&cached.names));
                }
            }
        }
        let names = Arc::new(read(conn)?);
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = Some(Cached {
            at: Instant::now(),
            version: now,
            names: Arc::clone(&names),
        });
        Ok(names)
    }

    pub fn invalidate(&self) {
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The user's names, read now.
pub fn read(conn: &Connection) -> rusqlite::Result<Names> {
    let (texts, kept) = recent(conn)?;
    let mut names = mid_sentence_capitals(&texts.join("\n"));
    names.extend(kept);
    Ok(names)
}

/// Recent texts, and the kept names of deleted dictations that were as recent.
fn recent(conn: &Connection) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    // A command's output may be a translation, not the user's writing.
    let refined: Vec<(String, Option<String>)> = conn
        .prepare(
            "SELECT transcript_refined, created_at FROM captures \
             WHERE transcript_refined IS NOT NULL AND source != 'command' \
             ORDER BY created_at DESC LIMIT ?1",
        )?
        .query_map([RECENT], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // A redictation's text is already a capture of its own above.
    let corrected: Vec<String> = conn
        .prepare(&format!(
            "SELECT expected_text FROM capture_feedback WHERE source IN {EXPLICIT_SOURCES} \
             ORDER BY created_at DESC LIMIT ?1"
        ))?
        .query_map([RECENT], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // A full page of dictations newer than a deleted one pushes its names out.
    let oldest = match refined.last() {
        Some((_, Some(at))) if refined.len() as i64 == RECENT => Some(at.clone()),
        _ => None,
    };
    let kept: Vec<String> = match &oldest {
        Some(oldest) => conn
            .prepare("SELECT name FROM known_names WHERE last_seen_at >= ?1")?
            .query_map([oldest], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        None => conn
            .prepare("SELECT name FROM known_names")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?,
    };
    let texts = refined
        .into_iter()
        .map(|(text, _)| text)
        .chain(corrected)
        .filter(|text| !text.is_empty())
        .collect();
    Ok((texts, kept))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::store::tests::database;
    use rusqlite::params;

    fn capture(conn: &Connection, id: &str, refined: Option<&str>, source: &str, at: &str) {
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, transcript_refined, \
             audio_deleted, created_at) VALUES (?1, '', ?2, '', ?3, 0, ?4)",
            params![id, source, refined, at],
        )
        .unwrap();
    }

    fn correct(conn: &Connection, id: &str, meant: &str, source: &str, at: &str) {
        capture(conn, &format!("{id}-capture"), None, "dictation", at);
        conn.execute(
            "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot, \
             source, created_at) VALUES (?1, ?2, 'refined', ?3, '', '{}', ?4, ?5)",
            params![id, format!("{id}-capture"), meant, source, at],
        )
        .unwrap();
    }

    fn keep(conn: &Connection, name: &str, at: &str) {
        conn.execute(
            "INSERT INTO known_names (name, last_seen_at) VALUES (?1, ?2)",
            params![name, at],
        )
        .unwrap();
    }

    #[test]
    fn names_come_from_dictations_and_explicit_corrections() {
        let conn = database();
        capture(
            &conn,
            "a",
            Some("We shipped it with Priya today."),
            "dictation",
            "2026-11-01 10:00:00.000000",
        );
        capture(
            &conn,
            "b",
            Some("Ask Tobias about it."),
            "command",
            "2026-11-01 10:01:00.000000",
        );
        correct(
            &conn,
            "fix",
            "Meet Morgan at noon.",
            "voice_fix",
            "2026-11-01 10:02:00.000000",
        );
        correct(
            &conn,
            "again",
            "Call Rosalind at noon.",
            "redictation",
            "2026-11-01 10:03:00.000000",
        );
        keep(&conn, "Ingrid", "2026-10-01 10:00:00.000000");

        let names = read(&conn).unwrap();
        assert!(names.contains("Priya") && names.contains("Morgan") && names.contains("Ingrid"));
        assert!(
            !names.contains("Tobias"),
            "a command's output isn't the user's writing"
        );
        assert!(
            !names.contains("Rosalind"),
            "a redictation doesn't teach names"
        );
        assert!(!names.contains("We") && !names.contains("Meet"));
    }

    #[test]
    fn a_full_page_of_newer_dictations_pushes_kept_names_out() {
        let conn = database();
        keep(&conn, "Ingrid", "2026-10-01 10:00:00.000000");
        keep(&conn, "Priya", "2026-11-05 10:00:00.000000");
        for i in 0..RECENT {
            let at = format!("2026-11-{:02} 10:{:02}:00.000000", 2 + i / 60, i % 60);
            capture(&conn, &format!("c{i}"), Some("ok."), "dictation", &at);
        }
        let names = read(&conn).unwrap();
        assert!(names.contains("Priya"));
        assert!(!names.contains("Ingrid"));
    }

    #[test]
    fn a_new_correction_is_seen_before_the_cache_expires() {
        let conn = database();
        let known = KnownNames::default();
        assert!(known.get(&conn).unwrap().is_empty());
        correct(
            &conn,
            "fix",
            "Meet Morgan at noon.",
            "manual",
            "2026-11-01 10:02:00.000000",
        );
        assert!(known.get(&conn).unwrap().contains("Morgan"));
    }
}
