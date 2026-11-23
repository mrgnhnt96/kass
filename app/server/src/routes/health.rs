//! The root, health, shutdown and internal-build checks. Python: `backend/routes/health.py`.
//!
//! `/health` describes the whole of Kass: this server's version, and the
//! model worker's process and loaded Whisper model, which the worker's own
//! `/health` reports (the app also checks that one when it starts the
//! worker). `/shutdown` stops this server, then the worker.

use std::ffi::CString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

use crate::models::cache;
use crate::models::registry::{self, Engine};
use crate::{settings, worker, AppState};

/// Below this much free disk, Settings warns that recordings may fail.
const LOW_DISK_MB: f64 = 500.0;

/// cargo xtask install writes this into the data dir: a build from a
/// checkout, which shows internal features (backend/internal.py).
const INTERNAL_MARKER: &str = "internal";

/// The app keeps this Mac's id in the data dir (src/this_mac.rs).
const MAC_ID_FILE: &str = "mac-id";

/// `/shutdown` answers before anything stops.
const ANSWER_FIRST: Duration = Duration::from_millis(100);

/// How long requests running when `/shutdown` came (a dictation finishing)
/// get before the worker they need stops.
const FINISH_RUNNING: Duration = Duration::from_secs(5);

/// The backend `/health` has always reported: MLX on Metal, since Kass only
/// runs on Apple Silicon (`backend/utils/platform_detect.py`).
const GPU_TYPE: &str = "Metal (Apple Silicon via MLX)";
const BACKEND_TYPE: &str = "mlx";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/shutdown", post(shutdown))
        .route("/health", get(health))
        .route("/health/filesystem", get(filesystem))
        .route("/internal", get(internal))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

async fn root(State(state): State<AppState>) -> Response {
    Json(json!({ "message": "kass API", "version": &*state.version })).into_response()
}

/// Stop taking requests, give the ones running a moment to finish, then
/// stop the worker. In that order, a dictation that's finishing still has
/// its models, and nothing new reaches a worker that's going. The app's API
/// goes with its worker (`main.rs`), and sees this server stopped either way.
async fn shutdown(State(state): State<AppState>) -> Response {
    tokio::spawn(async move {
        tokio::time::sleep(ANSWER_FIRST).await;
        state.stopping.stop();
        if !state.stopping.finished(FINISH_RUNNING).await {
            eprintln!("[server] stopping with requests still open");
        }
        if let Err(error) = worker::shutdown(&state.proxy).await {
            eprintln!("[server] couldn't stop the model worker: {error}");
        }
    });
    Json(json!({ "message": "Shutting down..." })).into_response()
}

/// `backend/models.py`'s `HealthResponse`, in its order.
#[derive(Serialize)]
struct Health {
    status: &'static str,
    model_loaded: bool,
    model_downloaded: Option<bool>,
    model_size: Option<Value>,
    gpu_available: bool,
    gpu_type: &'static str,
    backend_type: &'static str,
    version: String,
    started_at: Option<Value>,
    pid: Option<Value>,
    peak_memory_mb: Option<Value>,
    /// This Mac's id, which stays when its network name changes, so a Mac
    /// using this Kass can find it under the new name.
    mac_id: Option<String>,
}

/// Whether the Whisper model chosen in the capture settings is downloaded;
/// None when the settings can't be read or name no Whisper model. The
/// settings are read once and kept (`settings::ChosenSpeech`), so a health
/// check never waits behind other work on the database.
async fn whisper_downloaded(state: &AppState) -> Option<bool> {
    let chosen = match state.chosen_speech.get() {
        Ok(chosen) => chosen,
        Err(mark) => {
            let saved = state.db.call(settings::get).await.ok()?;
            state.chosen_speech.keep(mark, &saved.stt_model);
            saved.stt_model
        }
    };
    let model = registry::sized(Engine::Whisper, &chosen)?;
    let cache = state.models.cache().to_path_buf();
    tokio::task::spawn_blocking(move || {
        cache::is_cached(&cache, model.repo, cache::WHISPER_WEIGHTS)
    })
    .await
    .ok()
}

