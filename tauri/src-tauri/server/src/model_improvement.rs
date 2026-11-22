//! Model improvement: training the cleanup adapter, the voice adapter and
//! trying other speech models on the user's reports, and keeping the ones
//! that do better. Its state is `model-improvement/state.json` next to
//! `kass.db`.
//!
//! This server owns the state, the schedule, status and rollback. The worker
//! plans a run from the reports and the models it has (`prepare`), trains
//! and judges it in a child process (`train`), and uses the trained models
//! this server saves (`use`). A trained model the worker stops using (it
//! failed its checks or didn't load) comes back in its `reports`. Python:
//! `backend/services/model_improvement/manager.py`.
//!
//! A cleanup adapter is stamped with the cleanup pipeline it was tested with
//! ([`pipeline`]); the worker, told the pipeline at launch, drops one with
//! another. The training process prepares its text through this server
//! (`crate::training_cases`), the way dictation prepares it.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::events::Kind;
use crate::{worker, AppState};

/// How often the schedule looks for a run to start.
const CHECK_EVERY: Duration = Duration::from_secs(60);

/// A run every six hours at most, unless reports change what it'd learn.
const INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// After macOS stops a run to keep the Mac responsive, nothing starts the
/// next one for a day: the Mac was busy, and an hour later it often still is.
const GPU_PREEMPTED_WAIT: Duration = Duration::from_secs(24 * 3600);

/// Earlier versions kept for rollback.
const HISTORY: usize = 10;

/// The recordings the voice bank needs before a voice adapter trains.
const VOICE_MIN_TRAIN: u64 = 100;
const VOICE_MIN_TEST: u64 = 12;

/// Phases a run is in while it runs; one saved like this didn't finish.
const RUNNING: [&str; 4] = ["preparing", "training", "evaluating", "queued"];

/// One change to the file at a time, across servers too: sharing restarts
/// the server, and the old one may still be finishing a run.
static FILE: Mutex<()> = Mutex::new(());

/// What this server knows about runs beyond the file.
#[derive(Default)]
pub struct Runtime(Mutex<Runs>);

#[derive(Default)]
struct Runs {
    /// A run is being prepared, trained or judged.
    running: bool,
    /// The running run's folder, where the child writes its progress.
    folder: Option<PathBuf>,
    /// When a run last ended, or was cancelled; None before the first.
    last_attempt: Option<Instant>,
    /// After a failed run, no run starts on its own before this, not even
    /// for new reports or rules.
    hold_until: Option<Instant>,
    /// A withdrawn report may be in the cleanup adapter's training data.
    retrain: bool,
    /// The learned corrections the schedule last looked at.
    rules: Option<Value>,
}

impl Runtime {
    fn running(&self) -> bool {
        self.runs().running
    }

    /// Claim the next run; false when one is going.
    fn begin(&self) -> bool {
        let mut runs = self.runs();
        let free = !runs.running;
        if free {
            runs.running = true;
            runs.folder = None;
        }
        free
    }

    fn end(&self) {
        let mut runs = self.runs();
        runs.running = false;
        runs.folder = None;
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, Runs> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reports were withdrawn: retrain the cleanup adapter at the next idle
    /// moment, as one of them may be in its training data.
    pub fn retrain(&self) {
        self.runs().retrain = true;
    }
}

pub fn path(state: &AppState) -> PathBuf {
    folder(state).join("state.json")
}

fn folder(state: &AppState) -> PathBuf {
    folder_for(state.db.path())
}

/// The state's folder for `database`, `kass.db`.
fn folder_for(database: &Path) -> PathBuf {
    database
        .parent()
        .map(|dir| dir.join("model-improvement"))
        .unwrap_or_else(|| PathBuf::from("model-improvement"))
}

fn empty() -> Value {
    json!({
        "version": 1,
        "revision": 0,
        "phase": "waiting",
        "active": {},
        "history": [],
        "groups": [],
        "attempted": [],
        "blocked": [],
        "counts": {},
        "last_run": null,
        "metrics": null,
        "error": null,
        "evaluated_report_ids": [],
        // The last trained voice adapter, promoted or not, that the next run
        // continues from (docs/plans/VOICE_TRAINING.md).
        "voice_resume": null,
    })
}

/// The cleanup pipeline cleanup adapters are tested with: what the worker
/// gets as `KASS_CLEANUP_PIPELINE`.
pub fn pipeline() -> &'static str {
    kass_text::pipeline::id()
}

