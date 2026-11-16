//! What is read from and saved to `captures.prosody`: the speaker's
//! baseline, what their edits taught, and a dictation's measurements.
//! Python: `load_baseline` and `save` in `backend/services/prosody.py`, and
//! `load` in `backend/services/expression_learning.py`.
//!
//! Everything is read again when a dictation starts, so a withdrawn report
//! stops teaching at once.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::Value;

use super::learning::{gather, learn, Learned, Saved, SavedCapture};
use super::voice::{Baseline, Measured, Statement};

/// Captures the speaker's baseline is read from.
const BASELINE_CAPTURES: i64 = 200;
/// Recent dictations read for the speaker's plain sentences and words.
const RECENT_CAPTURES: i64 = 500;
/// Report sources that are the user's own edit; any other (a redictation)
/// may only make the rules stricter.
const EXPLICIT_SOURCES: [&str; 2] = ["manual", "voice_fix"];

/// The speaker's baseline, from the sentences saved with recent captures.
pub fn load_baseline(conn: &Connection) -> rusqlite::Result<Option<Baseline>> {
    let mut statement = conn.prepare(
        "SELECT prosody FROM captures WHERE prosody IS NOT NULL \
         ORDER BY created_at DESC LIMIT ?1",
    )?;
    let saved = statement
        .query_map([BASELINE_CAPTURES], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut found = Vec::new();
    for text in saved {
        let Ok(Value::Object(mut measured)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(Value::Array(sentences)) = measured.remove("sentences") else {
            continue;
        };
        found.extend(
            sentences
                .into_iter()
                .filter_map(|sentence| Statement::deserialize(sentence).ok()),
        );
    }
    Ok(Baseline::of(&found))
}

/// The speaker's learned rule, from their saved measurements and their
/// reports of them.
pub fn load_learned(conn: &Connection) -> rusqlite::Result<Learned> {
    // Capture id → (saved measurements, text delivered), in the order first
    // read: recent captures, then any older one a report is about.
    let mut order: Vec<String> = Vec::new();
    let mut captures: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut keep = |id: String, saved: String, delivered: Option<String>| {
        if captures.insert(id.clone(), (saved, delivered)).is_none() {
            order.push(id);
        }
    };
    let mut recent = conn.prepare(
        "SELECT id, prosody, transcript_refined FROM captures WHERE prosody IS NOT NULL \
         ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = recent.query_map([RECENT_CAPTURES], |row| {
        Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
    })?;
    for row in rows {
        let (id, saved, delivered) = row?;
        keep(id, saved, delivered);
    }
    let mut reports = conn.prepare(
        "SELECT f.capture_id, f.expected_text, f.source, c.prosody, c.transcript_refined \
         FROM capture_feedback f JOIN captures c ON c.id = f.capture_id \
         WHERE f.target = 'refined' AND c.prosody IS NOT NULL \
         ORDER BY f.created_at, f.id",
    )?;
    let rows = reports.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let (mut edits, mut redictated) = (HashMap::new(), HashSet::new());
    for row in rows {
        let (id, expected, source, saved, delivered) = row?;
        keep(id.clone(), saved, delivered);
        // Reports stack: the newest is the text as the user left it.
        edits.insert(id.clone(), expected);
        if source.is_some_and(|source| EXPLICIT_SOURCES.contains(&source.as_str())) {
            redictated.remove(&id);
        } else {
            redictated.insert(id);
        }
    }
    let measured: Vec<SavedCapture> = order
        .into_iter()
        .filter_map(|id| {
            let (saved, delivered) = captures.remove(&id)?;
            let saved = serde_json::from_str::<Saved>(&saved).ok()?;
            Some(SavedCapture {
                id,
                saved,
                delivered: delivered.unwrap_or_default(),
            })
        })
        .collect();
    Ok(learn(&gather(&measured, &edits, &redictated)))
}

/// Store a capture's measurements with it.
pub fn save(conn: &Connection, capture_id: &str, measured: &Measured) -> rusqlite::Result<()> {
    let json = serde_json::to_string(measured)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    conn.execute(
        "UPDATE captures SET prosody = ?1 WHERE id = ?2",
        params![json, capture_id],
    )?;
    Ok(())
}
