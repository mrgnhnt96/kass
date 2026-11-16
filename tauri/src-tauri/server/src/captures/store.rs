//! Captures and their reports in `kass.db` (`captures`, `capture_feedback`),
//! as the Python worker created them. Every write runs in one transaction
//! and, while this Mac stands in for a shared Kass, records each row it
//! changed in `sync_changes` the way the worker's `services/sync/changes.py`
//! does.

use chrono::NaiveDateTime;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use super::{stored_flags, stored_review, Capture};
use crate::dictionary::store::{now, parse_stored, stored, Sync};

const COLUMNS: &str = "id, audio_path, source, language, duration_ms, transcript_raw, \
                       transcript_refined, stt_model, llm_model, refinement_flags, \
                       refinement_review, app_bundle_id, app_name, command_selection, \
                       command_instruction, command_transform, style_id, audio_deleted, \
                       mac_id, mac_name, created_at";

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

fn capture(row: &Row) -> rusqlite::Result<Capture> {
    let flags: Option<String> = row.get(9)?;
    let review: Option<String> = row.get(10)?;
    Ok(Capture {
        id: row.get(0)?,
        audio_path: row.get(1)?,
        source: row.get(2)?,
        language: row.get(3)?,
        duration_ms: integer(row, 4)?,
        transcript_raw: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        transcript_refined: row.get(6)?,
        stt_model: row.get(7)?,
        llm_model: row.get(8)?,
        refinement_flags: stored_flags(flags.as_deref()),
        refinement_review: stored_review(review.as_deref()),
        app_bundle_id: row.get(11)?,
        app_name: row.get(12)?,
        command_selection: row.get(13)?,
        command_instruction: row.get(14)?,
        command_transform: row.get(15)?,
        style_id: row.get(16)?,
        audio_deleted: integer(row, 17)?.unwrap_or(0) != 0,
        mac_id: row.get(18)?,
        mac_name: row.get(19)?,
        created_at: parse_stored(row.get(20)?),
    })
}

/// Which captures a list keeps.
#[derive(Clone, Debug)]
pub enum Apps {
    All,
    /// Those with no app recorded: uploads, and dictation from before apps
    /// were saved.
    Unknown,
    One(String),
}

/// The newest captures first, and how many there are in all.
pub fn list(
    conn: &Connection,
    limit: i64,
    offset: i64,
    apps: &Apps,
) -> rusqlite::Result<(Vec<Capture>, i64)> {
    let (filter, app) = match apps {
        Apps::All => ("", None),
        Apps::Unknown => ("WHERE app_bundle_id IS NULL", None),
        Apps::One(bundle_id) => ("WHERE app_bundle_id = ?3", Some(bundle_id.as_str())),
    };
    let total: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM captures {}",
            filter.replace("?3", "?1")
        ),
        rusqlite::params_from_iter(app),
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM captures {filter} ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
    ))?;
    let items = match app {
        Some(bundle_id) => statement.query_map(params![limit, offset, bundle_id], capture)?,
        None => statement.query_map(params![limit, offset], capture)?,
    }
    .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((items, total))
}

pub fn get(conn: &Connection, capture_id: &str) -> rusqlite::Result<Option<Capture>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM captures WHERE id = ?1"),
        [capture_id],
        capture,
    )
    .optional()
}