/// Up and answering, with the worker's process and loaded Whisper model.
/// A worker that doesn't answer gets the proxy's 502, as before.
async fn health(State(state): State<AppState>) -> Response {
    let (worker, model_downloaded) =
        tokio::join!(worker::health(&state.proxy), whisper_downloaded(&state));
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("[server] the model worker didn't answer /health: {error}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "detail": "The Kass model worker isn't answering" })),
            )
                .into_response();
        }
    };
    let field = |name: &str| worker.get(name).filter(|value| !value.is_null()).cloned();
    let model_loaded = worker.get("model_loaded").and_then(Value::as_bool) == Some(true);
    Json(Health {
        status: "healthy",
        model_loaded,
        model_downloaded,
        model_size: field("model_size").filter(|_| model_loaded),
        gpu_available: true,
        gpu_type: GPU_TYPE,
        backend_type: BACKEND_TYPE,
        version: state.version.to_string(),
        started_at: field("started_at"),
        pid: field("pid"),
        peak_memory_mb: field("peak_memory_mb"),
        mac_id: mac_id(&state),
    })
    .into_response()
}

/// The id the app saved for this Mac, if it has one.
fn mac_id(state: &AppState) -> Option<String> {
    let saved = std::fs::read_to_string(data_dir(state).join(MAC_ID_FILE)).ok()?;
    let id = saved.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// The folder `kass.db` is in, where everything else Kass keeps lives too.
fn data_dir(state: &AppState) -> PathBuf {
    state
        .db
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

async fn internal(State(state): State<AppState>) -> Response {
    let marker = data_dir(&state).join(INTERNAL_MARKER);
    Json(json!({ "internal": marker.is_file() })).into_response()
}

#[derive(Serialize)]
struct DirectoryCheck {
    path: String,
    exists: bool,
    writable: bool,
    error: Option<String>,
}

/// An OS error as Python's `str(OSError)` writes it.
fn os_error(error: &std::io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let text = text
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&text);
            format!("[Errno {code}] {text}: '{}'", path.display())
        }
        None => error.to_string(),
    }
}

/// Whether `dir` exists and a file can be written into it.
fn check(dir: &Path) -> DirectoryCheck {
    let exists = dir.exists();
    let mut writable = false;
    let mut error = None;
    if exists {
        let probe = dir.join(".kass_probe");
        match std::fs::write(&probe, "ok").and_then(|()| std::fs::remove_file(&probe)) {
            Ok(()) => writable = true,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                error = Some("Permission denied".to_string())
            }
            Err(e) => error = Some(os_error(&e, &probe)),
        }
        let _ = std::fs::remove_file(&probe);
    } else {
        error = Some("Directory does not exist".to_string());
    }
    let path = std::fs::canonicalize(dir)
        .unwrap_or_else(|_| std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()));
    DirectoryCheck {
        path: path.display().to_string(),
        exists,
        writable,
        error,
    }
}

/// Free and total megabytes on the disk holding `dir`, as Python's
/// `shutil.disk_usage` counts them.
fn disk_usage(dir: &Path) -> Option<(f64, f64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid C string and `stats` a writable statvfs.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as f64;
    let mb = |blocks: f64| (blocks * block / (1024.0 * 1024.0) * 10.0).round() / 10.0;
    Some((mb(stats.f_bavail as f64), mb(stats.f_blocks as f64)))
}

/// Whether the captures and data folders exist and can be written, and
/// how much disk is free.
async fn filesystem(State(state): State<AppState>) -> Response {
    let data = data_dir(&state);
    let answer = tokio::task::spawn_blocking(move || {
        let captures = data.join("captures");
        let _ = std::fs::create_dir_all(&captures);
        let directories = vec![check(&captures), check(&data)];
        let mut healthy = directories.iter().all(|d| d.exists && d.writable);
        let usage = disk_usage(&data);
        match usage {
            Some((free, _)) if free < LOW_DISK_MB => healthy = false,
            None => healthy = false,
            _ => {}
        }
        json!({
            "healthy": healthy,
            "disk_free_mb": usage.map(|(free, _)| free),
            "disk_total_mb": usage.map(|(_, total)| total),
            "directories": directories,
        })
    })
    .await;
    match answer {
        Ok(answer) => Json(answer).into_response(),
        Err(error) => {
            eprintln!("[server] filesystem health: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}
