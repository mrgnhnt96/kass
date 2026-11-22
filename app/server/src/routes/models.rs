//! Model downloads, status, storage and unloading, answered here as
//! `backend/routes/models.py` answered them. The files are this server's
//! (`crate::models`); which models are loaded is the worker's to say.

use std::convert::Infallible;
use std::path::{Path as FilePath, PathBuf};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use super::request::{body, detail, required, Problems};
use crate::models::progress::{Progress, Update};
use crate::models::registry::{self, MODELS};
use crate::models::{cache, migrate};
use crate::{worker, AppState};

/// A listener with nothing new hears this often that it's still connected.
const HEARTBEAT: Duration = Duration::from_secs(1);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/models/status", get(status))
        .route("/models/download", post(download))
        .route("/models/download/cancel", post(cancel))
        .route("/models/progress/{model_name}", get(model_progress))
        .route("/models/cache-dir", get(cache_dir))
        .route("/models/disk-space", get(disk_space))
        .route("/models/migrate", post(migrate_models))
        .route("/models/migrate/progress", get(migration_progress))
        .route("/models/{model_name}", delete(delete_model))
        .route("/models/{model_name}/unload", post(unload))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

fn unknown(name: &str) -> Response {
    detail(StatusCode::BAD_REQUEST, &format!("Unknown model: {name}"))
}

fn message(text: String) -> Response {
    Json(json!({ "message": text })).into_response()
}

/// A body's required string field, or what FastAPI's 422 lists.
fn field(bytes: &Bytes, name: &str) -> Result<String, Problems> {
    let fields = body(bytes)?;
    let mut problems = Problems::default();
    let value = required(&fields, name, usize::MAX, &mut problems);
    problems.into_result(value.unwrap_or_default())
}

