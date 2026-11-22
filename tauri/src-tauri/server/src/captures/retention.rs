//! History retention: delete old captures without forgetting what they
//! taught (docs/plans/HISTORY_RETENTION.md). Python:
//! `backend/services/history_retention.py`.
//!
//! Captures older than the user's window (`history_retention_days`, 0 keeps
//! them forever) are deleted with their audio a minute after the server
//! starts, every hour after that, and when the user saves a window. Before
//! a capture goes, everything learned from it moves somewhere that lasts:
//!
//! - its corrections stay, with the app and style they teach copied onto
//!   them and their audio copied to the correction audio folder (unless the
//!   user keeps no recordings, `discard_audio`);
//! - usage stats get a `retired_captures` row of its numbers;
//! - the names it wrote go into `known_names`.
//!
//! Each batch folds and deletes in one transaction, so a failure leaves the
//! captures as they were, and a folded capture is deleted in the same
//! commit, so it is never folded twice. Audio files go after the commit; a
//! crash before then leaves files with no capture, which the next sweep
//! deletes once they are older than the window.
//!
//! Nothing is deleted until the user confirms the window
//! (`history_retention_confirmed`); with nothing old to delete there is
//! nothing to ask, so it confirms itself (`settings::retention_status`).
//!
//! The newest capture, the current take, is never deleted, nor one being
//! refined or retranscribed (`in_use`).
//!
//! A stand-in for a shared Kass (`sync`) keeps its captures only until the
//! shared Kass has them: it applies the window the shared Kass set, which
//! it copies with the settings, to captures already sent there, and deletes
//! them without folding (the shared Kass folds its copies under the same
//! window, and the names it keeps are copied back) and without recording
//! the deletes, which would delete the shared Kass's copies too. A capture
//! still waiting to be sent, or whose correction is, stays.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use kass_text::phrase_seams::mid_sentence_capitals;
use kass_text::py;
use rusqlite::types::ValueRef;
use rusqlite::{params, params_from_iter, Connection, Row, Transaction, TransactionBehavior};
use serde_json::Value;

use super::in_use::InUse;
use super::resolve_storage_path;
use crate::db::{Db, DbError};
use crate::dictionary::store::{now, parse_stored, stored, Sync};
use crate::{settings, AppState};

/// How often the window is applied while the server runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);
/// The first sweep waits for launch to settle: models load and the first
/// dictation comes then.
const FIRST_SWEEP: Duration = Duration::from_secs(60);
const BATCH: usize = 200;
/// Where a deleted capture's corrections keep its audio, in the data folder.
pub const CORRECTION_AUDIO: &str = "correction-audio";

/// Sweep a minute after start, then hourly, until the server stops.
pub async fn watch(state: AppState) {
    tokio::time::sleep(FIRST_SWEEP).await;
    loop {
        if let Err(error) = sweep(&state, now()).await {
            eprintln!("[server] history retention sweep failed; keeping the captures: {error}");
        }
        tokio::time::sleep(SWEEP_EVERY).await;
    }
}

/// Fold and delete every capture past a confirmed window as of `at`. How
/// many went.
pub async fn sweep(state: &AppState, at: NaiveDateTime) -> Result<usize, DbError> {
    let deleted = sweep_db(&state.db, &state.in_use, state.sync, at).await?;
    if deleted > 0 {
        // Their cleaned text no longer counts; the names kept from it do.
        state.known_names.invalidate();
    }
    Ok(deleted)
}

