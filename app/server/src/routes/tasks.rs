//! Model downloads in flight (`/tasks/active`), answered here as
//! `backend/routes/tasks.py` answered them, from `crate::models::progress`.

use std::collections::{BTreeSet, HashMap};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

use crate::models::progress::Update;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tasks/active", get(active))
        .route("/tasks/clear", post(clear))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

#[derive(Serialize)]
struct ActiveDownload {
    model_name: String,
    status: String,
    started_at: String,
    error: Option<String>,
    progress: Option<f64>,
    current: Option<u64>,
    total: Option<u64>,
    filename: Option<String>,
}

/// Every download started and not finished, a failed one with its error,
/// with where it's at.
async fn active(State(state): State<AppState>) -> Json<Value> {
    let progress = &state.models.progress;
    let tasks: HashMap<String, _> = progress.tasks().into_iter().collect();
    let going: HashMap<String, Update> = progress
        .all_active()
        .into_iter()
        .map(|update| (update.model_name.clone(), update))
        .collect();
    let names: BTreeSet<&String> = tasks.keys().chain(going.keys()).collect();
    let downloads: Vec<ActiveDownload> = names
        .into_iter()
        .map(|name| {
            let latest = going.get(name).cloned().or_else(|| progress.get(name));
            match tasks.get(name) {
                Some(task) => ActiveDownload {
                    model_name: name.clone(),
                    status: task.status.clone(),
                    started_at: task.started_at.clone(),
                    error: task
                        .error
                        .clone()
                        .or_else(|| progress.get(name).and_then(|update| update.error)),
                    progress: latest.as_ref().map(|update| update.progress),
                    current: latest.as_ref().map(|update| update.current),
                    total: latest.as_ref().map(|update| update.total),
                    filename: latest.and_then(|update| update.filename),
                },
                // Progress with no task: shown as it is, from when it was
                // last heard of.
                None => {
                    let update = &going[name];
                    ActiveDownload {
                        model_name: name.clone(),
                        status: update.status.clone(),
                        started_at: update.timestamp.clone(),
                        error: update.error.clone(),
                        progress: Some(update.progress),
                        current: Some(update.current),
                        total: Some(update.total),
                        filename: update.filename.clone(),
                    }
                }
            }
        })
        .collect();
    Json(json!({ "downloads": downloads }))
}

/// Stop every download and forget all progress.
async fn clear(State(state): State<AppState>) -> Json<Value> {
    state.models.progress.clear();
    Json(json!({ "message": "All task state cleared" }))
}