/// A path as Python's `str(Path(...))` writes it: no trailing slash.
fn shown(path: &FilePath) -> String {
    path.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

// -- status -------------------------------------------------------------------

#[derive(Serialize)]
struct ModelStatus {
    model_name: &'static str,
    display_name: &'static str,
    hf_repo_id: &'static str,
    downloaded: bool,
    downloading: bool,
    size_mb: Option<f64>,
    loaded: bool,
}

/// Every model: on disk, downloading, loaded. A model downloading reports
/// neither downloaded nor a size until it's done.
async fn status(State(state): State<AppState>) -> Response {
    let loaded = worker::loaded_models(&state.proxy)
        .await
        .map_err(|error| eprintln!("[server] couldn't ask the model worker what's loaded: {error}"))
        .unwrap_or_default();
    let pending = state.models.progress.pending();
    let cache = state.models.cache().to_path_buf();
    let on_disk = tokio::task::spawn_blocking(move || {
        MODELS
            .iter()
            .map(|model| cache::on_disk(&cache, model.repo))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let statuses: Vec<ModelStatus> = MODELS
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let downloading = pending.iter().any(|name| name == model.name);
            let disk = on_disk.get(i).copied().filter(|_| !downloading);
            ModelStatus {
                model_name: model.name,
                display_name: model.display_name,
                hf_repo_id: model.repo,
                downloaded: disk.is_some_and(|disk| disk.downloaded),
                downloading,
                size_mb: disk.and_then(|disk| disk.size_mb),
                loaded: loaded.iter().any(|name| name == model.name),
            }
        })
        .collect();
    Json(json!({ "models": statuses })).into_response()
}

// -- downloads ----------------------------------------------------------------

async fn download(State(state): State<AppState>, bytes: Bytes) -> Response {
    let name = match field(&bytes, "model_name") {
        Ok(name) => name,
        Err(problems) => return problems.response(),
    };
    let Some(model) = registry::find(&name) else {
        return unknown(&name);
    };
    // Asked again while it's downloading: the one running carries on.
    if !state.models.progress.is_running(model.name) {
        state.models.download(&state.proxy, model);
    }
    message(format!("Model {name} download started"))
}

/// Stop a download, or dismiss one that failed.
async fn cancel(State(state): State<AppState>, bytes: Bytes) -> Response {
    let name = match field(&bytes, "model_name") {
        Ok(name) => name,
        Err(problems) => return problems.response(),
    };
    if state.models.cancel(&name) {
        message(format!("Download task for {name} cancelled"))
    } else {
        message(format!("No active task found for {name}"))
    }
}

async fn model_progress(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    events(&state.models.progress, name)
}

/// Server-sent events for one name: where it's at now if it's still going,
/// then each update until it's complete or failed, with a heartbeat each
/// second nothing happens.
fn events(progress: &Progress, name: String) -> Response {
    struct Listening {
        events: tokio::sync::broadcast::Receiver<Update>,
        first: Option<Update>,
        name: String,
        done: bool,
    }
    let events = progress.listen();
    let first = progress.get(&name).filter(Update::is_active);
    let data = |update: &Update| {
        Bytes::from(format!(
            "data: {}\n\n",
            serde_json::to_string(update).unwrap_or_default()
        ))
    };
    let stream = futures_util::stream::unfold(
        Listening {
            events,
            first,
            name,
            done: false,
        },
        move |mut listening| async move {
            if listening.done {
                return None;
            }
            if let Some(update) = listening.first.take() {
                return Some((Ok::<_, Infallible>(data(&update)), listening));
            }
            let deadline = tokio::time::Instant::now() + HEARTBEAT;
            loop {
                match tokio::time::timeout_at(deadline, listening.events.recv()).await {
                    Err(_) => return Some((Ok(Bytes::from_static(b": heartbeat\n\n")), listening)),
                    Ok(Ok(update)) if update.model_name == listening.name => {
                        listening.done = update.is_finished();
                        return Some((Ok(data(&update)), listening));
                    }
                    Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
                    Ok(Err(RecvError::Closed)) => return None,
                }
            }
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// -- storage ------------------------------------------------------------------

async fn cache_dir(State(state): State<AppState>) -> Response {
    Json(json!({ "path": shown(state.models.cache()) })).into_response()
}

/// Free MB where models download to.
async fn disk_space(State(state): State<AppState>) -> Response {
    let cache = state.models.cache().to_path_buf();
    let free_mb = tokio::task::spawn_blocking(move || cache::free_mb_at(&cache))
        .await
        .unwrap_or(0);
    Json(json!({ "free_mb": free_mb })).into_response()
}

/// Move every downloaded model to `destination` in the background; progress
/// streams from `/models/migrate/progress`. The app restarts the server on
/// the new folder once it's done.
async fn migrate_models(State(state): State<AppState>, bytes: Bytes) -> Response {
    let destination = match field(&bytes, "destination") {
        Ok(destination) => PathBuf::from(destination),
        Err(problems) => return problems.response(),
    };
    let source = state.models.cache().to_path_buf();
    let (source_shown, destination_shown) = (shown(&source), shown(&destination));
    let progress = state.models.progress.clone();
    let mut plan = match migrate::prepare(&source, &destination) {
        Err(migrate::Refused::Missing) => {
            return detail(
                StatusCode::NOT_FOUND,
                "Current model cache directory not found",
            )
        }
        Err(migrate::Refused::Invalid(why)) => return detail(StatusCode::BAD_REQUEST, why),
        Ok(None) => {
            progress.update(migrate::NAME, 1, 1, None, "complete");
            progress.mark_complete(migrate::NAME);
            return Json(json!({
                "moved": 0,
                "errors": [],
                "source": source_shown,
                "destination": destination_shown,
            }))
            .into_response();
        }
        Ok(Some(plan)) => plan,
    };
    if let Err(error) = plan.make_destination() {
        return detail(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Could not create {destination_shown}: {error}"),
        );
    }
    tokio::spawn(async move {
        let moving = progress.clone();
        match tokio::task::spawn_blocking(move || plan.run(&moving)).await {
            Ok((moved, errors)) => {
                if !errors.is_empty() {
                    eprintln!("[server] moved {moved} models; these didn't move: {errors:?}");
                }
                progress.update(migrate::NAME, 1, 1, None, "complete");
                progress.mark_complete(migrate::NAME);
            }
            Err(error) => {
                progress.update(migrate::NAME, 0, 0, None, "error");
                progress.mark_error(migrate::NAME, &error.to_string());
            }
        }
    });
    Json(json!({ "source": source_shown, "destination": destination_shown })).into_response()
}

async fn migration_progress(State(state): State<AppState>) -> Response {
    events(&state.models.progress, migrate::NAME.to_string())
}

// -- unloading and deleting ---------------------------------------------------

/// Free a model's memory; its files stay.
async fn unload(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(model) = registry::find(&name) else {
        return unknown(&name);
    };
    match worker::unload_model(&state.proxy, model.name).await {
        Ok(true) => message(format!("Model {name} unloaded successfully")),
        Ok(false) => message(format!("Model {name} is not loaded")),
        Err(error) => detail(StatusCode::INTERNAL_SERVER_ERROR, &error),
    }
}

/// Unload a model and delete its files, stopping its download if one's
/// running.
async fn delete_model(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(model) = registry::find(&name) else {
        return unknown(&name);
    };
    state.models.cancel(model.name);
    if let Err(error) = worker::unload_model(&state.proxy, model.name).await {
        return detail(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to delete model: {error}"),
        );
    }
    let folder = state.models.cache().join(cache::repo_folder(model.repo));
    if std::fs::symlink_metadata(&folder).is_err() {
        return detail(
            StatusCode::NOT_FOUND,
            &format!("Model {name} not found in cache"),
        );
    }
    let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&folder))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
    match removed {
        Ok(()) => message(format!("Model {name} deleted successfully")),
        Err(error) => detail(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to delete model cache directory: {error}"),
        ),
    }
}
