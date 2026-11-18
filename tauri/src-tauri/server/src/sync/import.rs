//! A shared Kass applying what a stand-in changed while it was away
//! (`backend/services/sync/importer.py` at b340f4f7).
//!
//! Each change carries the row as the stand-in last had it, or says it was
//! deleted, and when. Where both Macs changed the same row, the later edit
//! wins. Applying the same changes twice leaves the same result, so a batch
//! the stand-in sends again after a lost reply does no harm (take reports
//! aside: they're counts with no id of their own).
//!
//! Reports are applied as rows; the caller then has everything learned from
//! them refreshed the way a new report refreshes it here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;

use super::rows::{self, Row, KINDS, OWN_SETTINGS};
use crate::captures::{resolve_storage_path, store as captures};
use crate::dictionary::store::{self as dictionary, Sync};
use crate::styles;

/// The recordings a stand-in may send; anything else is saved as a WAV.
const AUDIO_SUFFIXES: [&str; 6] = [".wav", ".mp3", ".m4a", ".flac", ".ogg", ".webm"];

/// One row a stand-in changed.
#[derive(Clone, Debug, Deserialize)]
pub struct Change {
    pub kind: String,
    pub id: String,
    pub deleted: bool,
    pub changed_at: String,
    #[serde(default)]
    pub row: Option<Row>,
}

/// A new capture's recording, by capture id: its file name and bytes.
pub type Audio = HashMap<String, (String, Vec<u8>)>;

/// What applying changed, for the caller to tell the worker and the apps.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// Reports that changed, by target and source, and whether one went.
    pub reports: BTreeMap<(String, String), bool>,
    /// Captures deleted, whose voice takes the worker forgets.
    pub forgotten: Vec<String>,
    /// Styles deleted, whose learning the worker forgets.
    pub styles_deleted: Vec<String>,
    /// Whether any other style or app assignment changed.
    pub styles: bool,
}

fn newer_here(row: &Row, changed_at: NaiveDateTime) -> bool {
    rows::edited_at(row).is_some_and(|here| here > changed_at)
}

/// Insert the row, or update it unless it changed here since.
fn upsert(
    conn: &Connection,
    table: &str,
    id: &str,
    values: &Row,
    changed_at: NaiveDateTime,
    skip_update: &[&str],
) -> rusqlite::Result<()> {
    match rows::get(conn, table, id)? {
        None => rows::insert(conn, table, values, &[]),
        Some(here) if newer_here(&here, changed_at) => Ok(()),
        Some(_) => rows::update(conn, table, id, values, skip_update).map(|_| ()),
    }
}

/// Delete the row unless it changed here since.
fn delete_unless_newer(
    conn: &Connection,
    table: &str,
    id: &str,
    changed_at: NaiveDateTime,
) -> rusqlite::Result<()> {
    if let Some(here) = rows::get(conn, table, id)? {
        if !newer_here(&here, changed_at) {
            rows::delete(conn, table, id)?;
        }
    }
    Ok(())
}

fn text<'a>(row: &'a Row, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn style(
    conn: &mut Connection,
    change: &Change,
    row: &Row,
    changed_at: NaiveDateTime,
    applied: &mut Applied,
) -> rusqlite::Result<()> {
    let sync = Sync::default();
    if change.deleted {
        let Some(here) = rows::get(conn, "writing_styles", &change.id)? else {
            return Ok(());
        };
        if here.get("is_default") == Some(&Value::Bool(true)) || newer_here(&here, changed_at) {
            return Ok(());
        }
        if let Ok(default) = styles::delete_style(conn, &change.id, sync)? {
            dictionary::move_style(conn, &change.id, &default, sync)?;
            applied.styles_deleted.push(change.id.clone());
        }
        return Ok(());
    }
    // One default: the shared Kass keeps its own.
    let mut values = row.clone();
    values.insert("is_default".into(), Value::Bool(false));
    upsert(
        conn,
        "writing_styles",
        &change.id,
        &values,
        changed_at,
        &["is_default"],
    )?;
    applied.styles = true;
    Ok(())
}