/// What a write changed, by sync kind, recorded as one batch.
#[derive(Default)]
struct Changes(Vec<(&'static str, String, bool)>);

impl Changes {
    fn changed(&mut self, kind: &'static str, id: &str) {
        self.0.push((kind, id.to_string(), false));
    }

    fn deleted(&mut self, kind: &'static str, id: &str) {
        self.0.push((kind, id.to_string(), true));
    }

    fn record(self, tx: &Transaction, sync: Sync, at: NaiveDateTime) -> rusqlite::Result<()> {
        if !sync.standing_in {
            return Ok(());
        }
        let mut statement = tx.prepare(
            "INSERT INTO sync_changes (kind, ref_id, deleted, changed_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (kind, ref_id) DO UPDATE SET deleted = excluded.deleted, changed_at = excluded.changed_at",
        )?;
        for (kind, id, deleted) in self.0 {
            statement.execute(params![kind, id, deleted, stored(at)])?;
        }
        Ok(())
    }
}

/// A capture a dictation made, as its session saves it.
#[derive(Clone, Debug, Default)]
pub struct NewCapture {
    pub id: String,
    pub audio_path: String,
    pub source: String,
    pub language: Option<String>,
    pub duration_ms: i64,
    pub transcript_raw: String,
    pub transcript_refined: Option<String>,
    pub stt_model: Option<String>,
    pub llm_model: Option<String>,
    /// JSON, as the worker saved it.
    pub refinement_flags: Option<String>,
    pub refinement_review: Option<String>,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub app_category: Option<String>,
    pub command_selection: Option<String>,
    pub command_instruction: Option<String>,
    pub command_transform: Option<String>,
    pub style_id: Option<String>,
    pub teaches_style_id: Option<String>,
}

/// Save a new capture, and return it as stored.
pub fn insert_capture(
    conn: &mut Connection,
    new: &NewCapture,
    sync: Sync,
) -> rusqlite::Result<Capture> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let at = now();
    tx.execute(
        "INSERT INTO captures (id, audio_path, source, language, duration_ms, transcript_raw, \
         transcript_refined, stt_model, llm_model, refinement_flags, refinement_review, \
         app_bundle_id, app_name, app_category, command_selection, command_instruction, \
         command_transform, style_id, teaches_style_id, audio_deleted, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        params![
            new.id,
            new.audio_path,
            new.source,
            new.language,
            new.duration_ms,
            new.transcript_raw,
            new.transcript_refined,
            new.stt_model,
            new.llm_model,
            new.refinement_flags,
            new.refinement_review,
            new.app_bundle_id,
            new.app_name,
            new.app_category,
            new.command_selection,
            new.command_instruction,
            new.command_transform,
            new.style_id,
            new.teaches_style_id,
            new.audio_path.is_empty(),
            stored(at),
        ],
    )?;
    let mut changes = Changes::default();
    changes.changed("capture", &new.id);
    changes.record(&tx, sync, at)?;
    let capture = get(&tx, &new.id)?.expect("the capture was just saved");
    tx.commit()?;
    Ok(capture)
}

// -- reports ------------------------------------------------------------------

/// One `capture_feedback` row.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub id: String,
    pub capture_id: String,
    pub target: String,
    pub expected_text: String,
    pub notes: String,
    /// The capture as the user saw it, as JSON.
    pub snapshot: String,
    pub source: String,
    pub created_at: Option<NaiveDateTime>,
}

const REPORT_COLUMNS: &str =
    "id, capture_id, target, expected_text, notes, snapshot, source, created_at";

fn report(row: &Row) -> rusqlite::Result<Report> {
    Ok(Report {
        id: row.get(0)?,
        capture_id: row.get(1)?,
        target: row.get(2)?,
        expected_text: row.get(3)?,
        notes: row.get(4)?,
        snapshot: row.get(5)?,
        source: row
            .get::<_, Option<String>>(6)?
            .unwrap_or_else(|| "manual".into()),
        created_at: parse_stored(row.get(7)?),
    })
}

/// Every report, or one capture's: newest first.
pub fn list_reports(conn: &Connection, capture_id: Option<&str>) -> rusqlite::Result<Vec<Report>> {
    let filter = if capture_id.is_some() {
        "WHERE capture_id = ?1"
    } else {
        ""
    };
    let mut statement = conn.prepare(&format!(
        "SELECT {REPORT_COLUMNS} FROM capture_feedback {filter} ORDER BY created_at DESC, id"
    ))?;
    let rows = statement.query_map(rusqlite::params_from_iter(capture_id), report)?;
    rows.collect()
}

fn get_report(conn: &Connection, report_id: &str) -> rusqlite::Result<Option<Report>> {
    conn.query_row(
        &format!("SELECT {REPORT_COLUMNS} FROM capture_feedback WHERE id = ?1"),
        [report_id],
        report,
    )
    .optional()
}

