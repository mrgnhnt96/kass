//! One-shot transcription of an uploaded recording. Python: `backend/routes/transcription.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/transcribe", to_worker())
}
