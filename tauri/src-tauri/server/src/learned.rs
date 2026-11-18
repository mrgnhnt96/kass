//! What each writing style learned, kept in two files next to `kass.db`:
//! `writing-style.json` (taught replies, punctuation habits counted from
//! corrections, hidden examples) and `correction-notes.json` (rules summarized
//! from older examples).
//!
//! This server owns both files. Every change here is made under [`FILES`],
//! written whole, announced as `learned`, and followed by
//! `worker::learned_changed` so the worker rereads them before it next
//! cleans up. Python: `backend/services/writing_style.py`,
//! `personal_examples.py` and `correction_notes.py`.
//!
//! Two writers are still in the worker: a teach session's finish saves its
//! replies (`writing_style.save_run`), and the notes job saves new notes.
//! The teach finish is forwarded while holding [`FILES`]
//! (`routes::writing_style`), so it can't land between a read and a write
//! here.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use kass_text::py;
use kass_text::writing_style::{observe, summary, Counts, Profile};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::captures::EXPLICIT_SOURCES;
use crate::cleanup_inputs::{self, PersonalExample};
use crate::events::Kind;
use crate::styles::{self, Snapshot, MIGRATED_STYLE};
use crate::{worker, AppState};

pub const WRITING_STYLE: &str = "writing-style.json";
pub const CORRECTION_NOTES: &str = "correction-notes.json";

/// Bumped when `observe` counts corrections differently, so saved counts
/// are recounted at start. 2: corrections that only fix words count nothing.
pub const COUNTING: i64 = 2;

/// The newest corrections the habits are counted from.
const COUNTED_CORRECTIONS: usize = 200;
/// A longer correction isn't counted.
const MAX_COUNTED_CHARS: usize = 2000;
/// Hidden example ids kept.
const MAX_HIDDEN: usize = 1000;
/// Earlier note lists kept.
const MAX_NOTES_HISTORY: usize = 10;

/// One change to the learned files at a time, across servers too: sharing
/// restarts the server, and the old one may still be finishing a change.
pub static FILES: Mutex<()> = Mutex::const_new(());