/// The capture's newest report of `target`: its text as corrected so far.
pub fn latest_report(
    conn: &Connection,
    capture_id: &str,
    target: &str,
) -> rusqlite::Result<Option<Report>> {
    conn.query_row(
        &format!(
            "SELECT {REPORT_COLUMNS} FROM capture_feedback WHERE capture_id = ?1 AND target = ?2 \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        ),
        [capture_id, target],
        report,
    )
    .optional()
}

/// A report to keep, checked against the capture it corrects.
pub struct NewReport {
    pub capture_id: String,
    pub target: String,
    pub expected_text: String,
    pub notes: String,
    pub source: String,
    /// The report of the same capture and target this one amends.
    pub replaces: Option<String>,
    /// The capture as the request's checks read it.
    pub seen: Capture,
    /// The voice edit capture that filed it, which takes it back when deleted.
    pub filed_by: Option<String>,
}

/// Why a report wasn't kept.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// The capture is gone.
    NoCapture,
    Changed,
    AmendedGone,
}

impl Refused {
    pub fn message(&self) -> &'static str {
        match self {
            Refused::NoCapture => "Capture not found",
            Refused::Changed => "Capture changed. Refresh it before reporting a correction.",
            Refused::AmendedGone => "The correction being amended is gone. Refresh the capture.",
        }
    }
}

pub struct Saved {
    pub report: Report,
    /// The report it amended, gone now.
    pub replaced: Option<Report>,
}

/// Keep a report, in place of the one it amends, in one transaction.
pub fn save_report(
    conn: &mut Connection,
    new: NewReport,
    sync: Sync,
) -> rusqlite::Result<Result<Saved, Refused>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match get(&tx, &new.capture_id)? {
        None => return Ok(Err(Refused::NoCapture)),
        Some(capture) if capture != new.seen => return Ok(Err(Refused::Changed)),
        Some(_) => {}
    }
    let mut changes = Changes::default();
    let replaced = match &new.replaces {
        None => None,
        Some(id) => match get_report(&tx, id)? {
            Some(old) if old.capture_id == new.capture_id && old.target == new.target => {
                tx.execute("DELETE FROM capture_feedback WHERE id = ?1", [id])?;
                changes.deleted("feedback", id);
                Some(old)
            }
            _ => return Ok(Err(Refused::AmendedGone)),
        },
    };
    let at = now();
    let row = Report {
        id: uuid::Uuid::new_v4().to_string(),
        capture_id: new.capture_id,
        target: new.target,
        expected_text: new.expected_text,
        notes: new.notes,
        snapshot: serde_json::to_string(&new.seen).expect("a capture is JSON"),
        source: new.source,
        created_at: Some(at),
    };
    tx.execute(
        "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot, source, created_at, filed_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.id,
            row.capture_id,
            row.target,
            row.expected_text,
            row.notes,
            row.snapshot,
            row.source,
            stored(at),
            new.filed_by
        ],
    )?;
    changes.changed("feedback", &row.id);
    changes.record(&tx, sync, at)?;
    tx.commit()?;
    Ok(Ok(Saved {
        report: row,
        replaced,
    }))
}

/// A report and the capture's reports of the same target, oldest first,
/// with where it is among them; None if the capture has no such report.
pub fn stack(
    conn: &Connection,
    capture_id: &str,
    report_id: &str,
) -> rusqlite::Result<Option<(Vec<Report>, usize)>> {
    let Some(row) = get_report(conn, report_id)? else {
        return Ok(None);
    };
    if row.capture_id != capture_id {
        return Ok(None);
    }
    let mut statement = conn.prepare(&format!(
        "SELECT {REPORT_COLUMNS} FROM capture_feedback WHERE capture_id = ?1 AND target = ?2 \
         ORDER BY created_at, id"
    ))?;
    let stack = statement
        .query_map([capture_id, &row.target], report)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let at = stack.iter().position(|other| other.id == row.id);
    Ok(at.map(|at| (stack, at)))
}

/// Delete a report, and set each newer one's text (None deletes it), in one
/// transaction. Whether the report was still there.
pub fn withdraw(
    conn: &mut Connection,
    report_id: &str,
    newer: &[(String, Option<String>)],
    sync: Sync,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tx.execute("DELETE FROM capture_feedback WHERE id = ?1", [report_id])? == 0 {
        return Ok(false);
    }
    let mut changes = Changes::default();
    for (id, text) in newer {
        let updated = match text {
            Some(text) => tx.execute(
                "UPDATE capture_feedback SET expected_text = ?2 WHERE id = ?1",
                params![id, text],
            )?,
            None => tx.execute("DELETE FROM capture_feedback WHERE id = ?1", [id])?,
        };
        if updated > 0 {
            match text {
                Some(_) => changes.changed("feedback", id),
                None => changes.deleted("feedback", id),
            }
        }
    }
    changes.deleted("feedback", report_id);
    changes.record(&tx, sync, now())?;
    tx.commit()?;
    Ok(true)
}

