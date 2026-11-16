//! Streamed dictation: the `/captures/stream` WebSocket and its result recovery. Python: `backend/routes/capture_stream.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/captures/stream", to_worker())
        .route("/captures/stream/{session_id}/result", to_worker())
}
