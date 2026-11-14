//! One module per route group, matching `backend/routes/`. Every path still
//! goes to the Python worker; anything no module lists (a trailing slash,
//! a path the worker added since) goes there too.

pub mod capture_stream;
pub mod captures;
pub mod commands;
pub mod dictionary;
pub mod health;
pub mod links;
pub mod llm;
pub mod models;
pub mod settings;
pub mod styles;
pub mod sync;
pub mod tasks;
pub mod transcription;
pub mod usage;
pub mod writing_style;

use axum::Router;

use crate::proxy::forward;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::router())
        .merge(transcription::router())
        .merge(llm::router())
        .merge(capture_stream::router())
        .merge(captures::router())
        .merge(commands::router())
        .merge(models::router())
        .merge(settings::router())
        .merge(styles::router())
        .merge(tasks::router())
        .merge(writing_style::router())
        .merge(dictionary::router())
        .merge(links::router())
        .merge(usage::router())
        .merge(sync::router())
        .fallback(forward)
        .with_state(state)
}