/// Restamp a cleanup adapter the Python worker stamped with the pipeline
/// this server ports. Whether it was.
fn restamp_adapter(adapter: Option<&mut Value>) -> bool {
    let Some(fields) = adapter.and_then(Value::as_object_mut) else {
        return false;
    };
    let restamped = fields
        .get("pipeline")
        .and_then(Value::as_str)
        .and_then(kass_text::pipeline::restamped);
    restamped.is_some_and(|id| fields.insert("pipeline".into(), json!(id)).is_some())
}

/// Restamp the cleanup adapters, in use and kept for rollback, that the
/// Python worker stamped with the pipeline this server ports. Whether any was.
fn restamp(saved: &mut Value) -> bool {
    let mut changed = restamp_adapter(saved.pointer_mut("/active/llm"));
    if let Some(history) = saved["history"].as_array_mut() {
        for entry in history {
            changed |= restamp_adapter(entry.pointer_mut("/active/llm"));
        }
    }
    changed
}

/// Keep the user's cleanup adapters across the move of the cleanup pipeline
/// from the Python worker to this server: restamp them in the state next to
/// `database`. Runs before the worker starts, which reads the state itself.
/// A state it can't read or save is left as it is.
pub fn restamp_legacy(database: &Path) {
    let path = folder_for(database).join("state.json");
    let _file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => return eprintln!("[model] couldn't read {}: {error}", path.display()),
    };
    let Ok(mut saved) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    if saved["version"] != json!(1) || !restamp(&mut saved) {
        return;
    }
    match save(&path, &saved) {
        Ok(()) => eprintln!("[model] restamped the cleanup adapters for the Rust cleanup pipeline"),
        Err(error) => eprintln!("[model] couldn't save {}: {error}", path.display()),
    }
}

/// The saved state, over the defaults; the defaults alone when the file is
/// missing or can't be used.
pub fn load(path: &Path) -> Value {
    let mut state = empty();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return state,
        Err(error) => {
            eprintln!("[model] couldn't read {}: {error}", path.display());
            return state;
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(fields)) if fields.get("version") == Some(&json!(1)) => {
            for (key, value) in fields {
                state[key] = value;
            }
        }
        _ => eprintln!("[model] {} isn't usable", path.display()),
    }
    state
}

/// Write the whole file at once: the worker never reads half of it.
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

fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
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

fn push(list: &mut Value, item: Value) {
    match list.as_array_mut() {
        Some(items) => items.push(item),
        None => *list = json!([item]),
    }
}

fn active(state: &Value) -> Map<String, Value> {
    state["active"].as_object().cloned().unwrap_or_default()
}

/// Change the saved state and save it, then tell the app. `change` answers
/// `Err` to leave the file as it was.
async fn update<T: Send + 'static>(
    state: &AppState,
    change: impl FnOnce(&mut Value) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let path = path(state);
    let changed = tokio::task::spawn_blocking(move || {
        let _file = FILE.lock().unwrap_or_else(|e| e.into_inner());
        let mut saved = load(&path);
        let answer = change(&mut saved)?;
        save(&path, &saved).map_err(|e| format!("couldn't save the model state: {e}"))?;
        Ok(answer)
    })
    .await
    .map_err(|e| e.to_string())?;
    if changed.is_ok() {
        state.events.publish(Kind::Learned);
    }
    changed
}

async fn read(state: &AppState) -> Value {
    let path = path(state);
    tokio::task::spawn_blocking(move || load(&path))
        .await
        .unwrap_or_else(|_| empty())
}

/// The trained models in use, by kind, each with the folder it's in, for a
/// stand-in to copy.
pub async fn in_use(state: &AppState) -> Map<String, Value> {
    active(&read(state).await)
}

/// Whether there are earlier trained models to go back to.
pub async fn can_rollback(state: &AppState) -> bool {
    read(state).await["history"]
        .as_array()
        .is_some_and(|history| !history.is_empty())
}

/// Whether a personal cleanup adapter is in use.
pub async fn adapter_in_use(state: &AppState) -> bool {
    in_use(state).await.contains_key("llm")
}

