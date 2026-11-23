//! The stand-in's side of sync (`backend/services/sync/job.py` at
//! b340f4f7): every few seconds, see whether the shared Kass is up; when it
//! is, send it what changed here, then copy what it has.
//!
//! Also wakes the stand-in when the app falls back to it, loading the models
//! it left unloaded, puts it back to sleep once the shared Kass has been
//! back a while, and fetches the models the shared Kass uses while it's up
//! so falling back never waits on a download.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::SecondsFormat;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use tokio::io::AsyncWriteExt;

use super::mirror;
use super::rows::{self, isoformat, parse_time};
use crate::captures::resolve_storage_path;
use crate::events::Kind;
use crate::models::{cache, registry};
use crate::{model_improvement, worker, AppState};

const CHECK_EVERY: Duration = Duration::from_secs(5);
/// How often to copy from the shared Kass when nothing was sent to it.
const COPY_EVERY: Duration = Duration::from_secs(60);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(300);
/// How long the shared Kass must stay back before the stand-in frees its
/// models (~5 GB): long enough that a brief drop doesn't reload them each
/// time.
const SLEEP_AFTER: Duration = Duration::from_secs(300);
/// Changes per request: each capture brings its recording.
const BATCH: usize = 20;

#[derive(Default)]
struct Progress {
    reachable: Option<bool>,
    synced_at: Option<String>,
    revision: Option<String>,
    copied_at: Option<Instant>,
    error: Option<String>,
    awake: bool,
    /// When the shared Kass was last found back after being away.
    back_since: Option<Instant>,
    /// Hugging Face repos downloading to stand in without waiting.
    downloading: BTreeSet<String>,
}

impl Progress {
    /// Whether the shared Kass has been back long enough for a stand-in
    /// that woke to free its models again; a drop starts the wait over.
    fn sleep_due(&mut self, now: Instant) -> bool {
        if self.reachable != Some(true) {
            self.back_since = None;
            return false;
        }
        if !self.awake {
            return false;
        }
        let since = *self.back_since.get_or_insert(now);
        now.duration_since(since) >= SLEEP_AFTER
    }
}

/// This Mac's server standing in for the shared Kass at `url`.
pub struct StandIn {
    url: String,
    client: reqwest::Client,
    progress: Mutex<Progress>,
    /// Waking and sleeping, one at a time.
    switching: tokio::sync::Mutex<()>,
}

/// Why a round of syncing stopped.
enum Failed {
    /// The shared Kass couldn't be reached.
    Away(String),
    /// It answered, but turned the request down: an older Kass, say.
    Refused(String),
    /// Something here went wrong.
    Here(String),
}

impl From<reqwest::Error> for Failed {
    fn from(error: reqwest::Error) -> Self {
        if error.is_status() || error.is_decode() {
            Failed::Refused(error.to_string())
        } else {
            Failed::Away(error.to_string())
        }
    }
}

impl From<String> for Failed {
    fn from(error: String) -> Self {
        Failed::Here(error)
    }
}

/// One change as sent: the row as it is now, or that it went.
struct Batch {
    payload: Vec<Value>,
    /// Recordings: (file name, bytes).
    audio: Vec<(String, Vec<u8>)>,
    /// What to forget once sent: (kind, ref id, changed at as stored).
    sent: Vec<(String, String, String)>,
}