async fn sweep_db(
    db: &Db,
    in_use: &Arc<InUse>,
    sync: Sync,
    at: NaiveDateTime,
) -> Result<usize, DbError> {
    let standing_in = sync.standing_in;
    let (days, confirmed, keep_audio) = db
        .call(move |conn| {
            let saved = settings::get(conn)?;
            if standing_in {
                // The shared Kass's choice, as copied; confirming it is
                // that Kass's to do.
                return Ok((
                    saved.history_retention_days,
                    saved.history_retention_confirmed,
                    false,
                ));
            }
            let (days, confirmed, _) = settings::retention_status_at(conn, sync, at)?;
            Ok((days, confirmed, !saved.discard_audio))
        })
        .await?;
    let Some(before) = settings::cutoff(days, at) else {
        return Ok(0);
    };
    if !confirmed {
        return Ok(0);
    }
    let data_dir = db
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut deleted = 0;
    loop {
        let (dir, in_use) = (data_dir.clone(), in_use.clone());
        let (count, audio) = db
            .call(move |conn| fold_and_delete(conn, &dir, &in_use, before, keep_audio, standing_in))
            .await?;
        if count == 0 {
            break;
        }
        let dir = data_dir.clone();
        let _ = tokio::task::spawn_blocking(move || remove_audio(&dir, &audio)).await;
        deleted += count;
    }
    let known = db.call(capture_ids).await?;
    let orphans = tokio::task::spawn_blocking(move || orphan_audio(&data_dir, &known, before))
        .await
        .unwrap_or(0);
    if deleted > 0 {
        eprintln!("[server] history retention deleted {deleted} captures older than {days} days");
    }
    if orphans > 0 {
        eprintln!("[server] history retention deleted {orphans} audio files with no capture");
    }
    Ok(deleted)
}

/// A capture past the window, as the fold reads it.
struct Expired {
    id: String,
    audio_path: String,
    source: String,
    app_bundle_id: Option<String>,
    raw: Option<String>,
    refined: Option<String>,
    duration_ms: Option<i64>,
    teaches_style_id: Option<String>,
    created_at: NaiveDateTime,
}

/// An integer column SQLite may hold as a real or as text.
fn integer(row: &Row, index: usize) -> rusqlite::Result<Option<i64>> {
    Ok(match row.get_ref(index)? {
        ValueRef::Integer(n) => Some(n),
        ValueRef::Real(f) => Some(f as i64),
        ValueRef::Text(text) => std::str::from_utf8(text)
            .ok()
            .and_then(|t| t.trim().parse().ok()),
        _ => None,
    })
}

/// Captures a stand-in has sent to the shared Kass: neither the capture nor
/// a correction of it waits in `sync_changes`.
const SENT: &str = "AND NOT EXISTS (SELECT 1 FROM sync_changes \
         WHERE kind = 'capture' AND ref_id = captures.id) \
     AND NOT EXISTS (SELECT 1 FROM sync_changes JOIN capture_feedback \
         ON capture_feedback.id = sync_changes.ref_id \
         WHERE sync_changes.kind = 'feedback' AND capture_feedback.capture_id = captures.id)";