/// A stand-in copied the shared Kass's trained models and the worker uses
/// `kept`: they're this Mac's now.
pub async fn use_mirrored(state: &AppState, kept: Map<String, Value>) -> Result<(), String> {
    if in_use(state).await == kept {
        return Ok(());
    }
    update(state, move |saved| {
        saved["active"] = Value::Object(kept);
        Ok(())
    })
    .await
}

/// Record the trained models the worker stopped using. One that failed while
/// in use (`block`) isn't tried again.
fn apply_reports(saved: &mut Value, reports: &[Value]) {
    for report in reports {
        let Some(kind) = report["kind"].as_str() else {
            continue;
        };
        let block = report["block"].as_bool().unwrap_or(false);
        let in_use = saved["active"][kind]["id"] == report["id"];
        if !in_use {
            continue;
        }
        if let Some(active) = saved["active"].as_object_mut() {
            active.remove(kind);
        }
        if block {
            push(&mut saved["blocked"], report["id"].clone());
            if kind == "voice" {
                saved["voice_resume"] = Value::Null;
            }
            saved["phase"] = json!("reverted");
        }
        saved["error"] = report["error"].clone();
    }
}

async fn take_reports(state: &AppState) -> Result<(), String> {
    let reports = worker::model_reports(&state.proxy).await?;
    if !reports.is_empty() {
        update(state, move |saved| {
            apply_reports(saved, &reports);
            Ok(())
        })
        .await?;
    }
    Ok(())
}

/// The status the app shows: the saved state, what the worker knows about
/// the voice bank, the background sounds' download, and whether a run is
/// going.
pub async fn status(state: &AppState) -> Result<Value, String> {
    take_reports(state).await?;
    let mut voice = worker::model_voice(&state.proxy).await?;
    voice["sounds"] = crate::voice_sounds::status(state).await;
    let saved = read(state).await;
    let (running, folder) = {
        let runs = state.model_improvement.runs();
        (runs.running, runs.folder.clone())
    };
    let folder = folder.filter(|_| running);
    let mut phase = saved["phase"].clone();
    if !running && RUNNING.iter().any(|p| phase == *p) {
        phase = json!("interrupted");
    }
    if let (true, Some(folder)) = (running, folder) {
        if phase == "training" || phase == "evaluating" {
            let progress = fs::read_to_string(folder.join("progress.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok());
            if progress.is_some_and(|p| {
                p["phase"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("evaluating"))
            }) {
                phase = json!("evaluating");
            }
        }
    }
    Ok(describe(&saved, phase, running, voice))
}

fn describe(saved: &Value, phase: Value, running: bool, voice: Value) -> Value {
    let metrics = &saved["metrics"];
    let item = &saved["active"]["voice"];
    let history = saved["history"].as_array().cloned().unwrap_or_default();
    json!({
        "phase": phase,
        "revision": saved["revision"],
        "counts": saved["counts"],
        "last_run": saved["last_run"],
        "metrics": metrics,
        "error": saved["error"],
        "active_adapter": saved["active"]["llm"]["id"],
        "speech_model": saved["active"]["speech"]["model"],
        "can_rollback": !history.is_empty(),
        "voice": {
            "active": item["id"],
            "active_since": item["activated"],
            "bank": voice["bank"],
            "sounds": voice["sounds"],
            "metrics": metrics["voice"],
            "training": metrics["voice_training"],
            "min_train": VOICE_MIN_TRAIN,
            "min_test": VOICE_MIN_TEST,
            // Undo restores the previous model version; offer it here only
            // when that would undo a voice model change.
            "can_undo": history.last().is_some_and(|last| last["active"]["voice"] != *item),
        },
        "running": running,
    })
}

/// Start a run unless one is going or a dictation is; the status after.
pub async fn start(state: &AppState) -> Result<Value, String> {
    if state.model_improvement.running() {
        return status(state).await;
    }
    if worker::models_dictating(&state.proxy).await? {
        update(state, |saved| {
            saved["phase"] = json!("waiting_idle");
            Ok(())
        })
        .await?;
        return status(state).await;
    }
    if !state.model_improvement.begin() {
        return status(state).await;
    }
    let started = update(state, |saved| {
        saved["phase"] = json!("preparing");
        saved["error"] = Value::Null;
        Ok(())
    })
    .await;
    if let Err(error) = started {
        state.model_improvement.end();
        return Err(error);
    }
    let run_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = run(&run_state).await {
            eprintln!("[model] Model improvement failed; production model unchanged: {error}");
            let saved = update(&run_state, move |saved| {
                saved["phase"] = json!("failed");
                saved["error"] = json!(error);
                Ok(())
            })
            .await;
            if let Err(error) = saved {
                eprintln!("[model] {error}");
            }
            run_state.model_improvement.runs().failed(INTERVAL);
        }
        run_state.model_improvement.end();
    });
    status(state).await
}

