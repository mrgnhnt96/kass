//! One module per route group, matching `backend/routes/`. Every path still
//! goes to the Python worker; anything no module lists (a trailing slash,
//! a path the worker added since) goes there too.

pub mod capture_jobs;
pub mod capture_stream;
pub mod captures;
pub mod commands;
pub mod dictionary;
pub mod health;
pub mod learning;
pub mod links;
pub mod llm;
pub mod models;
mod request;
pub mod settings;
pub mod styles;
pub mod sync;
pub mod tasks;
pub mod teach;
pub mod transcription;
pub mod usage;
pub mod writing_style;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::any;
use axum::{Json, Router};

use crate::proxy::forward;
use crate::AppState;

/// The worker's own endpoints are for this server alone: from outside they
/// don't exist, as FastAPI says of a path it doesn't have.
async fn worker_only() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "detail": "Not Found" })),
    )
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::router())
        .merge(transcription::router())
        .merge(llm::router())
        .merge(capture_stream::router())
        .merge(captures::router())
        .merge(learning::router())
        .merge(commands::router())
        .merge(models::router())
        .merge(settings::router())
        .merge(styles::router())
        .merge(tasks::router())
        .merge(writing_style::router())
        .merge(teach::router())
        .merge(dictionary::router())
        .merge(links::router())
        .merge(usage::router())
        .merge(sync::router())
        .route("/worker", any(worker_only))
        .route("/worker/{*rest}", any(worker_only))
        .fallback(forward)
        .with_state(state)
}