fn app_style(
    conn: &Connection,
    change: &Change,
    row: &Row,
    changed_at: NaiveDateTime,
) -> rusqlite::Result<bool> {
    if change.deleted {
        delete_unless_newer(conn, "app_styles", &change.id, changed_at)?;
        return Ok(true);
    }
    let style = text(row, "style_id").unwrap_or_default();
    if rows::get(conn, "writing_styles", style)?.is_none() {
        return Ok(false);
    }
    upsert(conn, "app_styles", &change.id, row, changed_at, &[])?;
    Ok(true)
}

fn settings(
    conn: &Connection,
    change: &Change,
    row: &Row,
    changed_at: NaiveDateTime,
) -> rusqlite::Result<()> {
    if change.deleted {
        return Ok(());
    }
    match rows::get(conn, "capture_settings", "1")? {
        Some(here) if !newer_here(&here, changed_at) => {
            rows::update(conn, "capture_settings", "1", row, &OWN_SETTINGS).map(|_| ())
        }
        _ => Ok(()),
    }
}

fn dictionary_entry(
    conn: &mut Connection,
    change: &Change,
    row: &Row,
    changed_at: NaiveDateTime,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    if change.deleted {
        delete_unless_newer(&tx, "dictionary_entries", &change.id, changed_at)?;
        return tx.commit();
    }
    // The same word added on both Macs under different ids: keep the later one.
    let same_word: Option<String> = tx
        .query_row(
            "SELECT id FROM dictionary_entries WHERE scope = ?1 AND scope_id = ?2 AND \"key\" = ?3 AND id != ?4",
            params![text(row, "scope"), text(row, "scope_id"), text(row, "key"), change.id],
            |found| found.get(0),
        )
        .optional()?;
    if let Some(same_word) = same_word {
        let here = rows::get(&tx, "dictionary_entries", &same_word)?.unwrap_or_default();
        if newer_here(&here, changed_at) {
            return Ok(());
        }
        rows::delete(&tx, "dictionary_entries", &same_word)?;
    }
    upsert(&tx, "dictionary_entries", &change.id, row, changed_at, &[])?;
    tx.commit()
}

fn save_audio(
    data_dir: &Path,
    capture_id: &str,
    filename: &str,
    data: &[u8],
) -> std::io::Result<String> {
    let suffix = Path::new(filename)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .filter(|suffix| AUDIO_SUFFIXES.contains(&suffix.as_str()))
        .unwrap_or_else(|| ".wav".into());
    let stored = format!("captures/{capture_id}{suffix}");
    std::fs::create_dir_all(data_dir.join("captures"))?;
    std::fs::write(data_dir.join(&stored), data)?;
    Ok(stored)
}

fn capture(
    conn: &mut Connection,
    data_dir: &Path,
    change: &Change,
    row: &Row,
    audio: &Audio,
    applied: &mut Applied,
) -> rusqlite::Result<()> {
    let sync = Sync::default();
    if change.deleted {
        let Some(here) = rows::get(conn, "captures", &change.id)? else {
            return Ok(());
        };
        let recording: Option<PathBuf> = text(&here, "audio_path")
            .and_then(|stored| resolve_storage_path(data_dir, stored))
            .filter(|path| path.is_file());
        let Some(reports) = captures::delete(conn, &change.id, sync)? else {
            return Ok(());
        };
        if let Some(path) = recording {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!(
                    "[sync] couldn't remove capture audio {}: {error}",
                    path.display()
                );
            }
        }
        // The words it spelled as a voice edit, and those its reports fixed.
        dictionary::delete_added_by(conn, &change.id, sync)?;
        for (report, target, source) in reports {
            dictionary::delete_added_by(conn, &report, sync)?;
            applied.reports.insert((target, source), true);
        }
        applied.forgotten.push(change.id.clone());
        return Ok(());
    }
    let mut values = row.clone();
    if rows::get(conn, "captures", &change.id)?.is_none() {
        match audio.get(&change.id) {
            Some((filename, data)) => {
                let stored = save_audio(data_dir, &change.id, filename, data)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
                values.insert("audio_path".into(), stored.into());
            }
            None => {
                let had_audio = text(&values, "audio_path").is_some_and(|path| !path.is_empty());
                let deleted = values.get("audio_deleted") == Some(&Value::Bool(true));
                values.insert("audio_deleted".into(), Value::Bool(deleted || had_audio));
            }
        }
        return rows::insert(conn, "captures", &values, &[]);
    }
    // Its recording and where it came from stay as the shared Kass has them.
    rows::update(
        conn,
        "captures",
        &change.id,
        &values,
        &[
            "audio_path",
            "audio_deleted",
            "mac_id",
            "mac_name",
            "created_at",
        ],
    )
    .map(|_| ())
}