/// The worker's error, if it answered with one.
fn refused(answer: &Value) -> Result<(), String> {
    match answer["error"].as_str() {
        Some(error) => Err(error.to_string()),
        None => Ok(()),
    }
}

fn attempted(state: &AppState) {
    state.model_improvement.runs().last_attempt = Some(Instant::now());
}

/// Prepare, train and judge one run, saving each step.
async fn run(state: &AppState) -> Result<(), String> {
    take_reports(state).await?;
    let saved = read(state).await;
    let mut skip = strings(&saved["attempted"]);
    skip.extend(strings(&saved["blocked"]));
    let baseline = saved["revision"].clone();
    let voice_resume = saved["voice_resume"].clone();
    let body = json!({
        "groups": saved["groups"],
        "voice_resume": voice_resume,
        "skip": skip,
        "pipeline": pipeline(),
        // The training process prepares its text here (`training_cases`).
        "cleanup_server": format!("http://127.0.0.1:{}", state.address.port()),
        "rehearsal": kass_text::refinement::REFINEMENT_EXAMPLES,
    });
    let plan = worker::model_prepare(&state.proxy, body).await?;
    refused(&plan)?;
    if plan["unsupported"] == true {
        update(state, |saved| {
            saved["phase"] = json!("unsupported");
            Ok(())
        })
        .await?;
        attempted(state);
        return Ok(());
    }
    let fingerprint = plan["fingerprint"].clone();
    let run = plan["run"].as_str().unwrap_or_default().to_string();
    let folder = folder(state).join("runs").join(&run);
    let prepared = plan.clone();
    // What happens next: stop, wait or train.
    let next = update(state, move |saved| {
        for key in ["groups", "counts", "evaluated_report_ids"] {
            saved[key] = prepared[key].clone();
        }
        let tried: HashSet<String> = strings(&saved["attempted"])
            .into_iter()
            .chain(strings(&saved["blocked"]))
            .collect();
        let phase = if prepared["interrupted"] == true {
            "paused"
        } else if prepared["ready"] != true {
            saved["last_run"] = json!(now());
            match prepared["waiting"].as_str() {
                Some("waiting_model") => "waiting_model",
                _ => "waiting_data",
            }
        } else if prepared["fingerprint"]
            .as_str()
            .is_some_and(|f| tried.contains(f))
        {
            "no_new_data"
        } else if prepared["trains"] == true {
            "training"
        } else {
            "evaluating"
        };
        saved["phase"] = json!(phase);
        Ok(phase)
    })
    .await?;
    match next {
        "training" | "evaluating" => {}
        "paused" => return Ok(()),
        _ => {
            attempted(state);
            return Ok(());
        }
    }
    state.model_improvement.runs().folder = Some(folder);
    let body =
        json!({ "run": run, "generation": plan["generation"], "voice_resume": voice_resume });
    let trained = worker::model_train(&state.proxy, body).await?;
    if trained["gpu_preempted"] == true {
        state.model_improvement.runs().failed(GPU_PREEMPTED_WAIT);
    }
    refused(&trained)?;
    if trained["waiting_memory"] == true {
        // Not an attempt: it tries again at the next idle minute.
        return update(state, |saved| {
            saved["phase"] = json!("waiting_memory");
            Ok(())
        })
        .await;
    }
    if trained["interrupted"] == true {
        return update(state, |saved| {
            // A rollback meanwhile keeps its own phase.
            if saved["phase"] == "training" || saved["phase"] == "evaluating" {
                saved["phase"] = json!("paused");
            }
            Ok(())
        })
        .await;
    }
    let candidate = trained["active"].as_object().cloned().unwrap_or_default();
    let changed = update(state, move |saved| {
        promote(saved, &baseline, &trained, fingerprint)
    })
    .await?;
    attempted(state);
    if changed {
        worker::model_use(&state.proxy, &candidate).await?;
    }
    Ok(())
}