/// A report a deleted capture took with it: its id, target and source.
pub type Forgotten = (String, String, String);

/// Delete a capture with its own reports and those it filed as a voice
/// edit. The reports, or None if the capture was already gone.
pub fn delete(
    conn: &mut Connection,
    capture_id: &str,
    sync: Sync,
) -> rusqlite::Result<Option<Vec<Forgotten>>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let reports = {
        let mut statement = tx.prepare(
            "SELECT id, target, coalesce(source, 'manual') FROM capture_feedback \
             WHERE capture_id = ?1 OR filed_by = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([capture_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<Forgotten>>>()?
    };
    let mut changes = Changes::default();
    for (id, _, _) in &reports {
        tx.execute("DELETE FROM capture_feedback WHERE id = ?1", [id])?;
        changes.deleted("feedback", id);
    }
    if tx.execute("DELETE FROM captures WHERE id = ?1", [capture_id])? == 0 {
        return Ok(None);
    }
    changes.deleted("capture", capture_id);
    changes.record(&tx, sync, now())?;
    tx.commit()?;
    Ok(Some(reports))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const STAND_IN: Sync = Sync { standing_in: true };

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../tests/kass_schema.sql"))
            .unwrap();
        conn.execute_batch(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, transcript_refined,
                 audio_deleted, refinement_flags, app_bundle_id, created_at)
             VALUES ('take', 'captures/take.wav', 'dictation', 'wrong words', 'Wrong words.', 0,
                     '{\"style\": \"code\"}', 'com.apple.mail', '2026-03-04 05:06:07.000000'),
                    ('older', 'captures/older.wav', 'file', 'hello', NULL, 1, NULL, NULL,
                     '2026-03-03 05:06:07.000000');",
        )
        .unwrap();
        conn
    }

    fn new_report(conn: &Connection, text: &str) -> NewReport {
        NewReport {
            capture_id: "take".into(),
            target: "raw".into(),
            expected_text: text.into(),
            notes: String::new(),
            source: "manual".into(),
            replaces: None,
            seen: get(conn, "take").unwrap().unwrap(),
            filed_by: None,
        }
    }

    fn save(conn: &mut Connection, text: &str) -> Saved {
        let new = new_report(conn, text);
        save_report(conn, new, STAND_IN).unwrap().unwrap()
    }

    fn changes(conn: &Connection) -> Vec<(String, String, bool)> {
        let mut statement = conn
            .prepare("SELECT kind, ref_id, deleted FROM sync_changes ORDER BY kind, ref_id")
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    #[test]
    fn lists_newest_first_and_by_app() {
        let conn = database();
        let (items, total) = list(&conn, 50, 0, &Apps::All).unwrap();
        assert_eq!((items.len(), total), (2, 2));
        assert_eq!(items[0].id, "take");
        assert_eq!(
            items[0].refinement_flags.as_ref().unwrap().style.as_deref(),
            Some("code")
        );
        assert!(items[1].audio_deleted);
        let (items, total) = list(&conn, 1, 1, &Apps::All).unwrap();
        assert_eq!((items[0].id.as_str(), total), ("older", 2));
        let (items, total) = list(&conn, 50, 0, &Apps::One("com.apple.mail".into())).unwrap();
        assert_eq!((items[0].id.as_str(), total), ("take", 1));
        let (items, total) = list(&conn, 50, 0, &Apps::Unknown).unwrap();
        assert_eq!((items[0].id.as_str(), total), ("older", 1));
    }

    #[test]
    fn a_saved_report_keeps_the_capture_as_seen() {
        let mut conn = database();
        let saved = save(&mut conn, "Right words");
        let seen: serde_json::Value = serde_json::from_str(&saved.report.snapshot).unwrap();
        assert_eq!(seen["transcript_raw"], json!("wrong words"));
        assert_eq!(seen["created_at"], json!("2026-03-04T05:06:07"));
        let mut problems = crate::captures::Problems::default();
        let read = crate::captures::snapshot(&seen, &[], &mut problems).unwrap();
        assert!(read.is(&get(&conn, "take").unwrap().unwrap()));
        assert_eq!(
            list_reports(&conn, Some("take")).unwrap(),
            vec![saved.report.clone()]
        );
        assert_eq!(
            changes(&conn),
            vec![("feedback".into(), saved.report.id, false)]
        );
    }

    #[test]
    fn a_report_on_a_changed_capture_or_a_gone_one_is_refused() {
        let mut conn = database();
        let mut stale = new_report(&conn, "Right words");
        stale.seen.transcript_raw = "other".into();
        assert_eq!(
            save_report(&mut conn, stale, STAND_IN).unwrap().err(),
            Some(Refused::Changed)
        );
        let mut amends = new_report(&conn, "Right words");
        amends.replaces = Some("gone".into());
        assert_eq!(
            save_report(&mut conn, amends, STAND_IN).unwrap().err(),
            Some(Refused::AmendedGone)
        );
        assert!(list_reports(&conn, None).unwrap().is_empty());
        assert!(changes(&conn).is_empty());
    }

    #[test]
    fn an_amended_report_replaces_the_one_it_amends() {
        let mut conn = database();
        let first = save(&mut conn, "Right words").report;
        let mut amended = new_report(&conn, "Right words, Postgres");
        amended.replaces = Some(first.id.clone());
        let saved = save_report(&mut conn, amended, STAND_IN).unwrap().unwrap();
        assert_eq!(saved.replaced.unwrap().id, first.id);
        assert_eq!(
            list_reports(&conn, None).unwrap(),
            vec![saved.report.clone()]
        );
        let mut other = new_report(&conn, "Right");
        other.target = "refined".into();
        other.replaces = Some(saved.report.id.clone());
        assert_eq!(
            save_report(&mut conn, other, STAND_IN).unwrap().err(),
            Some(Refused::AmendedGone)
        );
        assert!(changes(&conn).contains(&("feedback".into(), first.id, true)));
    }

    #[test]
    fn withdrawing_sets_the_newer_reports_and_deletes_it() {
        let mut conn = database();
        let mut ids = Vec::new();
        for text in ["right words", "right words.", "right words!"] {
            let saved = save(&mut conn, text);
            ids.push(saved.report.id);
        }
        let (stack, at) = stack(&conn, "take", &ids[1]).unwrap().unwrap();
        assert_eq!(at, 1);
        assert_eq!(stack.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), ids);
        assert!(super::stack(&conn, "older", &ids[1]).unwrap().is_none());
        let newer = vec![(ids[2].clone(), None)];
        assert!(withdraw(&mut conn, &ids[1], &newer, STAND_IN).unwrap());
        assert!(!withdraw(&mut conn, &ids[1], &newer, STAND_IN).unwrap());
        let left: Vec<_> = list_reports(&conn, None)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(left, vec![ids[0].clone()]);
        let newer = vec![(ids[0].clone(), Some("bright words".into()))];
        assert!(withdraw(&mut conn, &ids[0], &[], STAND_IN).unwrap());
        assert!(!withdraw(&mut conn, "gone", &newer, STAND_IN).unwrap());
        assert!(changes(&conn).iter().all(|(_, _, deleted)| *deleted));
    }

    #[test]
    fn deleting_a_capture_takes_its_reports_and_those_it_filed() {
        let mut conn = database();
        let own = save(&mut conn, "Right words").report;
        conn.execute(
            "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot, source, filed_by)
             VALUES ('filed', 'older', 'refined', 'x', '', '{}', 'voice_fix', 'take')",
            [],
        )
        .unwrap();
        let forgotten = delete(&mut conn, "take", STAND_IN).unwrap().unwrap();
        assert_eq!(forgotten.len(), 2);
        assert!(forgotten.contains(&(own.id.clone(), "raw".into(), "manual".into())));
        assert!(forgotten.contains(&("filed".into(), "refined".into(), "voice_fix".into())));
        assert!(list_reports(&conn, None).unwrap().is_empty());
        assert!(get(&conn, "take").unwrap().is_none());
        assert_eq!(delete(&mut conn, "take", STAND_IN).unwrap(), None);
        assert!(changes(&conn).contains(&("capture".into(), "take".into(), true)));
    }
}