/// Up to a batch of captures created before `before`, oldest first, but
/// not the newest capture nor one in `busy`; only ones already sent when
/// `standing_in`.
fn expired(
    conn: &Connection,
    before: NaiveDateTime,
    busy: &[String],
    standing_in: bool,
) -> rusqlite::Result<Vec<Expired>> {
    let skipped = (0..busy.len())
        .map(|n| format!("?{}", n + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sent = if standing_in { SENT } else { "" };
    let sql = format!(
        "SELECT id, audio_path, source, app_bundle_id, transcript_raw, transcript_refined, \
         duration_ms, teaches_style_id, created_at FROM captures \
         WHERE created_at < ?1 AND id IS NOT ( \
             SELECT id FROM captures ORDER BY created_at DESC, id DESC LIMIT 1) \
         AND id NOT IN ({skipped}) {sent} ORDER BY created_at LIMIT {BATCH}"
    );
    let values = std::iter::once(stored(before)).chain(busy.iter().cloned());
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| {
        Ok(Expired {
            id: row.get(0)?,
            audio_path: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            source: row.get(2)?,
            app_bundle_id: row.get(3)?,
            raw: row.get(4)?,
            refined: row.get(5)?,
            duration_ms: integer(row, 6)?,
            teaches_style_id: row.get(7)?,
            created_at: parse_stored(row.get(8)?).unwrap_or_else(now),
        })
    })?;
    rows.collect()
}

/// Fold and delete one batch in one transaction, keeping new holds out
/// meanwhile. How many went, and the audio they leave to delete.
/// Corrections outlive their capture, as they always have in kass.db: no
/// connection checks foreign keys (`db::open`). A stand-in deletes only
/// what it has sent and folds nothing; the deletes aren't recorded either
/// way.
fn fold_and_delete(
    conn: &mut Connection,
    data_dir: &Path,
    in_use: &InUse,
    before: NaiveDateTime,
    keep_audio: bool,
    standing_in: bool,
) -> rusqlite::Result<(usize, Vec<String>)> {
    in_use.holding(|busy| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = expired(&tx, before, &busy, standing_in)?;
        if rows.is_empty() {
            return Ok((0, Vec::new()));
        }
        if !standing_in {
            fold(&tx, data_dir, &rows, keep_audio)?;
        }
        let ids = placeholders(rows.len());
        tx.execute(
            &format!("DELETE FROM captures WHERE id IN ({ids})"),
            params_from_iter(rows.iter().map(|row| &row.id)),
        )?;
        tx.commit()?;
        let count = rows.len();
        let audio = rows
            .into_iter()
            .map(|row| row.audio_path)
            .filter(|path| !path.is_empty())
            .collect();
        Ok((count, audio))
    })
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

/// Words as usage stats count them: runs of non-space.
fn words(text: Option<&str>) -> i64 {
    text.map_or(0, |text| py::split(text).len() as i64)
}

/// The text delivered: the cleaned text unless it's blank, as usage stats
/// take it (`COALESCE(NULLIF(TRIM(refined), ''), raw)`; SQLite's TRIM
/// strips spaces only).
fn delivered(row: &Expired) -> Option<&str> {
    match row.refined.as_deref() {
        Some(refined) if !refined.trim_matches(' ').is_empty() => Some(refined),
        _ => row.raw.as_deref(),
    }
}

/// Move what `rows` taught into lasting storage. The caller commits.
/// `keep_audio` false keeps no recording for corrections: the user keeps
/// none (`discard_audio`).
fn fold(
    tx: &Transaction,
    data_dir: &Path,
    rows: &[Expired],
    keep_audio: bool,
) -> rusqlite::Result<()> {
    let mut reports: BTreeMap<&str, Vec<(String, Option<String>)>> = BTreeMap::new();
    {
        let mut statement = tx.prepare(&format!(
            "SELECT id, capture_id, snapshot FROM capture_feedback WHERE capture_id IN ({})",
            placeholders(rows.len())
        ))?;
        let found =
            statement.query_map(params_from_iter(rows.iter().map(|row| &row.id)), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                ))
            })?;
        for report in found {
            let (id, capture_id, snapshot) = report?;
            if let Some(row) = rows.iter().find(|row| row.id == capture_id) {
                reports.entry(&row.id).or_default().push((id, snapshot));
            }
        }
    }

    let mut names: BTreeMap<String, NaiveDateTime> = BTreeMap::new();
    for row in rows {
        let corrections = reports.get(row.id.as_str()).map_or(&[][..], Vec::as_slice);
        tx.execute(
            "INSERT INTO retired_captures              (capture_id, created_at, source, app_bundle_id, words, raw_words, duration_ms, fixed)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT (capture_id) DO NOTHING",
            params![
                row.id,
                stored(row.created_at),
                row.source,
                row.app_bundle_id,
                words(delivered(row)),
                words(row.raw.as_deref()),
                row.duration_ms,
                !corrections.is_empty(),
            ],
        )?;
        // Known names read cleaned dictation, not a command's rewrite.
        if let Some(refined) = row.refined.as_deref().filter(|text| !text.is_empty()) {
            if row.source != "command" {
                for name in mid_sentence_capitals(refined) {
                    let seen = names.entry(name).or_insert(row.created_at);
                    *seen = (*seen).max(row.created_at);
                }
            }
        }
        let audio = if !corrections.is_empty() && keep_audio {
            keep_correction_audio(data_dir, &row.audio_path)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
        } else {
            None
        };
        for (id, snapshot) in corrections {
            let moved = audio
                .as_deref()
                .and_then(|audio| with_audio(id, snapshot.as_deref(), audio));
            tx.execute(
                "UPDATE capture_feedback SET app_bundle_id = ?2, teaches_style_id = ?3,                  snapshot = coalesce(?4, snapshot) WHERE id = ?1",
                params![id, row.app_bundle_id, row.teaches_style_id, moved],
            )?;
        }
    }

    for (name, seen) in names {
        tx.execute(
            "INSERT INTO known_names (name, last_seen_at) VALUES (?1, ?2)              ON CONFLICT (name) DO UPDATE SET              last_seen_at = max(known_names.last_seen_at, excluded.last_seen_at)",
            params![name, stored(seen)],
        )?;
    }
    Ok(())
}

