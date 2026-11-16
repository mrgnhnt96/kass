//! Correction learning: the rules Kass learns from the user's reports, kept
//! in `correction-learning.json` next to `kass.db`.
//!
//! This server owns the file. It reads the reports from `kass.db` (the
//! worker saves voice fixes and redictations there too), learns from them
//! with kass-text, saves the file, and tells the worker to reload it: the
//! worker applies the rules to dictations and retests its cleanup adapter
//! when they change. Python: `backend/services/correction_learning.py`.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use kass_text::correction_rules::{self, Example, Rule, Rules, MAX_RULES};
use kass_text::spoken_punctuation;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::AppState;

/// How often the server looks for new, changed or withdrawn reports.
const CHECK_EVERY: Duration = Duration::from_secs(15);

/// The newest reports learned from.
const REPORTS: usize = 500;

/// Longer takes aren't learned from.
const MAX_EXAMPLE: usize = 1000;

/// Earlier rule sets kept for rollback.
const HISTORY: usize = 10;

/// One job at a time, across servers too: sharing restarts the server, and
/// the old one may still be finishing a job.
static JOB: Mutex<()> = Mutex::new(());

pub fn path(state: &AppState) -> PathBuf {
    state
        .db
        .path()
        .parent()
        .map(|dir| dir.join("correction-learning.json"))
        .unwrap_or_else(|| PathBuf::from("correction-learning.json"))
}

fn empty() -> Value {
    json!({
        "version": 1,
        "revision": 0,
        "rules": [],
        "history": [],
        "blocked": [],
        // Rule id -> the reports that contradicted it. Rollback blocks have none.
        "blocked_by": {},
        "last_run": null,
        "fingerprint": null,
        "evaluated_report_ids": [],
        "metrics": null,
        "outcome": "waiting",
        "punctuation": {"aliases": {}, "kept": []},
    })
}

/// The saved state, over the defaults; the defaults alone when the file is
/// missing or can't be used.
pub fn load(path: &Path) -> Value {
    let mut state = empty();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return state,
        Err(error) => {
            eprintln!(
                "[learning] couldn't read {}: {error}; using no learned rules",
                path.display()
            );
            return state;
        }
    };
    let loaded: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let usable = loaded["version"] == json!(1)
        && loaded["rules"].as_array().is_some_and(|rules| {
            rules.len() <= MAX_RULES && rules.iter().all(|r| Rule::from_json(r).is_some())
        });
    match loaded {
        Value::Object(fields) if usable => {
            for (key, value) in fields {
                state[key] = value;
            }
        }
        _ => eprintln!(
            "[learning] {} isn't usable; using no learned rules",
            path.display()
        ),
    }
    state
}

/// Write the whole file at once: a dictation never reads half of it.
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

fn rules(value: &Value) -> Vec<Rule> {
    value
        .as_array()
        .map(|rules| rules.iter().filter_map(Rule::from_json).collect())
        .unwrap_or_default()
}

