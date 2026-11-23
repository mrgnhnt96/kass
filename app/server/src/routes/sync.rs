//! Sync between a shared Kass and a Mac standing in for it (`crate::sync`).
//! Python: `backend/routes/sync.py` at b340f4f7.
//!
//! A shared Kass serves `/sync/snapshot` and `/sync/adapters/{kind}/{name}`
//! for a stand-in to copy, and takes what the stand-in changed at
//! `/sync/import`. A stand-in answers `/sync/status` and `/sync/wake` for
//! its own app.

use std::collections::BTreeMap;

use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::request::detail;
use crate::events::Kind;
use crate::sync::{import, mirror};
use crate::{learned, model_improvement, worker, AppState};

/// A batch's changes, as JSON: its rows' text, never its recordings.
const MAX_CHANGES_BYTES: usize = 32 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sync/snapshot", get(snapshot))
        .route("/sync/adapters/{kind}/{name}", get(adapter_file))
        // Each new capture in a batch brings its recording.
        .route(
            "/sync/import",
            post(import_changes).layer(DefaultBodyLimit::disable()),
        )
        .route("/sync/status", get(status))
        .route("/sync/wake", post(wake))
}

fn failed(error: impl std::fmt::Display) -> Response {
    eprintln!("[server] sync: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

fn data_dir(state: &AppState) -> std::path::PathBuf {
    state
        .db
        .path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default()
}

/// What a stand-in copies from this Kass.
async fn snapshot(State(state): State<AppState>) -> Response {
    let in_use = model_improvement::in_use(&state).await;
    let rows = match state.db.call(|conn| mirror::snapshot_rows(conn)).await {
        Ok(rows) => rows,
        Err(error) => return failed(error),
    };
    let data_dir = data_dir(&state);
    let snapshot = tokio::task::spawn_blocking(move || {
        mirror::snapshot(rows, &data_dir, mirror::in_use_adapters(&in_use))
    })
    .await;
    match snapshot {
        Ok(snapshot) => Json(Value::Object(snapshot)).into_response(),
        Err(error) => failed(error),
    }
}

/// A file of a trained model in use, for a stand-in to copy.
async fn adapter_file(
    State(state): State<AppState>,
    Path((kind, name)): Path<(String, String)>,
    request: Request,
) -> Response {
    let not_found = || detail(StatusCode::NOT_FOUND, "No such model file in use");
    let in_use = model_improvement::in_use(&state).await;
    let Some(path) = mirror::adapter_file(&in_use, &kind, &name) else {
        return not_found();
    };
    match ServeFile::new(&path).oneshot(request).await {
        Ok(served) => served.into_response(),
        Err(error) => failed(error),
    }
}

/// Why a batch couldn't be read: its status and detail.
type Unread = (StatusCode, &'static str);

/// The batch a stand-in sent: its changes, and new captures' recordings by
/// capture id.
async fn read_batch(
    state: &AppState,
    request: Request,
) -> Result<(Vec<import::Change>, import::Audio), Unread> {
    let unreadable = || {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Expected the changes as JSON",
        )
    };
    let mut form = Multipart::from_request(request, state)
        .await
        .map_err(|_| unreadable())?;
    let mut changes = None;
    let mut audio = import::Audio::new();
    while let Some(mut field) = form.next_field().await.map_err(|_| unreadable())? {
        match field.name() {
            Some("changes") => {
                let mut text = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(|_| unreadable())? {
                    text.extend_from_slice(&chunk);
                    if text.len() > MAX_CHANGES_BYTES {
                        return Err((StatusCode::PAYLOAD_TOO_LARGE, "The changes are too large"));
                    }
                }
                changes = Some(serde_json::from_slice(&text).map_err(|_| unreadable())?);
            }
            Some("audio") => {
                let Some(filename) = field
                    .file_name()
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                else {
                    continue;
                };
                let bytes = field.bytes().await.map_err(|_| unreadable())?;
                let id = filename
                    .rsplit_once('.')
                    .map_or(filename.as_str(), |(stem, _)| stem);
                audio.insert(id.to_string(), (filename.clone(), bytes.to_vec()));
            }
            _ => {}
        }
    }
    Ok((changes.ok_or_else(unreadable)?, audio))
}

/// Apply what a stand-in changed while this Kass was away.
async fn import_changes(State(state): State<AppState>, request: Request) -> Response {
    if state.sync.standing_in {
        return detail(
            StatusCode::CONFLICT,
            "This Kass stands in for another; send changes to that one",
        );
    }
    let (changes, audio) = match read_batch(&state, request).await {
        Ok(batch) => batch,
        Err((status, message)) => return detail(status, message),
    };
    let count = changes.len();
    let data_dir = data_dir(&state);
    let applied = state
        .db
        .call(move |conn| import::apply(conn, &data_dir, changes, &audio))
        .await;
    // The settings may have changed, even by a batch that failed partway.
    state.chosen_speech.forget();
    let applied = match applied {
        Ok(applied) => applied,
        Err(error) => return failed(error),
    };
    state.dictionaries.invalidate();
    for style in &applied.styles_deleted {
        learned::forget_style(&state, style).await;
    }
    let mut reports: BTreeMap<bool, Vec<(String, String)>> = BTreeMap::new();
    for (pair, withdrawn) in applied.reports {
        reports.entry(withdrawn).or_default().push(pair);
    }
    for (withdrawn, changed) in &reports {
        worker::reports_changed(&state, changed, *withdrawn, None).await;
    }
    let changed: Vec<_> = reports.into_values().flatten().collect();
    learned::reports_changed(&state, &changed).await;
    for capture in &applied.forgotten {
        worker::reports_changed(&state, &[], true, Some(capture)).await;
    }
    state.events.publish(Kind::Changed);
    state.events.publish(Kind::Learned);
    eprintln!("[sync] applied {count} changes from a Mac that dictated while this one was away");
    StatusCode::NO_CONTENT.into_response()
}

/// Whether this server stands in for a shared Kass, and how syncing with
/// it goes.
async fn status(State(state): State<AppState>) -> Response {
    match &state.stand_in {
        Some(stand_in) => Json(stand_in.status(&state).await).into_response(),
        None => Json(json!({ "standing_in": false })).into_response(),
    }
}

/// The app is about to dictate here: load the models a stand-in leaves
/// unloaded.
async fn wake(State(state): State<AppState>) -> StatusCode {
    if let Some(stand_in) = state.stand_in.clone() {
        stand_in.wake(&state).await;
    }
    StatusCode::NO_CONTENT
}