fn feedback(
    conn: &mut Connection,
    change: &Change,
    row: &Row,
    applied: &mut Applied,
) -> rusqlite::Result<()> {
    let here = rows::get(conn, "capture_feedback", &change.id)?;
    let pair = |row: &Row| {
        (
            text(row, "target").unwrap_or_default().to_string(),
            text(row, "source").unwrap_or("manual").to_string(),
        )
    };
    if change.deleted {
        if let Some(here) = here {
            applied.reports.insert(pair(&here), true);
            rows::delete(conn, "capture_feedback", &change.id)?;
            dictionary::delete_added_by(conn, &change.id, Sync::default())?;
        }
        return Ok(());
    }
    let capture = text(row, "capture_id").unwrap_or_default();
    if rows::get(conn, "captures", capture)?.is_none() {
        return Ok(());
    }
    applied.reports.entry(pair(row)).or_insert(false);
    match here {
        None => rows::insert(conn, "capture_feedback", row, &[]),
        // Reports are never edited; a withdrawn one before it can trim its text.
        Some(_) => {
            let mut expected = Row::new();
            expected.insert(
                "expected_text".into(),
                row.get("expected_text").cloned().unwrap_or_default(),
            );
            rows::update(conn, "capture_feedback", &change.id, &expected, &[]).map(|_| ())
        }
    }
}

/// Apply a stand-in's changes, oldest first within each kind. A kind this
/// Kass doesn't know (a newer one's) is left out.
pub fn apply(
    conn: &mut Connection,
    data_dir: &Path,
    changes: Vec<Change>,
    audio: &Audio,
) -> rusqlite::Result<Applied> {
    let order = |kind: &str| KINDS.iter().position(|(name, _)| *name == kind);
    let mut ordered: Vec<(usize, NaiveDateTime, Change)> = changes
        .into_iter()
        .filter_map(|change| {
            let at = rows::parse_time(&change.changed_at)?;
            Some((order(&change.kind)?, at, change))
        })
        .collect();
    ordered.sort_by_key(|(kind, at, _)| (*kind, *at));
    let mut applied = Applied::default();
    for (_, changed_at, change) in ordered {
        let row = change.row.clone().unwrap_or_default();
        match change.kind.as_str() {
            "style" => style(conn, &change, &row, changed_at, &mut applied)?,
            "app_style" => applied.styles |= app_style(conn, &change, &row, changed_at)?,
            "settings" => settings(conn, &change, &row, changed_at)?,
            "dictionary" => dictionary_entry(conn, &change, &row, changed_at)?,
            "capture" => capture(conn, data_dir, &change, &row, audio, &mut applied)?,
            "feedback" => feedback(conn, &change, &row, &mut applied)?,
            "take" if !change.deleted => rows::insert(conn, "take_reports", &row, &["id"])?,
            _ => {}
        }
    }
    Ok(applied)
}