/// Keep the judged run: its trained models become the ones in use when they
/// differ, with the ones before kept for rollback. Whether they changed.
fn promote(
    saved: &mut Value,
    baseline: &Value,
    trained: &Value,
    fingerprint: Value,
) -> Result<bool, String> {
    if saved["revision"] != *baseline {
        return Err("The production model changed during evaluation".into());
    }
    let candidate = trained["active"].clone();
    let before = saved["active"].clone();
    let changed = candidate != before;
    if changed {
        let mut history = saved["history"].as_array().cloned().unwrap_or_default();
        history.push(json!({ "active": before, "fingerprint": fingerprint }));
        let start = history.len().saturating_sub(HISTORY);
        saved["history"] = Value::Array(history.split_off(start));
        saved["revision"] = json!(saved["revision"].as_i64().unwrap_or(0) + 1);
        saved["active"] = candidate;
    }
    saved["phase"] = json!(if changed { "activated" } else { "rejected" });
    saved["metrics"] = trained["metrics"].clone();
    saved["last_run"] = json!(now());
    saved["voice_resume"] = trained["voice_resume"].clone();
    push(&mut saved["attempted"], fingerprint);
    Ok(changed)
}

/// Stop a run in progress; the schedule waits its interval before the next.
pub async fn cancel(state: &AppState) -> Result<Value, String> {
    worker::model_stop(&state.proxy).await?;
    attempted(state);
    status(state).await
}

/// Go back to the trained models before the last change; that change's run
/// isn't tried again. `Ok(Err(why))` when there's nothing to go back to.
pub async fn rollback(state: &AppState) -> Result<Result<Value, String>, String> {
    worker::model_stop(&state.proxy).await?;
    attempted(state);
    let saved = read(state).await;
    let Some(entry) = saved["history"].as_array().and_then(|h| h.last()).cloned() else {
        return Ok(Err(NOTHING_TO_ROLL_BACK.into()));
    };
    let restored = active(&entry);
    // Ones that fail their checks now are left out.
    let kept = worker::model_check(&state.proxy, &restored).await?;
    let saving = kept.clone();
    let rolled = update(state, move |saved| Ok(rolled_back(saved, &entry, saving))).await?;
    if !rolled {
        return Ok(Err(NOTHING_TO_ROLL_BACK.into()));
    }
    worker::model_use(&state.proxy, &kept).await?;
    status(state).await.map(Ok)
}

pub const NOTHING_TO_ROLL_BACK: &str = "No previous model version is available";

/// Undo the newest change, `entry`, restoring `kept`; false when it's no
/// longer the newest.
fn rolled_back(saved: &mut Value, entry: &Value, kept: Map<String, Value>) -> bool {
    let Some(history) = saved["history"].as_array_mut() else {
        return false;
    };
    if history.last() != Some(entry) {
        return false;
    }
    history.pop();
    push(&mut saved["blocked"], entry["fingerprint"].clone());
    // Training continues from the restored voice model, not the undone one.
    saved["voice_resume"] = kept
        .get("voice")
        .map(|voice| voice["path"].clone())
        .unwrap_or(Value::Null);
    saved["active"] = Value::Object(kept);
    saved["revision"] = json!(saved["revision"].as_i64().unwrap_or(0) + 1);
    saved["phase"] = json!("rolled_back");
    true
}

/// The learned corrections' rules, to notice they changed.
async fn rules(state: &AppState) -> Value {
    let path = crate::learning::path(state);
    tokio::task::spawn_blocking(move || crate::learning::load(&path)["rules"].clone())
        .await
        .unwrap_or(Value::Null)
}

/// Whether a run is due: a withdrawn report may be in the cleanup adapter,
/// six hours went by, or the rules it was tested with changed. Not while a
/// failed run's wait lasts.
async fn due(state: &AppState) -> bool {
    let adapter = adapter_in_use(state).await;
    let rules = rules(state).await;
    state.model_improvement.runs().due(adapter, rules)
}