/// A report's snapshot pointing at `audio`; None, with a warning, when the
/// snapshot isn't a JSON object.
fn with_audio(report_id: &str, snapshot: Option<&str>, audio: &str) -> Option<String> {
    match snapshot.and_then(|text| serde_json::from_str::<Value>(text).ok()) {
        Some(Value::Object(mut snapshot)) => {
            snapshot.insert("audio_path".into(), Value::String(audio.into()));
            Some(Value::Object(snapshot).to_string())
        }
        _ => {
            eprintln!("[server] could not move the audio of correction {report_id}");
            None
        }
    }
}

/// Copy a corrected capture's audio to the correction audio folder; its new
/// stored path, or None when the capture has no audio file.
fn keep_correction_audio(data_dir: &Path, stored_path: &str) -> std::io::Result<Option<String>> {
    let Some(source) = resolve_storage_path(data_dir, stored_path).filter(|path| path.is_file())
    else {
        return Ok(None);
    };
    let Some(name) = source.file_name() else {
        return Ok(None);
    };
    let folder = data_dir.join(CORRECTION_AUDIO);
    std::fs::create_dir_all(&folder)?;
    std::fs::copy(&source, folder.join(name))?;
    Ok(Some(format!(
        "{CORRECTION_AUDIO}/{}",
        name.to_string_lossy()
    )))
}

fn remove_audio(data_dir: &Path, stored_paths: &[String]) {
    for stored_path in stored_paths {
        let Some(path) = resolve_storage_path(data_dir, stored_path).filter(|p| p.is_file()) else {
            continue;
        };
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!(
                "[server] could not remove capture audio {}: {error}",
                path.display()
            );
        }
    }
}

fn capture_ids(conn: &mut Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare("SELECT id FROM captures")?;
    let ids = statement.query_map([], |row| row.get(0))?;
    ids.collect()
}

