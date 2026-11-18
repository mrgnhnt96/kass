//! One module per route group, matching the old `backend/routes/`. This
//! server answers the whole API itself: the Python worker only runs the
//! models, called from here, and nothing from outside reaches it.

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
use axum::{Json, Router};

use crate::AppState;

/// A path this server doesn't have, as FastAPI said of one. The worker's own
/// endpoints are for this server alone, so from outside they don't exist
/// either.
async fn not_found() -> impl IntoResponse {
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
        .fallback(not_found)
        .with_state(state)
}