impl Runs {
    /// Whether a run is due, given whether a cleanup adapter is in use and
    /// the learned corrections' rules now. Takes the retrain request.
    fn due(&mut self, adapter: bool, rules: Value) -> bool {
        // A failed run waits for all of them, and they're kept for after.
        if self.hold_until.is_some_and(|until| Instant::now() < until) {
            return false;
        }
        // The server learns from a saved or withdrawn report soon, not in six
        // hours. The adapter was tested with the old rules, so new ones retest
        // it; seen only when asked, as Python's take_rules_changed was.
        (std::mem::take(&mut self.retrain) && adapter)
            || self.last_attempt.is_none_or(|at| at.elapsed() >= INTERVAL)
            || (self.rules_changed(rules) && adapter)
    }

    /// A run failed: the next waits `wait` from now, or longer when one was
    /// already set.
    fn failed(&mut self, wait: Duration) {
        let now = Instant::now();
        self.last_attempt = Some(now);
        self.hold_until = self.hold_until.max(Some(now + wait));
    }

    /// Whether the rules changed since the schedule last asked.
    fn rules_changed(&mut self, rules: Value) -> bool {
        let before = self.rules.replace(rules.clone());
        before.is_some_and(|before| before != rules)
    }
}

/// Start a run while nothing uses the models, when one is due. Not while
/// standing in for a shared Kass: that one trains.
pub async fn watch(state: AppState) {
    if state.sync.standing_in {
        return;
    }
    let rules = rules(&state).await;
    state.model_improvement.runs().rules = Some(rules);
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        if let Err(error) = take_reports(&state).await {
            eprintln!("[model] couldn't ask the model worker which models it dropped: {error}");
        }
        if state.model_improvement.running() || !worker::models_idle(&state.proxy).await {
            continue;
        }
        if due(&state).await {
            if let Err(error) = start(&state).await {
                eprintln!("[model] couldn't start model improvement: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trained(active: Value) -> Value {
        json!({ "active": active, "metrics": { "adapter": { "passed": true } }, "voice_resume": "/runs/b/voice" })
    }

    #[test]
    fn a_better_model_is_kept_with_the_one_before_for_rollback() {
        let mut saved = empty();
        saved["active"] = json!({ "llm": { "id": "a" } });
        let changed = promote(
            &mut saved,
            &json!(0),
            &trained(json!({ "llm": { "id": "b" } })),
            json!("f1"),
        )
        .unwrap();
        assert!(changed);
        assert_eq!(saved["active"]["llm"]["id"], "b");
        assert_eq!(saved["revision"], 1);
        assert_eq!(saved["phase"], "activated");
        assert_eq!(
            saved["history"],
            json!([{ "active": { "llm": { "id": "a" } }, "fingerprint": "f1" }])
        );
        assert_eq!(saved["attempted"], json!(["f1"]));
        assert_eq!(saved["voice_resume"], "/runs/b/voice");
    }

    #[test]
    fn the_same_models_are_rejected_without_a_new_revision() {
        let mut saved = empty();
        let changed = promote(&mut saved, &json!(0), &trained(json!({})), json!("f1")).unwrap();
        assert!(!changed);
        assert_eq!(
            (saved["revision"].clone(), saved["phase"].clone()),
            (json!(0), json!("rejected"))
        );
        assert_eq!(saved["history"], json!([]));
        assert_eq!(saved["attempted"], json!(["f1"]));
    }

    #[test]
    fn a_run_judged_against_an_older_revision_is_refused() {
        let mut saved = empty();
        saved["revision"] = json!(2);
        let refused = promote(
            &mut saved,
            &json!(1),
            &trained(json!({ "llm": {} })),
            json!("f"),
        );
        assert_eq!(
            refused,
            Err("The production model changed during evaluation".to_string())
        );
    }

    #[test]
    fn keeps_ten_earlier_versions() {
        let mut saved = empty();
        for i in 0..12 {
            let revision = saved["revision"].clone();
            promote(
                &mut saved,
                &revision,
                &trained(json!({ "llm": { "id": i } })),
                json!(format!("f{i}")),
            )
            .unwrap();
        }
        let history = saved["history"].as_array().unwrap();
        assert_eq!(history.len(), 10);
        assert_eq!(history[0]["fingerprint"], "f2");
    }

    #[test]
    fn rollback_restores_the_models_before_and_blocks_the_run() {
        let mut saved = empty();
        saved["active"] = json!({ "voice": { "id": "new", "path": "/runs/new/voice" } });
        let entry = json!({ "active": { "voice": { "id": "old", "path": "/runs/old/voice" } }, "fingerprint": "f" });
        saved["history"] = json!([entry]);
        let kept = active(&entry);
        assert!(rolled_back(&mut saved, &entry, kept));
        assert_eq!(saved["active"]["voice"]["id"], "old");
        assert_eq!(saved["voice_resume"], "/runs/old/voice");
        assert_eq!(saved["blocked"], json!(["f"]));
        assert_eq!(
            (saved["revision"].clone(), saved["phase"].clone()),
            (json!(1), json!("rolled_back"))
        );
        assert!(!rolled_back(&mut saved, &entry, Map::new()));
    }

    #[test]
    fn a_model_that_failed_in_use_is_dropped_and_blocked() {
        let mut saved = empty();
        saved["active"] = json!({ "llm": { "id": "a" }, "voice": { "id": "v" } });
        saved["voice_resume"] = json!("/runs/v/voice");
        apply_reports(
            &mut saved,
            &[
                json!({ "kind": "voice", "id": "v", "error": "didn't load", "block": true }),
                // An older one than the adapter in use changes nothing.
                json!({ "kind": "llm", "id": "old", "error": "stale", "block": true }),
            ],
        );
        assert_eq!(saved["active"], json!({ "llm": { "id": "a" } }));
        assert_eq!(saved["blocked"], json!(["v"]));
        assert_eq!(saved["voice_resume"], Value::Null);
        assert_eq!(
            (saved["phase"].clone(), saved["error"].clone()),
            (json!("reverted"), json!("didn't load"))
        );
    }

    #[test]
    fn a_saved_model_that_fails_its_checks_is_dropped_without_blocking() {
        let mut saved = empty();
        saved["active"] = json!({ "llm": { "id": "a" } });
        apply_reports(
            &mut saved,
            &[json!({ "kind": "llm", "id": "a", "error": "failed checks", "block": false })],
        );
        assert_eq!(saved["active"], json!({}));
        assert_eq!(saved["blocked"], json!([]));
        assert_eq!(
            (saved["phase"].clone(), saved["error"].clone()),
            (json!("waiting"), json!("failed checks"))
        );
    }

    #[test]
    fn status_offers_voice_undo_only_for_a_voice_change() {
        let mut saved = empty();
        saved["active"] =
            json!({ "voice": { "id": "v", "activated": "today" }, "llm": { "id": "a" } });
        saved["history"] = json!([{ "active": { "llm": { "id": "a" } }, "fingerprint": "f" }]);
        let status = describe(
            &saved,
            json!("activated"),
            false,
            json!({ "bank": { "train": 3 }, "sounds": null }),
        );
        assert_eq!(status["active_adapter"], "a");
        assert_eq!(status["can_rollback"], true);
        assert_eq!(status["voice"]["can_undo"], true);
        assert_eq!(status["voice"]["active_since"], "today");
        assert_eq!(status["voice"]["bank"], json!({ "train": 3 }));
        saved["history"] = json!([{ "active": saved["active"].clone(), "fingerprint": "f" }]);
        let status = describe(&saved, json!("activated"), false, json!({}));
        assert_eq!(status["voice"]["can_undo"], false);
    }

    #[test]
    fn a_run_is_due_after_six_hours_withdrawn_reports_or_new_rules() {
        let rules = |n: usize| json!(vec![json!({ "id": "r" }); n]);
        // (rules changed, retrain, adapter in use, due)
        for (changed, retrain, adapter, due) in [
            (false, false, true, false),
            // The server learned new rules; no adapter to retest.
            (true, false, false, false),
            // New rules invalidate the adapter tested with the old ones.
            (true, false, true, true),
            // A withdrawn report may be in the adapter's training data.
            (false, true, true, true),
            (false, true, false, false),
        ] {
            let mut runs = Runs {
                last_attempt: Some(Instant::now()),
                retrain,
                rules: Some(rules(0)),
                ..Runs::default()
            };
            let now = rules(usize::from(changed));
            assert_eq!(
                runs.due(adapter, now.clone()),
                due,
                "{changed} {retrain} {adapter}"
            );
            assert!(!runs.retrain);
            // Once seen, the same rules don't make it due again.
            assert!(!runs.due(adapter, now));
        }
        // The first run is due at once, the next six hours after.
        assert!(Runs::default().due(false, Value::Null));
        let mut runs = Runs {
            last_attempt: Instant::now().checked_sub(INTERVAL),
            ..Runs::default()
        };
        assert!(runs.due(false, Value::Null));
    }

    #[test]
    fn a_failed_run_holds_off_reports_and_rules_until_its_wait_ends() {
        let mut runs = Runs {
            retrain: true,
            rules: Some(json!([])),
            ..Runs::default()
        };
        runs.failed(GPU_PREEMPTED_WAIT);
        // A plain failure meanwhile doesn't shorten the longer wait.
        runs.failed(INTERVAL);
        let changed = json!([{ "id": "r" }]);
        assert!(!runs.due(true, changed.clone()));
        // Neither is used up while it waits.
        assert!(runs.retrain);
        assert_eq!(runs.rules, Some(json!([])));
        runs.hold_until = Instant::now().checked_sub(Duration::from_secs(1));
        runs.last_attempt = Instant::now().checked_sub(Duration::from_secs(60));
        runs.retrain = false;
        // The rules that changed while it waited start a run once it ends.
        assert!(runs.due(true, changed));
    }

    #[test]
    fn macos_stopping_a_run_waits_a_day_and_a_plain_failure_six_hours() {
        for (wait, hours) in [(GPU_PREEMPTED_WAIT, 24), (INTERVAL, 6)] {
            let mut runs = Runs::default();
            runs.failed(wait);
            let until = runs.hold_until.unwrap() - runs.last_attempt.unwrap();
            assert_eq!(until, Duration::from_secs(hours * 3600));
        }
    }

    #[test]
    fn the_python_pipeline_ids_are_restamped_in_use_and_for_rollback() {
        let legacy = kass_text::pipeline::LEGACY;
        let mut saved = empty();
        saved["active"] =
            json!({ "llm": { "id": "a", "pipeline": legacy[1] }, "voice": { "id": "v" } });
        saved["history"] = json!([
            { "active": { "llm": { "id": "b", "pipeline": legacy[0] } }, "fingerprint": "f1" },
            { "active": { "llm": { "id": "c", "pipeline": "an older one" } }, "fingerprint": "f2" },
            { "active": {}, "fingerprint": "f3" },
        ]);
        assert!(restamp(&mut saved));
        assert_eq!(saved["active"]["llm"]["pipeline"], pipeline());
        assert_eq!(saved["active"]["voice"], json!({ "id": "v" }));
        assert_eq!(saved["history"][0]["active"]["llm"]["pipeline"], pipeline());
        assert_eq!(
            saved["history"][1]["active"]["llm"]["pipeline"],
            "an older one"
        );
        assert_eq!(saved["history"][2]["active"], json!({}));
        assert!(!restamp(&mut saved));
    }

    #[test]
    fn the_saved_state_is_restamped_before_the_worker_reads_it() {
        let dir = std::env::temp_dir().join(format!("kass-restamp-{}", uuid::Uuid::new_v4()));
        let database = dir.join("kass.db");
        restamp_legacy(&database);
        assert!(!dir.join("model-improvement").exists());
        let path = dir.join("model-improvement").join("state.json");
        let mut saved = json!({ "version": 1, "custom": true });
        saved["active"] = json!({ "llm": { "pipeline": kass_text::pipeline::LEGACY[0] } });
        save(&path, &saved).unwrap();
        restamp_legacy(&database);
        let restamped = load(&path);
        assert_eq!(restamped["active"]["llm"]["pipeline"], pipeline());
        assert_eq!(restamped["custom"], true);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_file_means_the_defaults() {
        let dir = std::env::temp_dir().join(format!("kass-model-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");
        fs::write(&file, r#"{"version": 2, "revision": 5}"#).unwrap();
        assert_eq!(load(&file), empty());
        let mut saved = empty();
        saved["revision"] = json!(3);
        save(&file, &saved).unwrap();
        assert_eq!(load(&file)["revision"], 3);
        fs::remove_dir_all(dir).unwrap();
    }
}