/// Delete capture audio older than the window that no capture points to.
/// How many files went.
fn orphan_audio(data_dir: &Path, known: &HashSet<String>, before: NaiveDateTime) -> usize {
    let Ok(entries) = std::fs::read_dir(data_dir.join("captures")) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let stem = path.file_stem().map(|stem| stem.to_string_lossy());
        if !metadata.is_file() || stem.is_some_and(|stem| known.contains(stem.as_ref())) {
            continue;
        }
        // A dictation in progress writes its file before its capture exists; it is new.
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if DateTime::<Utc>::from(modified).naive_utc() >= before {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) => eprintln!(
                "[server] could not remove orphan capture audio {}: {error}",
                path.display()
            ),
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Patch;
    use chrono::Duration as Days;

    const SLACK: &str = "com.tinyspeck.slackmacgap";
    const MAIL: &str = "com.apple.mail";
    const ON: Sync = Sync { standing_in: false };
    const STAND_IN: Sync = Sync { standing_in: true };

    /// A data folder with a prepared kass.db, gone when dropped.
    struct Data {
        dir: PathBuf,
        db: Db,
        in_use: Arc<InUse>,
        at: NaiveDateTime,
    }

    impl Drop for Data {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Data {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("kass-retention-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("captures")).unwrap();
            let path = dir.join("kass.db");
            crate::db::prepare(&path).unwrap();
            Data {
                db: Db::new(path),
                dir,
                in_use: Arc::new(InUse::default()),
                at: now(),
            }
        }

        async fn sql<T: Send + 'static>(
            &self,
            work: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        ) -> T {
            self.db.call(work).await.unwrap()
        }

        async fn keep(&self, days: i64) {
            self.patch(Patch {
                history_retention_days: Some(days),
                ..Patch::default()
            })
            .await;
        }

        async fn patch(&self, patch: Patch) {
            self.sql(move |conn| settings::update(conn, patch, ON))
                .await
                .unwrap();
        }

        /// A capture `days_ago` days old with an audio file as old.
        async fn add(&self, capture: Capture) -> PathBuf {
            let audio = self
                .dir
                .join("captures")
                .join(format!("{}.wav", capture.id));
            std::fs::write(&audio, b"RIFF").unwrap();
            let created = self.at - Days::days(capture.days_ago);
            age(&audio, created);
            let stored_audio = format!("captures/{}.wav", capture.id);
            self.sql(move |conn| {
                let refined = capture.refined.map(str::to_string).unwrap_or_else(|| {
                    let mut words = vec!["Tell", "Priya"];
                    words.extend(std::iter::repeat_n("word", capture.words - 2));
                    words.join(" ")
                });
                let raw = capture
                    .said
                    .map(str::to_string)
                    .unwrap_or_else(|| vec!["um"; capture.words + 1].join(" "));
                conn.execute(
                    "INSERT INTO captures (id, audio_path, source, transcript_raw, \
                     transcript_refined, duration_ms, app_bundle_id, app_name, \
                     teaches_style_id, audio_deleted, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, 0, ?9)",
                    params![
                        capture.id,
                        stored_audio,
                        capture.source,
                        raw,
                        refined,
                        4000 + capture.words as i64 * 100,
                        capture.app,
                        capture.style,
                        stored(created),
                    ],
                )?;
                if let Some(meant) = capture.meant {
                    let snapshot = serde_json::json!({
                        "transcript_raw": raw,
                        "transcript_refined": refined,
                        "app_bundle_id": capture.app,
                        "audio_path": stored_audio,
                    });
                    conn.execute(
                        "INSERT INTO capture_feedback \
                         (id, capture_id, target, expected_text, notes, snapshot, created_at) \
                         VALUES (?1, ?2, 'refined', ?3, '', ?4, ?5)",
                        params![
                            format!("report-{}", capture.id),
                            capture.id,
                            meant,
                            snapshot.to_string(),
                            stored(created)
                        ],
                    )?;
                }
                Ok(())
            })
            .await;
            audio
        }

        async fn sweep(&self) -> usize {
            self.sweep_at(self.at).await
        }

        async fn sweep_at(&self, at: NaiveDateTime) -> usize {
            sweep_db(&self.db, &self.in_use, ON, at).await.unwrap()
        }

        async fn sweep_standing_in(&self) -> usize {
            sweep_db(&self.db, &self.in_use, STAND_IN, self.at)
                .await
                .unwrap()
        }

        /// A change waiting to be sent to the shared Kass.
        async fn pending(&self, kind: &'static str, id: &'static str) {
            let at = stored(self.at);
            self.sql(move |conn| {
                conn.execute(
                    "INSERT INTO sync_changes (kind, ref_id, deleted, changed_at) \
                     VALUES (?1, ?2, 0, ?3)",
                    params![kind, id, at],
                )
            })
            .await;
        }

        async fn ids(&self) -> Vec<String> {
            let mut ids: Vec<_> = self.sql(capture_ids).await.into_iter().collect();
            ids.sort();
            ids
        }

        async fn count(&self, table: &'static str) -> i64 {
            self.sql(move |conn| {
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
            })
            .await
        }

        async fn status(&self) -> (i64, bool, i64) {
            let at = self.at;
            self.sql(move |conn| settings::retention_status_at(conn, ON, at))
                .await
        }
    }

    fn age(path: &Path, at: NaiveDateTime) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(at.and_utc().into()).unwrap();
    }

    /// A retired_captures row: id, source, app, words, raw words, duration, fixed.
    type Retired = (String, String, Option<String>, i64, i64, i64, bool);

    struct Capture {
        id: &'static str,
        days_ago: i64,
        app: &'static str,
        words: usize,
        source: &'static str,
        style: Option<&'static str>,
        said: Option<&'static str>,
        meant: Option<&'static str>,
        refined: Option<&'static str>,
    }

    fn capture(id: &'static str, days_ago: i64) -> Capture {
        Capture {
            id,
            days_ago,
            app: MAIL,
            words: 5,
            source: "dictation",
            style: None,
            said: None,
            meant: None,
            refined: None,
        }
    }

    fn sorted(ids: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn old_captures_and_their_audio_go_and_their_corrections_keep_teaching() {
        let data = Data::new();
        let old_audio = data
            .add(Capture {
                app: SLACK,
                style: Some("work"),
                said: Some("ship it to voice box"),
                meant: Some("Ship it to Voicebox."),
                ..capture("old-fixed", 40)
            })
            .await;
        let plain_audio = data.add(capture("old-plain", 35)).await;
        data.add(capture("recent", 2)).await;
        data.keep(30).await;

        assert_eq!(data.sweep().await, 2);

        assert_eq!(data.ids().await, sorted(&["recent"]));
        assert!(!old_audio.exists());
        assert!(!plain_audio.exists());
        // The correction stays, pointing at its app, its style and its moved audio.
        let (capture_id, app, style, snapshot): (String, Option<String>, Option<String>, String) = data
            .sql(|conn| {
                conn.query_row(
                    "SELECT capture_id, app_bundle_id, teaches_style_id, snapshot FROM capture_feedback",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
            })
            .await;
        assert_eq!(capture_id, "old-fixed");
        assert_eq!(app.as_deref(), Some(SLACK));
        assert_eq!(style.as_deref(), Some("work"));
        let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(snapshot["audio_path"], "correction-audio/old-fixed.wav");
        assert_eq!(snapshot["transcript_raw"], "ship it to voice box");
        assert!(data.dir.join("correction-audio/old-fixed.wav").is_file());
        // Names they wrote are still known.
        let names = data.sql(|conn| crate::known_names::read(conn)).await;
        assert!(names.contains("Priya"));
    }

    #[tokio::test]
    async fn correction_audio_is_kept_only_while_recordings_are_kept() {
        let data = Data::new();
        data.add(Capture {
            said: Some("send the deck"),
            meant: Some("Send the deck."),
            ..capture("old-fixed", 40)
        })
        .await;
        data.add(capture("recent", 1)).await;
        data.keep(30).await;
        data.patch(Patch {
            discard_audio: Some(true),
            ..Patch::default()
        })
        .await;

        assert_eq!(data.sweep().await, 1);

        assert!(!data.dir.join(CORRECTION_AUDIO).exists());
        let snapshot: String = data
            .sql(|conn| {
                conn.query_row("SELECT snapshot FROM capture_feedback", [], |row| {
                    row.get(0)
                })
            })
            .await;
        let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(snapshot["audio_path"], "captures/old-fixed.wav");
    }

    #[tokio::test]
    async fn usage_stats_keep_the_numbers_of_deleted_captures() {
        let data = Data::new();
        data.keep(7).await;
        data.add(Capture {
            app: SLACK,
            words: 12,
            ..capture("dictated", 20)
        })
        .await;
        data.add(Capture {
            said: Some("a b"),
            meant: Some("A b."),
            ..capture("fixed", 8)
        })
        .await;
        data.add(Capture {
            source: "command",
            refined: Some("Ein Satz von Anna"),
            ..capture("command", 12)
        })
        .await;
        data.add(Capture {
            refined: Some("   "),
            said: Some("only the raw words"),
            ..capture("blank", 10)
        })
        .await;
        data.add(capture("recent", 0)).await;

        assert_eq!(data.sweep().await, 4);

        let retired: Vec<Retired> = data
            .sql(|conn| {
                let mut statement = conn.prepare(
                    "SELECT capture_id, source, app_bundle_id, words, raw_words, duration_ms, fixed \
                     FROM retired_captures ORDER BY created_at",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })?;
                rows.collect()
            })
            .await;
        let row = |id: &str, source: &str, app: &str, words, raw, ms, fixed| {
            (
                id.to_string(),
                source.to_string(),
                Some(app.to_string()),
                words,
                raw,
                ms,
                fixed,
            )
        };
        assert_eq!(
            retired,
            vec![
                row("dictated", "dictation", SLACK, 12, 13, 5200, false),
                row("command", "command", MAIL, 4, 6, 4500, false),
                row("blank", "dictation", MAIL, 4, 4, 4500, false),
                row("fixed", "dictation", MAIL, 5, 2, 4500, true),
            ]
        );
        // A command's output isn't the user's writing.
        let kept: Vec<String> = data
            .sql(|conn| {
                let mut statement = conn.prepare("SELECT name FROM known_names ORDER BY name")?;
                let names = statement.query_map([], |row| row.get(0))?;
                names.collect()
            })
            .await;
        assert_eq!(kept, vec!["Priya".to_string()]);
    }

    #[tokio::test]
    async fn sweeping_again_changes_nothing() {
        let data = Data::new();
        data.keep(7).await;
        for days_ago in [30, 20, 10, 1] {
            let id: &'static str = Box::leak(format!("c{days_ago}").into_boxed_str());
            data.add(capture(id, days_ago)).await;
        }
        assert_eq!(data.sweep().await, 3);
        assert_eq!(data.sweep().await, 0);
        assert_eq!(data.count("retired_captures").await, 3);
        let seen: String = data
            .sql(|conn| {
                conn.query_row(
                    "SELECT last_seen_at FROM known_names WHERE name = 'Priya'",
                    [],
                    |row| row.get(0),
                )
            })
            .await;
        assert_eq!(seen, stored(data.at - Days::days(10)));
    }

    #[tokio::test]
    async fn a_failed_fold_loses_nothing_and_counts_nothing_twice() {
        let data = Data::new();
        data.keep(7).await;
        let audio = data
            .add(Capture {
                said: Some("the quick fox"),
                meant: Some("The quick fox."),
                ..capture("old", 30)
            })
            .await;
        data.add(capture("new", 1)).await;
        // The correction audio can't be copied: a file sits where its folder goes.
        std::fs::write(data.dir.join(CORRECTION_AUDIO), b"").unwrap();

        assert!(sweep_db(&data.db, &data.in_use, ON, data.at).await.is_err());

        assert_eq!(data.ids().await, sorted(&["new", "old"]));
        assert!(audio.exists());
        assert_eq!(data.count("retired_captures").await, 0);
        assert_eq!(data.count("known_names").await, 0);
        let app: Option<String> = data
            .sql(|conn| {
                conn.query_row("SELECT app_bundle_id FROM capture_feedback", [], |row| {
                    row.get(0)
                })
            })
            .await;
        assert_eq!(app, None);

        std::fs::remove_file(data.dir.join(CORRECTION_AUDIO)).unwrap();
        assert_eq!(data.sweep().await, 1);
        assert_eq!(data.count("retired_captures").await, 1);
        assert!(!audio.exists());
    }

    #[tokio::test]
    async fn audio_left_behind_goes_once_it_is_past_the_window() {
        let data = Data::new();
        data.keep(7).await;
        data.add(capture("current", 1)).await;
        // A sweep that stopped after its commit leaves a file with no capture.
        let left = data.dir.join("captures/gone.wav");
        std::fs::write(&left, b"RIFF").unwrap();
        age(&left, data.at - Days::days(30));
        // A dictation in progress writes its file before its capture exists.
        let in_progress = data.dir.join("captures/streaming.wav");
        std::fs::write(&in_progress, b"RIFF").unwrap();

        assert_eq!(data.sweep().await, 0);

        assert!(!left.exists());
        assert!(in_progress.exists());
        assert!(data.dir.join("captures/current.wav").exists());
    }

    #[tokio::test]
    async fn keep_forever_deletes_nothing() {
        let data = Data::new();
        data.keep(0).await;
        for (id, days_ago) in [("c3000", 3000), ("c400", 400), ("c1", 1)] {
            data.add(capture(id, days_ago)).await;
        }
        assert_eq!(data.status().await, (0, true, 0));
        assert_eq!(data.sweep().await, 0);
        assert_eq!(data.ids().await, sorted(&["c1", "c400", "c3000"]));
    }

    #[tokio::test]
    async fn the_current_take_and_captures_in_use_stay() {
        let data = Data::new();
        data.keep(7).await;
        data.add(capture("busy", 50)).await;
        data.add(capture("old", 40)).await;
        data.add(capture("current", 30)).await;
        assert_eq!(data.status().await, (7, true, 2));
        {
            let _held = super::super::in_use::hold(&data.in_use, "busy");
            let _again = super::super::in_use::hold(&data.in_use, "busy");
            assert_eq!(data.sweep().await, 1);
            assert_eq!(data.ids().await, sorted(&["busy", "current"]));
            drop(_again);
            assert_eq!(data.sweep().await, 0);
        }
        assert_eq!(data.sweep().await, 1);
        assert_eq!(data.ids().await, sorted(&["current"]));
    }

    #[tokio::test]
    async fn an_unconfirmed_window_deletes_nothing() {
        let data = Data::new();
        let audio = data.add(capture("old", 40)).await;
        data.add(capture("new", 1)).await;
        assert_eq!(data.sweep().await, 0);
        assert_eq!(data.ids().await, sorted(&["new", "old"]));
        assert!(audio.exists());
        assert_eq!(data.status().await, (30, false, 1));
        // Keeping the default, as the launch dialog's first choice does.
        data.keep(30).await;
        assert_eq!(data.sweep().await, 1);
        assert_eq!(data.ids().await, sorted(&["new"]));
    }

    #[tokio::test]
    async fn nothing_to_delete_confirms_without_asking() {
        let data = Data::new();
        data.add(capture("first", 3)).await;
        assert_eq!(data.sweep().await, 0);
        assert_eq!(data.status().await, (30, true, 0));
        // Once confirmed, captures that age past the window go without asking.
        data.add(capture("later", -40)).await;
        assert_eq!(data.sweep_at(data.at + Days::days(40)).await, 1);
        assert_eq!(data.ids().await, sorted(&["later"]));
    }
    #[tokio::test]
    async fn a_stand_in_prunes_only_what_it_has_sent_and_records_nothing() {
        let data = Data::new();
        data.keep(30).await;
        let sent_audio = data.add(capture("sent", 40)).await;
        data.add(Capture {
            said: Some("send the deck"),
            meant: Some("Send the deck."),
            ..capture("sent-fixed", 39)
        })
        .await;
        data.add(capture("unsent", 38)).await;
        data.pending("capture", "unsent").await;
        data.add(Capture {
            said: Some("call ana"),
            meant: Some("Call Ana."),
            ..capture("fix-unsent", 37)
        })
        .await;
        data.pending("feedback", "report-fix-unsent").await;
        data.add(capture("recent", 2)).await;

        assert_eq!(data.sweep_standing_in().await, 2);

        assert_eq!(
            data.ids().await,
            sorted(&["fix-unsent", "recent", "unsent"])
        );
        assert!(!sent_audio.exists());
        // Only the two changes still waiting; the deletes aren't sent.
        assert_eq!(data.count("sync_changes").await, 2);
        // The shared Kass folds its own copies.
        assert_eq!(data.count("retired_captures").await, 0);
        assert_eq!(data.count("known_names").await, 0);
        assert!(!data.dir.join(CORRECTION_AUDIO).exists());
        // Corrections stay, as they do when the shared Kass deletes.
        assert_eq!(data.count("capture_feedback").await, 2);

        // Once sent, they go too.
        data.sql(|conn| conn.execute("DELETE FROM sync_changes", []))
            .await;
        assert_eq!(data.sweep_standing_in().await, 2);
        assert_eq!(data.ids().await, sorted(&["recent"]));
        assert_eq!(data.count("sync_changes").await, 0);
    }

    #[tokio::test]
    async fn a_stand_in_waits_for_the_shared_kass_to_confirm_the_window() {
        let data = Data::new();
        data.add(capture("old", 40)).await;
        data.add(capture("new", 1)).await;

        assert_eq!(data.sweep_standing_in().await, 0);

        assert_eq!(data.ids().await, sorted(&["new", "old"]));
        let confirmed = data
            .sql(|conn| settings::get(conn).map(|s| s.history_retention_confirmed))
            .await;
        assert!(!confirmed);
        assert_eq!(data.count("sync_changes").await, 0);
    }
}
