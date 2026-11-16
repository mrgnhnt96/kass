//! The root, health, shutdown and internal-build checks. Python: `backend/routes/health.py`.
//!
//! `/`, `/health` and `/shutdown` describe or stop the model worker, so the
//! worker answers them; the app checks the worker's own `/health` at startup.

use std::ffi::CString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;

use crate::proxy::to_worker;
use crate::AppState;

/// Below this much free disk, Settings warns that recordings may fail.
const LOW_DISK_MB: f64 = 500.0;

/// cargo xtask install writes this into the data dir: a build from a
/// checkout, which shows internal features (backend/internal.py).
const INTERNAL_MARKER: &str = "internal";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", to_worker())
        .route("/shutdown", to_worker())
        .route("/health", to_worker())
        .merge(
            Router::new()
                .route("/health/filesystem", get(filesystem))
                .route("/internal", get(internal))
                .layer(axum::middleware::from_fn(crate::cors::layer)),
        )
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
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
            )
                .into_response()
        }
    }
}