fn pending_batch(conn: &rusqlite::Connection, data_dir: &Path) -> rusqlite::Result<Batch> {
    let mut statement = conn.prepare(
        "SELECT kind, ref_id, deleted, changed_at FROM sync_changes ORDER BY changed_at LIMIT ?1",
    )?;
    let pending = statement
        .query_map([BATCH as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut batch = Batch {
        payload: Vec::new(),
        audio: Vec::new(),
        sent: Vec::new(),
    };
    for (kind, id, deleted, changed_at) in pending {
        let Some(table) = rows::table(&kind) else {
            continue;
        };
        let row = if deleted {
            None
        } else {
            rows::get(conn, table, &id)?
        };
        if let Some(row) = row.as_ref().filter(|_| kind == "capture") {
            let kept = row.get("audio_deleted") != Some(&Value::Bool(true));
            let path = row
                .get("audio_path")
                .and_then(Value::as_str)
                .and_then(|stored| resolve_storage_path(data_dir, stored))
                .filter(|path| kept && path.is_file());
            if let Some(path) = path {
                let suffix = path
                    .extension()
                    .map(|ext| format!(".{}", ext.to_string_lossy()))
                    .unwrap_or_default();
                if let Ok(bytes) = std::fs::read(&path) {
                    batch.audio.push((format!("{id}{suffix}"), bytes));
                }
            }
        }
        let at = parse_time(&changed_at)
            .map(isoformat)
            .unwrap_or(changed_at.clone());
        batch.payload.push(json!({
            "kind": kind,
            "id": id,
            "deleted": row.is_none(),
            "changed_at": at,
            "row": row,
        }));
        batch.sent.push((kind, id, changed_at));
    }
    Ok(batch)
}

/// The changes and recordings as `multipart/form-data`, as `/sync/import`
/// reads them.
fn form(changes: &str, audio: &[(String, Vec<u8>)], boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"changes\"\r\n\
             Content-Type: application/json\r\n\r\n{changes}\r\n"
        )
        .as_bytes(),
    );
    for (name, bytes) in audio {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"{name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

async fn pending_count(state: &AppState) -> Result<u64, String> {
    state
        .db
        .call(|conn| conn.query_row("SELECT count(*) FROM sync_changes", [], |row| row.get(0)))
        .await
        .map_err(|error| error.to_string())
}

impl StandIn {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(HEALTH_TIMEOUT)
                .build()
                .unwrap_or_default(),
            progress: Mutex::default(),
            switching: tokio::sync::Mutex::new(()),
        }
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// How syncing with the shared Kass goes, for `GET /sync/status`.
    pub async fn status(&self, state: &AppState) -> Value {
        let pending = pending_count(state).await.ok();
        let progress = self.progress();
        json!({
            "standing_in": true,
            "shared_server": self.url,
            "reachable": progress.reachable,
            "pending": pending,
            "synced_at": progress.synced_at,
            "awake": progress.awake,
            "downloading": progress.downloading,
            "error": progress.error,
        })
    }

    /// Load the models the stand-in left unloaded, once: the app is
    /// dictating here.
    pub async fn wake(&self, state: &AppState) {
        let _switching = self.switching.lock().await;
        {
            let mut progress = self.progress();
            if progress.awake {
                return;
            }
            progress.awake = true;
            progress.back_since = None;
        }
        eprintln!("[sync] the shared Kass is away; loading models to dictate on this Mac");
        if let Err(error) = worker::load_startup_models(&state.proxy).await {
            eprintln!("[sync] couldn't load the models to dictate on this Mac: {error}");
        }
    }

    /// Free the models [`wake`](Self::wake) loaded: the app dictates on the
    /// shared Kass again.
    async fn sleep(&self, state: &AppState) {
        let _switching = self.switching.lock().await;
        if !std::mem::take(&mut self.progress().awake) {
            return;
        }
        for name in worker::loaded_models(&state.proxy)
            .await
            .unwrap_or_default()
        {
            if let Err(error) = worker::unload_model(&state.proxy, &name).await {
                eprintln!("[sync] couldn't free {name}: {error}");
            }
        }
        eprintln!("[sync] the shared Kass is back; freed the models loaded to dictate on this Mac");
    }

    /// Sleep once the shared Kass has stayed back a while and nothing is
    /// dictating here: a take moved here while it was away finishes here.
    async fn sleep_when_back(&self, state: &AppState) {
        let due = self.progress().sleep_due(Instant::now());
        if due && !state.streams.dictating() && worker::models_idle(&state.proxy).await {
            self.sleep(state).await;
        }
    }

    /// Send one batch of changes; whether there was one.
    async fn push(&self, state: &AppState) -> Result<bool, Failed> {
        let data_dir = data_dir(state);
        let batch = state
            .db
            .call(move |conn| pending_batch(conn, &data_dir))
            .await
            .map_err(|error| error.to_string())?;
        if batch.sent.is_empty() {
            return Ok(false);
        }
        let boundary = format!("kass-{}", uuid::Uuid::new_v4().simple());
        let changes = Value::Array(batch.payload).to_string();
        self.client
            .post(format!("{}/sync/import", self.url))
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(form(&changes, &batch.audio, &boundary))
            .timeout(PUSH_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        let count = batch.sent.len();
        let sent = batch.sent;
        state
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                for (kind, id, changed_at) in &sent {
                    // Unless the row changed again since it was read.
                    tx.execute(
                        "DELETE FROM sync_changes WHERE kind = ?1 AND ref_id = ?2 AND changed_at = ?3",
                        [kind, id, changed_at],
                    )?;
                }
                tx.commit()
            })
            .await
            .map_err(|error| error.to_string())?;
        eprintln!("[sync] sent {count} changes to the shared Kass");
        Ok(true)
    }

    /// Fetch one file of a trained model in use on the shared Kass.
    async fn download(&self, kind: &str, name: &str, destination: &Path) -> Result<(), String> {
        let response = self
            .client
            .get(format!("{}/sync/adapters/{kind}/{name}", self.url))
            .timeout(ADAPTER_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| error.to_string())?;
        let mut temporary = destination.as_os_str().to_owned();
        temporary.push(".part");
        let temporary = std::path::PathBuf::from(temporary);
        let mut file = tokio::fs::File::create(&temporary)
            .await
            .map_err(|error| error.to_string())?;
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| error.to_string())?;
            file.write_all(&chunk)
                .await
                .map_err(|error| error.to_string())?;
        }
        file.flush().await.map_err(|error| error.to_string())?;
        tokio::fs::rename(&temporary, destination)
            .await
            .map_err(|error| error.to_string())
    }

    /// The shared Kass's trained models, copied here where they aren't yet;
    /// the kinds copied.
    async fn copy_adapters(&self, adapters: &Map<String, Value>, root: &Path) -> Vec<String> {
        let mut copied = Vec::new();
        for adapter in mirror::adapters_to_copy(adapters) {
            let (kind, id, files) = match adapter {
                Ok(adapter) => adapter,
                Err(error) => {
                    eprintln!("[sync] couldn't copy a trained model from the shared Kass: {error}");
                    continue;
                }
            };
            let folder = root.join(&id).join(&kind);
            let mut result = tokio::fs::create_dir_all(&folder)
                .await
                .map_err(|error| error.to_string());
            for name in &files {
                let destination = folder.join(name);
                if result.is_err() || destination.exists() {
                    continue;
                }
                result = self.download(&kind, name, &destination).await;
            }
            match result {
                Ok(()) => copied.push(kind),
                Err(error) => {
                    eprintln!("[sync] couldn't copy the shared Kass's {kind} model: {error}")
                }
            }
        }
        copied
    }

    /// Replace this Mac's copy with the shared Kass's snapshot.
    async fn copy(&self, state: &AppState, data: Map<String, Value>) -> Result<(), String> {
        let data = Arc::new(data);
        let rows = Arc::clone(&data);
        let replaced = state
            .db
            .call(move |conn| mirror::replace(conn, &rows))
            .await;
        // The shared Kass's settings came with it.
        state.chosen_speech.forget();
        replaced.map_err(|error| error.to_string())?;
        let data_dir = data_dir(state);
        let files = data
            .get("files")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        {
            // Not while a change of what each style learned, or a learning
            // job over the correction rules, is under way.
            let _held = crate::learned::FILES.lock().await;
            let dir = data_dir.clone();
            tokio::task::spawn_blocking(move || {
                crate::learning::between_jobs(|| mirror::write_learned(&dir, &files))
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        }
        let adapters = data
            .get("adapters")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let root = mirror::mirrored_root(&data_dir);
        let copied = self.copy_adapters(&adapters, &root).await;
        let active = mirror::active(&adapters, &root, &copied);
        state.dictionaries.invalidate();
        state.known_names.invalidate();
        let used = match worker::use_mirrored(&state.proxy, &active).await {
            Ok(kept) => model_improvement::use_mirrored(state, kept).await,
            Err(error) => Err(error),
        };
        state.events.publish(Kind::Learned);
        used
    }

    /// Download, without loading, the models the shared Kass dictates with.
    async fn fetch_models(self: &Arc<Self>, state: &AppState, settings: &Map<String, Value>) {
        let text = |key: &str| {
            settings
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let mut wanted = Vec::new();
        if !text("stt_model").is_empty() {
            // A trained voice model runs on the size it was trained on.
            let size = crate::stream::models::speech_model(&state.proxy, text("stt_model"))
                .await
                .unwrap_or_else(|_| text("stt_model").to_string());
            wanted.push(format!("whisper-{size}"));
        }
        if settings.get("auto_refine") == Some(&Value::Bool(true)) {
            wanted.push(format!("qwen3-{}", text("llm_model").to_lowercase()));
        }
        for model in wanted.iter().filter_map(|name| registry::find(name)) {
            if cache::on_disk(state.models.cache(), model.repo).downloaded
                || !self.progress().downloading.insert(model.repo.to_string())
            {
                continue;
            }
            let (stand_in, models) = (Arc::clone(self), state.models.clone());
            tokio::spawn(async move {
                eprintln!(
                    "[sync] downloading {} so this Mac can dictate while the shared Kass is away",
                    model.repo
                );
                if let Err(error) = models.fetch(model).await {
                    eprintln!("[sync] couldn't download {}: {error}", model.repo);
                }
                stand_in.progress().downloading.remove(model.repo);
            });
        }
    }

    async fn sync_once(self: &Arc<Self>, state: &AppState) -> Result<(), Failed> {
        let health = self
            .client
            .get(format!("{}/health", self.url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status);
        let reached = health.is_ok();
        self.progress().reachable = Some(reached);
        if !reached {
            return Ok(());
        }
        let mut sent = false;
        while self.push(state).await? {
            sent = true;
        }
        let fresh = {
            let progress = self.progress();
            progress.revision.is_some()
                && progress
                    .copied_at
                    .is_some_and(|at| at.elapsed() < COPY_EVERY)
        };
        if (!sent && fresh) || pending_count(state).await? > 0 {
            return Ok(());
        }
        let data: Map<String, Value> = self
            .client
            .get(format!("{}/sync/snapshot", self.url))
            .timeout(SNAPSHOT_TIMEOUT)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        self.progress().copied_at = Some(Instant::now());
        let revision = data
            .get("revision")
            .and_then(Value::as_str)
            .map(str::to_string);
        let settings = data.get("settings").and_then(Value::as_object).cloned();
        if revision != self.progress().revision {
            self.copy(state, data).await?;
            self.progress().revision = revision;
            eprintln!("[sync] copied settings, dictionary and learning from the shared Kass");
        }
        {
            let mut progress = self.progress();
            progress.synced_at =
                Some(chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false));
            progress.error = None;
        }
        if let Some(settings) = settings {
            self.fetch_models(state, &settings).await;
        }
        Ok(())
    }

    /// Note a failed sync, logging it only when it's a new failure: they
    /// repeat every few seconds.
    fn failed(&self, message: String) {
        let mut progress = self.progress();
        if progress.error.as_deref() != Some(message.as_str()) {
            eprintln!("[sync] {message}");
        }
        progress.error = Some(message);
    }
}

fn data_dir(state: &AppState) -> std::path::PathBuf {
    state
        .db
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Sync with the shared Kass until the server stops.
pub async fn run(stand_in: Arc<StandIn>, state: AppState) {
    let mut ticks = tokio::time::interval(CHECK_EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let url = stand_in.url.clone();
        match stand_in.sync_once(&state).await {
            Ok(()) => {}
            Err(Failed::Refused(error)) => stand_in.failed(format!(
                "The shared Kass at {url} turned down a sync: {error}"
            )),
            Err(Failed::Away(error)) => {
                stand_in.progress().reachable = Some(false);
                stand_in.failed(format!(
                    "Could not sync with the shared Kass at {url}: {error}"
                ));
            }
            Err(Failed::Here(error)) => {
                stand_in.failed(format!("Syncing with the shared Kass failed: {error}"))
            }
        }
        stand_in.sleep_when_back(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in that woke, checked `after` each step with the shared
    /// Kass `reachable` or not: whether it's due to sleep at each check.
    fn checks(steps: &[(u64, bool)]) -> Vec<bool> {
        let mut progress = Progress {
            awake: true,
            ..Progress::default()
        };
        let mut now = Instant::now();
        steps
            .iter()
            .map(|&(after, reachable)| {
                now += Duration::from_secs(after);
                progress.reachable = Some(reachable);
                progress.sleep_due(now)
            })
            .collect()
    }

    #[test]
    fn the_stand_in_sleeps_once_the_shared_kass_stays_back() {
        let wait = SLEEP_AFTER.as_secs();
        assert_eq!(
            checks(&[(0, true), (wait - 1, true), (1, true)]),
            [false, false, true]
        );
    }

    #[test]
    fn a_brief_return_keeps_the_stand_in_awake() {
        let wait = SLEEP_AFTER.as_secs();
        assert_eq!(
            checks(&[(0, true), (wait - 1, false), (1, true), (wait - 1, true)]),
            [false, false, false, false]
        );
    }

    #[test]
    fn a_stand_in_that_never_woke_has_nothing_to_free() {
        let mut progress = Progress {
            reachable: Some(true),
            ..Progress::default()
        };
        let later = Instant::now() + SLEEP_AFTER * 2;
        assert!(!progress.sleep_due(Instant::now()));
        assert!(!progress.sleep_due(later));
    }

    #[test]
    fn the_form_holds_the_changes_then_each_recording() {
        let body = form("[]", &[("c1.wav".into(), b"RIFF".to_vec())], "b");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "--b\r\nContent-Disposition: form-data; name=\"changes\"\r\n\
             Content-Type: application/json\r\n\r\n[]\r\n\
             --b\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"c1.wav\"\r\n\
             Content-Type: application/octet-stream\r\n\r\nRIFF\r\n--b--\r\n"
        );
    }
}