/// The folder `kass.db` and the learned files are in.
pub fn data_dir(state: &AppState) -> PathBuf {
    state
        .db
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Write the whole file at once, as Python's `json.dumps(indent=2)` would:
/// a dictation never reads half of it.
fn save(path: &Path, state: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tmp");
    let written = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    let _ = fs::remove_file(&temporary);
    written
}

// -- writing-style.json --------------------------------------------------------

fn empty_style() -> Value {
    json!({
        "runs": 0,
        "last_run_at": null,
        "examples": [],
        "feedback_counts": Counts::default().to_json(),
    })
}

/// `saved` over `empty`, as Python's `{**empty, **saved}`.
fn over(mut empty: Value, saved: &Value) -> Value {
    if let Some(saved) = saved.as_object() {
        for (key, value) in saved {
            empty[key] = value.clone();
        }
    }
    empty
}

/// `style_id`'s saved profile over the empty one.
fn profile_json(writing: &Value, style_id: &str) -> Value {
    over(empty_style(), &writing["styles"][style_id])
}

fn styles_mut(writing: &mut Value) -> &mut Map<String, Value> {
    if !writing["styles"].is_object() {
        writing["styles"] = json!({});
    }
    writing["styles"]
        .as_object_mut()
        .expect("styles is an object")
}

/// What `style_id` learned, as `GET /writing-style` answers it.
pub fn status(writing: &Value, style_id: &str) -> Value {
    let saved = profile_json(writing, style_id);
    let profile = Profile::from_json(&saved);
    json!({
        "ready": profile.is_ready(),
        "runs": profile.runs,
        "last_run_at": saved["last_run_at"],
        "example_count": saved["examples"].as_array().map_or(0, Vec::len),
        "habits": summary(&profile.habits()),
    })
}

/// Forget `style_id`'s taught replies and habits; other styles keep theirs.
fn reset(writing: &mut Value, style_id: &str) {
    styles_mut(writing).insert(style_id.to_string(), empty_style());
}

/// Hide an example from every list and prompt.
fn hide(writing: &mut Value, example_id: &str) {
    let mut hidden: Vec<Value> = writing["hidden_examples"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    hidden.push(json!(example_id));
    let keep = hidden.len().saturating_sub(MAX_HIDDEN);
    writing["hidden_examples"] = Value::Array(hidden.split_off(keep));
}

/// Recount each style's habits from the newest explicit corrections to
/// cleaned-up text; a correction counts for the style it teaches.
fn recount(conn: &Connection, styles: &Snapshot, writing: &mut Value) -> rusqlite::Result<()> {
    let sources = EXPLICIT_SOURCES
        .map(|source| format!("'{source}'"))
        .join(", ");
    // A correction whose capture history retention deleted keeps its own
    // copy of the style it teaches.
    let mut statement = conn.prepare(&format!(
        "SELECT f.capture_id, f.expected_text, f.snapshot, \
         COALESCE(c.teaches_style_id, f.teaches_style_id) \
         FROM capture_feedback f LEFT OUTER JOIN captures c ON c.id = f.capture_id \
         WHERE f.target = 'refined' AND f.source IN ({sources}) \
         ORDER BY f.created_at DESC, f.id DESC LIMIT {COUNTED_CORRECTIONS}"
    ))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = HashSet::new();
    let mut counts: HashMap<String, Counts> = HashMap::new();
    for (capture_id, expected, snapshot, teaches) in rows {
        // The latest correction of a capture replaces earlier ones.
        if !seen.insert(capture_id) {
            continue;
        }
        let Ok(Value::Object(captured)) = serde_json::from_str::<Value>(&snapshot) else {
            continue;
        };
        let Some(original) = captured
            .get("transcript_refined")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        if py::len(original).max(py::len(&expected)) > MAX_COUNTED_CHARS {
            continue;
        }
        let bundle = captured.get("app_bundle_id").and_then(Value::as_str);
        let style = styles.correction_style(bundle, teaches.as_deref());
        counts
            .entry(style.to_string())
            .or_default()
            .add(&observe(original, &expected, false));
    }
    let profiles = styles_mut(writing);
    for (style_id, profile) in profiles.iter_mut() {
        let mut merged = over(empty_style(), profile);
        merged["feedback_counts"] = counts.remove(style_id).unwrap_or_default().to_json();
        *profile = merged;
    }
    for (style_id, counted) in counts {
        let mut profile = empty_style();
        profile["feedback_counts"] = counted.to_json();
        profiles.insert(style_id, profile);
    }
    writing["counting"] = json!(COUNTING);
    Ok(())
}

// -- correction-notes.json -----------------------------------------------------

fn empty_notes() -> Value {
    json!({
        "notes": [],
        "considered_ids": [],
        "history": [],
        "last_run": null,
        "outcome": "waiting",
        "metrics": null,
    })
}

/// The whole notes file in version 2. Version 1 held one list, which
/// becomes the migrated style's; a file Python wouldn't read starts empty.
pub fn read_notes(data_dir: &Path) -> Value {
    let mut state = json!({"version": 2, "styles": {}});
    let path = data_dir.join(CORRECTION_NOTES);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return state,
        Err(error) => {
            eprintln!("[learned] couldn't read {}: {error}", path.display());
            return state;
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(mut loaded)) if loaded.get("version") == Some(&json!(1)) => {
            loaded.remove("version");
            state["styles"][MIGRATED_STYLE] = Value::Object(loaded);
        }
        Ok(loaded @ Value::Object(_))
            if loaded["version"] == json!(2) && loaded["styles"].is_object() =>
        {
            state = loaded;
        }
        _ => eprintln!(
            "[learned] {} isn't usable; starting without correction notes",
            path.display()
        ),
    }
    state
}

/// `style_id`'s notes state over the empty one.
fn notes_json(notes: &Value, style_id: &str) -> Value {
    over(empty_notes(), &notes["styles"][style_id])
}

pub fn note_id(note: &str) -> String {
    let digest = Sha256::digest(note.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex[..12].to_string()
}

fn texts(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(String::from))
        .collect()
}

/// `style_id`'s notes, as `GET /writing-style/notes` answers them. `all` is
/// every example of the style, newest first: the ones out of the prompt that
/// no summary considered yet are pending.
pub fn notes_status(notes: &Value, style_id: &str, all: &[PersonalExample]) -> Value {
    let state = notes_json(notes, style_id);
    let shown: HashSet<String> = cleanup_inputs::in_prompt(all)
        .into_iter()
        .map(|example| example.id)
        .collect();
    let considered: HashSet<String> = texts(&state["considered_ids"]).into_iter().collect();
    let pending = all
        .iter()
        .filter(|example| !shown.contains(&example.id) && !considered.contains(&example.id))
        .count();
    let listed: Vec<Value> = texts(&state["notes"])
        .into_iter()
        .map(|note| json!({ "id": note_id(&note), "text": note }))
        .collect();
    json!({
        "notes": listed,
        "pending": pending,
        "last_run": state["last_run"],
        "outcome": state["outcome"],
    })
}

/// Drop one of `style_id`'s notes, keeping the list before in its history;
/// false when it has no such note.
fn remove_note(notes: &mut Value, style_id: &str, id: &str) -> bool {
    let mut state = notes_json(notes, style_id);
    let before = state["notes"].clone();
    let current = texts(&before);
    let kept: Vec<String> = current
        .iter()
        .filter(|note| note_id(note) != id)
        .cloned()
        .collect();
    if kept.len() == current.len() {
        return false;
    }
    let mut history = state["history"].as_array().cloned().unwrap_or_default();
    history.push(json!({ "notes": before }));
    let keep = history.len().saturating_sub(MAX_NOTES_HISTORY);
    state["history"] = Value::Array(history.split_off(keep));
    state["notes"] = json!(kept);
    if !notes["styles"].is_object() {
        notes["styles"] = json!({});
    }
    notes["styles"][style_id] = state;
    true
}

// -- changes -------------------------------------------------------------------

/// The learned files as one change reads and writes them.
pub struct Files {
    dir: PathBuf,
    pub writing: Value,
    pub notes: Value,
    writing_changed: bool,
    notes_changed: bool,
}

impl Files {
    pub fn read(dir: &Path) -> Files {
        Files {
            dir: dir.to_path_buf(),
            writing: cleanup_inputs::read_writing_style(dir),
            notes: read_notes(dir),
            writing_changed: false,
            notes_changed: false,
        }
    }

    pub fn reset(&mut self, style_id: &str) {
        reset(&mut self.writing, style_id);
        self.writing_changed = true;
    }

    /// Drop everything `style_id` learned, for a deleted style or a reset.
    pub fn forget_notes(&mut self, style_id: &str) {
        if let Some(styles) = self.notes["styles"].as_object_mut() {
            self.notes_changed |= styles.remove(style_id).is_some();
        }
    }

    pub fn forget_style(&mut self, style_id: &str) {
        if let Some(styles) = self.writing["styles"].as_object_mut() {
            self.writing_changed |= styles.remove(style_id).is_some();
        }
        self.forget_notes(style_id);
    }

    pub fn hide(&mut self, example_id: &str) {
        hide(&mut self.writing, example_id);
        self.writing_changed = true;
    }

    pub fn remove_note(&mut self, style_id: &str, id: &str) -> bool {
        let removed = remove_note(&mut self.notes, style_id, id);
        self.notes_changed |= removed;
        removed
    }

    pub fn recount(&mut self, conn: &Connection, styles: &Snapshot) -> rusqlite::Result<()> {
        recount(conn, styles, &mut self.writing)?;
        self.writing_changed = true;
        Ok(())
    }

    /// Whether the habits were counted by an older `observe`.
    pub fn stale(&self) -> bool {
        self.writing["counting"].as_i64() != Some(COUNTING)
    }

    /// Write what changed; true when anything did.
    pub fn save(self) -> std::io::Result<bool> {
        if self.writing_changed {
            save(&self.dir.join(WRITING_STYLE), &self.writing)?;
        }
        if self.notes_changed {
            save(&self.dir.join(CORRECTION_NOTES), &self.notes)?;
        }
        Ok(self.writing_changed || self.notes_changed)
    }
}

/// Read the learned files, change them with `edit`, and write what changed,
/// under [`FILES`]. When anything was written the apps hear `learned` and
/// the worker rereads the files; `forget_teaching` also ends the worker's
/// teach sessions in that style.
pub async fn change<T, F>(
    state: &AppState,
    forget_teaching: Option<&str>,
    edit: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Connection, &mut Files) -> rusqlite::Result<T> + Send + 'static,
{
    let _held = FILES.lock().await;
    let dir = data_dir(state);
    let (answer, written) = state
        .db
        .call(move |conn| {
            let mut files = Files::read(&dir);
            let answer = edit(conn, &mut files)?;
            let written = files
                .save()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            Ok((answer, written))
        })
        .await
        .map_err(|error| error.to_string())?;
    if written {
        state.events.publish(Kind::Learned);
    }
    if written || forget_teaching.is_some() {
        worker::learned_changed(&state.proxy, forget_teaching).await;
    }
    Ok(answer)
}

/// Recount every style's habits from its corrections: after a correction
/// changes or an app moves to another style.
pub async fn recount_habits(state: &AppState) {
    let teaching = worker::styles_state(&state.proxy).await.teaching_style;
    let recounted = change(state, None, move |conn, files| {
        files.recount(conn, &styles::load(conn, teaching)?)
    })
    .await;
    if let Err(error) = recounted {
        eprintln!("[learned] couldn't recount the writing style habits: {error}");
    }
}

/// A style was deleted: forget what it learned, and count its corrections
/// for the styles they teach now.
pub async fn forget_style(state: &AppState, style_id: &str) {
    let teaching = worker::styles_state(&state.proxy).await.teaching_style;
    let style_id = style_id.to_string();
    let forgotten = change(state, None, move |conn, files| {
        files.forget_style(&style_id);
        files.recount(conn, &styles::load(conn, teaching)?)
    })
    .await;
    if let Err(error) = forgotten {
        eprintln!("[learned] couldn't forget a deleted style: {error}");
    }
}

/// Whether `example_id` is one of the examples some style uses.
pub fn known_example(
    conn: &Connection,
    writing: &Value,
    styles: &Snapshot,
    example_id: &str,
) -> rusqlite::Result<bool> {
    for style in &styles.styles {
        let examples = cleanup_inputs::all_examples(conn, writing, styles, &style.id)?;
        if examples.iter().any(|example| example.id == example_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reports changed: an explicit correction to cleaned-up text teaches the
/// writing style, so its habits are counted again.
pub async fn reports_changed(state: &AppState, changed: &[(String, String)]) {
    let teaches = changed
        .iter()
        .any(|(target, source)| target == "refined" && EXPLICIT_SOURCES.contains(&source.as_str()));
    if teaches {
        recount_habits(state).await;
    }
}

/// At start: recount habits an older `observe` counted.
pub async fn recount_if_stale(state: AppState) {
    let dir = data_dir(&state);
    let stale = tokio::task::spawn_blocking(move || Files::read(&dir).stale())
        .await
        .unwrap_or(false);
    if !stale {
        return;
    }
    // The worker just started: no style is being taught.
    let recounted = change(&state, None, |conn, files| {
        files.recount(conn, &styles::load(conn, None)?)
    })
    .await;
    if let Err(error) = recounted {
        eprintln!("[learned] couldn't recount the writing style habits: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_ids_are_python_s() {
        // hashlib.sha256("Write Kass as one word.".encode()).hexdigest()[:12]
        assert_eq!(note_id("Write Kass as one word."), "dd4982fda515");
    }

    #[test]
    fn removing_a_note_keeps_the_list_before() {
        let mut notes = json!({"version": 2, "styles": {"s": {"notes": ["a", "b"]}}});
        assert!(!remove_note(&mut notes, "s", "missing"));
        assert!(remove_note(&mut notes, "s", &note_id("a")));
        assert_eq!(notes["styles"]["s"]["notes"], json!(["b"]));
        assert_eq!(
            notes["styles"]["s"]["history"],
            json!([{"notes": ["a", "b"]}])
        );
        assert_eq!(notes["styles"]["s"]["outcome"], json!("waiting"));
    }

    #[test]
    fn hidden_examples_keep_the_newest_thousand() {
        let mut writing = json!({"hidden_examples": (0..MAX_HIDDEN).collect::<Vec<_>>()});
        hide(&mut writing, "new");
        let hidden = writing["hidden_examples"].as_array().unwrap();
        assert_eq!(hidden.len(), MAX_HIDDEN);
        assert_eq!(hidden[0], json!(1));
        assert_eq!(hidden[MAX_HIDDEN - 1], json!("new"));
    }

    #[test]
    fn status_reads_an_empty_style() {
        let writing = json!({"version": 2, "styles": {}});
        assert_eq!(
            status(&writing, "s"),
            json!({"ready": false, "runs": 0, "last_run_at": null, "example_count": 0, "habits": []})
        );
    }

    #[test]
    fn version_1_notes_move_into_the_migrated_style() {
        let dir = std::env::temp_dir().join(format!("kass-notes-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(CORRECTION_NOTES),
            r#"{"version": 1, "notes": ["a"], "outcome": "updated"}"#,
        )
        .unwrap();
        let notes = read_notes(&dir);
        assert_eq!(notes["styles"][MIGRATED_STYLE]["notes"], json!(["a"]));
        assert_eq!(notes["version"], json!(2));
        fs::remove_dir_all(&dir).unwrap();
    }
}