fn rules_json(rules: &[Rule]) -> Value {
    Value::Array(rules.iter().map(Rule::to_json).collect())
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn status(state: &Value) -> Value {
    json!({
        "evaluated_report_ids": state["evaluated_report_ids"],
        "revision": state["revision"],
        "active_rules": state["rules"].as_array().map_or(0, Vec::len),
        "last_run": state["last_run"],
        "outcome": state["outcome"],
        "metrics": state["metrics"],
        "can_rollback": state["history"].as_array().is_some_and(|h| !h.is_empty()),
    })
}

/// What the job learns from, read from `kass.db`.
pub struct Reports {
    /// The latest report on each take and target, oldest first.
    examples: Vec<Example>,
    /// The reports behind a contradiction block that still exist.
    existing: HashSet<String>,
}

pub fn read(conn: &Connection, state: &Value) -> rusqlite::Result<Reports> {
    let mut statement = conn.prepare(
        "SELECT id, capture_id, target, expected_text, snapshot, source FROM capture_feedback
         ORDER BY created_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = statement.query_map([REPORTS as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut seen = HashSet::new();
    let mut latest = Vec::new();
    for row in rows {
        let row = row?;
        if seen.insert((row.1.clone(), row.2.clone())) {
            latest.push(row);
        }
    }
    let mut examples = Vec::new();
    for (id, capture_id, target, expected, snapshot, source) in latest.into_iter().rev() {
        let Ok(snapshot) = serde_json::from_str::<Value>(&snapshot) else {
            eprintln!("[learning] skipping report {id}: its snapshot isn't JSON");
            continue;
        };
        let field = if target == "raw" {
            "transcript_raw"
        } else {
            "transcript_refined"
        };
        let Some(original) = snapshot.get(field).and_then(Value::as_str) else {
            continue;
        };
        if original.is_empty()
            || original.chars().count().max(expected.chars().count()) > MAX_EXAMPLE
        {
            continue;
        }
        examples.push(Example {
            id,
            capture_id,
            original: original.to_string(),
            expected,
            language: snapshot
                .get("language")
                .and_then(Value::as_str)
                .map(String::from),
            source,
        });
    }

    let causes: Vec<String> = state["blocked_by"]
        .as_object()
        .map(|blocks| blocks.values().flat_map(strings).collect())
        .unwrap_or_default();
    let mut existing = HashSet::new();
    let mut statement = conn.prepare("SELECT 1 FROM capture_feedback WHERE id = ?1")?;
    for cause in causes {
        if statement.exists([&cause])? {
            existing.insert(cause);
        }
    }
    Ok(Reports { examples, existing })
}

fn punctuation(examples: &[Example]) -> Value {
    let examples: Vec<spoken_punctuation::Example> = examples
        .iter()
        .map(|e| spoken_punctuation::Example {
            original: e.original.clone(),
            expected: e.expected.clone(),
            capture_id: e.capture_id.clone(),
            source: e.source.clone(),
        })
        .collect();
    let learned = spoken_punctuation::learn(&examples);
    json!({"aliases": learned.aliases, "kept": learned.kept})
}

/// Rules whose contradicting reports are all gone: their block lifts.
fn lifted(state: &Value, reports: &Reports) -> Vec<String> {
    state["blocked_by"]
        .as_object()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|(_, causes)| !strings(causes).iter().any(|c| reports.existing.contains(c)))
                .map(|(rule, _)| rule.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the reports changed since the job last learned from them.
pub fn changed(state: &Value, reports: &Reports) -> bool {
    let ids: Vec<&str> = reports.examples.iter().map(|e| e.id.as_str()).collect();
    state["fingerprint"].as_str() != Some(&correction_rules::fingerprint(&reports.examples))
        || state["evaluated_report_ids"] != json!(ids)
        || !lifted(state, reports).is_empty()
        || state["punctuation"] != punctuation(&reports.examples)
}

/// Learn from `reports`: drop rules a report contradicts, lift blocks whose
/// reports are gone, add rules the reports now support. The new state, or
/// None when nothing changed since the last run.
pub fn learn(state: &Value, reports: &Reports) -> Option<Value> {
    let examples = &reports.examples;
    let fingerprint = correction_rules::fingerprint(examples);
    let ids: Vec<&str> = examples.iter().map(|e| e.id.as_str()).collect();
    let lifted = lifted(state, reports);
    let punctuation = punctuation(examples);
    if state["fingerprint"].as_str() == Some(&fingerprint)
        && state["evaluated_report_ids"] == json!(ids)
        && lifted.is_empty()
        && state["punctuation"] == punctuation
    {
        return None;
    }
    let mut next = state.clone();
    next["punctuation"] = punctuation;
    let mut blocked: Vec<String> = strings(&state["blocked"])
        .into_iter()
        .filter(|r| !lifted.contains(r))
        .collect();
    let mut blocked_by = state["blocked_by"].as_object().cloned().unwrap_or_default();
    blocked_by.retain(|rule, _| !lifted.contains(rule));

    // A new report contradicting a learned rule disables it before proposing
    // replacements. The block lasts until that report is withdrawn or deleted.
    let active = rules(&state["rules"]);
    let mut retained = Vec::new();
    for rule in &active {
        let compiled = Rules::new(vec![rule.clone()]);
        let contradicting: Vec<&str> = examples
            .iter()
            .filter(|e| {
                let language = e.language.as_deref();
                compiled.apply(&e.expected, language) != e.expected
                    || correction_rules::loss(&compiled.apply(&e.original, language), &e.expected)
                        > correction_rules::loss(&e.original, &e.expected)
            })
            .map(|e| e.id.as_str())
            .collect();
        if contradicting.is_empty() {
            retained.push(rule.clone());
        } else {
            blocked.push(rule.id.clone());
            blocked_by.insert(rule.id.clone(), json!(contradicting));
        }
    }
    let (learned, metrics) = correction_rules::evaluate(examples, &retained, &blocked);
    let updated = learned != active;
    if updated {
        let mut history = state["history"].as_array().cloned().unwrap_or_default();
        history.push(json!({"revision": state["revision"], "rules": state["rules"]}));
        let skip = history.len().saturating_sub(HISTORY);
        next["history"] = Value::Array(history.split_off(skip));
        next["revision"] = json!(state["revision"].as_i64().unwrap_or(0) + 1);
        next["rules"] = rules_json(&learned);
    }
    next["blocked"] = json!(blocked);
    next["blocked_by"] = Value::Object(blocked_by);
    next["fingerprint"] = json!(fingerprint);
    next["evaluated_report_ids"] = json!(ids);
    next["metrics"] = metrics.to_json();
    next["last_run"] = json!(now());
    next["outcome"] = json!(if updated { "updated" } else { "no_change" });
    Some(next)
}

/// Python's `datetime.now(UTC).isoformat()`.
fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Go back to the rules before the last change; the rules it took out stay
/// blocked. None when there is nothing to go back to.
pub fn rolled_back(state: &Value) -> Option<Value> {
    let mut next = state.clone();
    let previous = next["history"].as_array_mut()?.pop()?;
    let restored: HashSet<&str> = previous["rules"]
        .as_array()
        .map(|rules| rules.iter().filter_map(|r| r["id"].as_str()).collect())
        .unwrap_or_default();
    let mut blocked = strings(&state["blocked"]);
    for rule in rules(&state["rules"]) {
        if !restored.contains(rule.id.as_str()) && !blocked.contains(&rule.id) {
            blocked.push(rule.id);
        }
    }
    // A contradicted rule must not come back by rolling back another change.
    let kept: Vec<Rule> = rules(&previous["rules"])
        .into_iter()
        .filter(|r| !blocked.contains(&r.id))
        .collect();
    next["rules"] = rules_json(&kept);
    next["blocked"] = json!(blocked);
    next["revision"] = json!(state["revision"].as_i64().unwrap_or(0) + 1);
    next["outcome"] = json!("rolled_back");
    Some(next)
}

#[derive(Debug)]
pub enum Failed {
    Database(String),
    Save(String),
    Task(String),
    NothingToRollBack,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Database(error) => write!(f, "couldn't read the reports: {error}"),
            Failed::Save(error) => write!(f, "couldn't save what was learned: {error}"),
            Failed::Task(error) => write!(f, "learning failed: {error}"),
            Failed::NothingToRollBack => f.write_str("No previous correction version is available"),
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Failed> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Failed::Task(e.to_string()))
}

/// The saved state and the reports in `kass.db`.
async fn reports(state: &AppState) -> Result<(Value, Reports), Failed> {
    let path = path(state);
    let saved = blocking(move || load(&path)).await?;
    let snapshot = saved.clone();
    let reports = state
        .db
        .call(move |conn| read(conn, &snapshot))
        .await
        .map_err(|e| Failed::Database(e.to_string()))?;
    Ok((saved, reports))
}

/// Whether reports changed since the job last learned from them.
pub async fn pending(state: &AppState) -> Result<bool, Failed> {
    let (saved, reports) = reports(state).await?;
    blocking(move || changed(&saved, &reports)).await
}

/// Learn from the reports if they changed: the status after, and whether
/// the rules file changed.
pub async fn run(state: &AppState) -> Result<(Value, bool), Failed> {
    let (_, reports) = reports(state).await?;
    let path = path(state);
    // Learning reads no database: the connection is free while it runs.
    blocking(move || {
        let _job = JOB.lock().unwrap_or_else(|e| e.into_inner());
        // The file may have changed while the reports were read (a rollback,
        // a sync from the shared Kass).
        let saved = load(&path);
        match learn(&saved, &reports) {
            None => Ok((status(&saved), false)),
            Some(next) => {
                save(&path, &next).map_err(|e| Failed::Save(e.to_string()))?;
                Ok((status(&next), true))
            }
        }
    })
    .await?
}

pub async fn rollback(state: &AppState) -> Result<Value, Failed> {
    let path = path(state);
    blocking(move || {
        let _job = JOB.lock().unwrap_or_else(|e| e.into_inner());
        let next = rolled_back(&load(&path)).ok_or(Failed::NothingToRollBack)?;
        save(&path, &next).map_err(|e| Failed::Save(e.to_string()))?;
        Ok(status(&next))
    })
    .await?
}

pub async fn current(state: &AppState) -> Result<Value, Failed> {
    let path = path(state);
    blocking(move || status(&load(&path))).await
}

/// Learn from reports as they're saved, changed and withdrawn, whichever
/// process saved them, while nothing is dictating. Not while standing in
/// for a shared Kass: that one learns.
pub async fn watch(state: AppState) {
    if state.sync.standing_in {
        return;
    }
    let mut ticks = tokio::time::interval(CHECK_EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if !state.db.path().exists() {
            continue;
        }
        let learned = match pending(&state).await {
            Ok(false) => continue,
            // Learning takes the CPU a dictation needs.
            Ok(true) if !crate::worker::models_idle(&state.proxy).await => continue,
            Ok(true) => run(&state).await,
            Err(error) => Err(error),
        };
        match learned {
            Ok((_, true)) => crate::worker::learning_changed(&state.proxy).await,
            Ok(_) => {}
            Err(error) => eprintln!("[learning] {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(id: &str, capture: &str, original: &str, expected: &str) -> Example {
        Example {
            id: id.into(),
            capture_id: capture.into(),
            original: original.into(),
            expected: expected.into(),
            language: None,
            source: "manual".into(),
        }
    }

    fn reports(examples: Vec<Example>) -> Reports {
        Reports {
            existing: examples.iter().map(|e| e.id.clone()).collect(),
            examples,
        }
    }

    fn voicebox(n: usize) -> Vec<Example> {
        ["home", "work", "school", "lunch"][..n]
            .iter()
            .enumerate()
            .map(|(i, place)| {
                example(
                    &format!("r{i}"),
                    &format!("c{i}"),
                    &format!("Please use voice box today at {place}."),
                    &format!("Please use Voicebox today at {place}."),
                )
            })
            .collect()
    }

    #[test]
    fn learns_saves_and_rolls_back() {
        let state = learn(&empty(), &reports(voicebox(3))).unwrap();
        assert_eq!(status(&state)["active_rules"], 1);
        assert_eq!(state["outcome"], "updated");
        assert_eq!(state["revision"], 1);
        assert_eq!(state["evaluated_report_ids"], json!(["r0", "r1", "r2"]));
        // The same reports: nothing to do.
        assert!(learn(&state, &reports(voicebox(3))).is_none());

        let back = rolled_back(&state).unwrap();
        assert_eq!(back["rules"], json!([]));
        assert_eq!(back["outcome"], "rolled_back");
        assert_eq!(back["blocked"], json!([state["rules"][0]["id"]]));
        assert!(rolled_back(&back).is_none());
        // The block outlives new reports.
        let again = learn(&back, &reports(voicebox(4))).unwrap();
        assert_eq!(again["rules"], json!([]));
    }

    #[test]
    fn a_contradicting_report_blocks_a_rule_until_withdrawn() {
        let state = learn(&empty(), &reports(voicebox(3))).unwrap();
        let rule = state["rules"][0]["id"].as_str().unwrap().to_string();
        let mut examples = voicebox(3);
        examples.push(example(
            "keep",
            "c9",
            "Please use voice box today at noon.",
            "Please use voice box today at noon.",
        ));
        let blocked = learn(&state, &reports(examples)).unwrap();
        assert_eq!(blocked["rules"], json!([]));
        assert_eq!(blocked["blocked_by"][&rule], json!(["keep"]));

        // Withdrawn: the block lifts and the rule is learned again.
        let mut gone = reports(voicebox(3));
        gone.existing.remove("keep");
        let lifted = learn(&blocked, &gone).unwrap();
        assert_eq!(lifted["blocked"], json!([]));
        assert_eq!(lifted["rules"][0]["id"], rule);
    }

    #[test]
    fn keeps_ten_earlier_versions() {
        let mut state = empty();
        state["history"] = json!((0..10)
            .map(|r| json!({"revision": r, "rules": []}))
            .collect::<Vec<_>>());
        state["revision"] = json!(10);
        let next = learn(&state, &reports(voicebox(3))).unwrap();
        let history = next["history"].as_array().unwrap();
        assert_eq!(history.len(), 10);
        assert_eq!(history[0]["revision"], 1);
        assert_eq!(history[9]["revision"], 10);
        assert_eq!(next["revision"], 11);
    }

    #[test]
    fn an_unusable_file_means_no_rules() {
        let dir = std::env::temp_dir().join(format!("kass-learning-{}", uuid::Uuid::new_v4()));
        let path = dir.join("correction-learning.json");
        assert_eq!(load(&path), empty());
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, r#"{"version": 2, "rules": []}"#).unwrap();
        assert_eq!(load(&path), empty());
        fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), empty());

        let state = learn(&empty(), &reports(voicebox(3))).unwrap();
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        assert!(!path.with_extension("tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
