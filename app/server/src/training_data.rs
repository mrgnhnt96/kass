//! The reports and dictations a model-improvement run plans from, read from
//! kass.db for the worker, which doesn't open the database itself.
//!
//! The worker asks for them over HTTP (`/capture/learning/training/*`)
//! while it prepares a run: it groups the reports into samples and banks the
//! dictations' audio for voice training
//! (backend/services/model_improvement/data.py, voice_training/bank.py).

use chrono::NaiveDateTime;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::captures::EXPLICIT_SOURCES;
use crate::cleanup_inputs::isoformat;

/// How kass.db stores a datetime.
const STORED: &str = "%Y-%m-%d %H:%M:%S%.6f";

/// The newest `limit` reports, newest first: `{"id", "capture_id",
/// "target", "expected_text", "snapshot", "source", "created_at"}`.
pub fn reports(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<Value>> {
    let mut statement = conn.prepare(
        "SELECT id, capture_id, target, expected_text, snapshot, source, created_at \
         FROM capture_feedback ORDER BY created_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![limit], report)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The explicit corrections (typed or spoken fixes, not re-dictations) of
/// the captures in `capture_ids`, in the same shape as [`reports`].
pub fn corrections(conn: &Connection, capture_ids: &[String]) -> rusqlite::Result<Vec<Value>> {
    let sources = EXPLICIT_SOURCES
        .map(|source| format!("'{source}'"))
        .join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT id, capture_id, target, expected_text, snapshot, source, created_at \
         FROM capture_feedback \
         WHERE capture_id IN (SELECT value FROM json_each(?1)) AND source IN ({sources}) \
         ORDER BY created_at, id"
    ))?;
    let ids = serde_json::to_string(capture_ids).unwrap_or_else(|_| "[]".to_string());
    let rows = statement
        .query_map(params![ids], report)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn report(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "capture_id": row.get::<_, String>(1)?,
        "target": row.get::<_, String>(2)?,
        "expected_text": row.get::<_, String>(3)?,
        "snapshot": row.get::<_, String>(4)?,
        "source": row.get::<_, String>(5)?,
        "created_at": row.get::<_, Option<String>>(6)?.as_deref().map(isoformat),
    }))
}

/// Up to `limit` dictations made after `after` (an ISO 8601 datetime, or
/// every one for `None`), oldest first: `{"id", "transcript_raw",
/// "audio_path", "created_at"}`.
pub fn dictations(
    conn: &Connection,
    after: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<Value>> {
    let after = after.and_then(stored);
    let mut statement = conn.prepare(
        "SELECT id, transcript_raw, audio_path, created_at FROM captures \
         WHERE source = 'dictation' AND (?1 IS NULL OR created_at > ?1) \
         ORDER BY created_at LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![after, limit], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "transcript_raw": row.get::<_, String>(1)?,
                "audio_path": row.get::<_, String>(2)?,
                "created_at": row.get::<_, Option<String>>(3)?.as_deref().map(isoformat),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// An ISO 8601 datetime as kass.db stores it, so it compares as text.
fn stored(iso: &str) -> Option<String> {
    NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|at| at.format(STORED).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../tests/kass_schema.sql"))
            .unwrap();
        conn
    }

    fn capture(conn: &Connection, id: &str, source: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, audio_deleted, created_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![id, format!("captures/{id}.wav"), source, format!("said {id}"), created_at],
        )
        .unwrap();
    }

    fn feedback(conn: &Connection, id: &str, capture_id: &str, source: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot, source, created_at)
             VALUES (?1, ?2, 'refined', 'Meant.', '', '{}', ?3, ?4)",
            params![id, capture_id, source, created_at],
        )
        .unwrap();
    }

    #[test]
    fn reports_are_the_newest_first_with_iso_datetimes() {
        let conn = database();
        capture(&conn, "c1", "dictation", "2026-11-01 09:00:00.000000");
        feedback(&conn, "f1", "c1", "manual", "2026-11-01 10:00:00.000000");
        feedback(
            &conn,
            "f2",
            "c1",
            "redictation",
            "2026-11-02 10:00:00.250000",
        );
        feedback(&conn, "f3", "c1", "voice_fix", "2026-11-03 10:00:00.000000");
        let rows = reports(&conn, 2).unwrap();
        let ids: Vec<_> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["f3", "f2"]);
        assert_eq!(rows[0]["created_at"], "2026-11-03T10:00:00");
        assert_eq!(rows[1]["created_at"], "2026-11-02T10:00:00.250000");
        assert_eq!(rows[1]["source"], "redictation");
        assert_eq!(rows[0]["snapshot"], "{}");
    }

    #[test]
    fn corrections_are_the_explicit_ones_of_the_captures_asked_for() {
        let conn = database();
        capture(&conn, "c1", "dictation", "2026-11-01 09:00:00.000000");
        capture(&conn, "c2", "dictation", "2026-11-01 09:30:00.000000");
        feedback(&conn, "f1", "c1", "manual", "2026-11-01 10:00:00.000000");
        feedback(
            &conn,
            "f2",
            "c1",
            "redictation",
            "2026-11-02 10:00:00.000000",
        );
        feedback(&conn, "f3", "c2", "voice_fix", "2026-11-03 10:00:00.000000");
        let rows = corrections(&conn, &["c1".to_string()]).unwrap();
        let ids: Vec<_> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["f1"]);
        assert!(corrections(&conn, &[]).unwrap().is_empty());
    }

    #[test]
    fn dictations_continue_after_the_last_one_scanned() {
        let conn = database();
        capture(&conn, "a", "dictation", "2026-11-01 09:00:00.000000");
        capture(&conn, "b", "command", "2026-11-01 09:30:00.000000");
        capture(&conn, "c", "dictation", "2026-11-01 10:00:00.500000");
        capture(&conn, "d", "dictation", "2026-11-01 11:00:00.000000");
        let ids = |rows: Vec<Value>| -> Vec<String> {
            rows.iter()
                .map(|row| row["id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(dictations(&conn, None, 2).unwrap()), ["a", "c"]);
        let first = dictations(&conn, None, 1).unwrap();
        assert_eq!(first[0]["created_at"], "2026-11-01T09:00:00");
        assert_eq!(first[0]["audio_path"], "captures/a.wav");
        assert_eq!(first[0]["transcript_raw"], "said a");
        assert_eq!(
            ids(dictations(&conn, Some("2026-11-01T09:00:00"), 10).unwrap()),
            ["c", "d"]
        );
        assert_eq!(
            ids(dictations(&conn, Some("2026-11-01T10:00:00.500000"), 10).unwrap()),
            ["d"]
        );
    }
}
