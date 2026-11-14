//! Saved captures, their audio, feedback and events, and learning from them. Python: `backend/routes/captures.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/capture/learning", to_worker())
        .route("/capture/learning/run", to_worker())
        .route("/capture/learning/activity", to_worker())
        .route("/capture/learning/cancel", to_worker())
        .route("/capture/learning/voice/sounds", to_worker())
        .route("/capture/learning/rollback", to_worker())
        .route("/capture/readiness", to_worker())
        .route("/capture/feedback/export", to_worker())
        .route("/captures", to_worker())
        .route("/captures/apps", to_worker())
        .route("/captures/stats", to_worker())
        .route("/captures/events", to_worker())
        .route("/captures/{capture_id}", to_worker())
        .route("/captures/{capture_id}/audio", to_worker())
        .route("/captures/{capture_id}/refine", to_worker())
        .route("/captures/{capture_id}/retranscribe", to_worker())
        .route("/captures/{capture_id}/feedback", to_worker())
        .route("/captures/{capture_id}/feedback/{report_id}", to_worker())
}
