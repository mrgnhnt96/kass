//! The learned writing style: examples, correction notes and teaching sessions. Python: `backend/routes/writing_style.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/writing-style", to_worker())
        .route("/writing-style/examples", to_worker())
        .route("/writing-style/examples/{example_id}", to_worker())
        .route("/writing-style/notes", to_worker())
        .route("/writing-style/notes/{note_id}", to_worker())
        .route("/writing-style/teach", to_worker())
        .route("/writing-style/teach/{session_id}", to_worker())
        .route("/writing-style/teach/{session_id}/finish", to_worker())
        .route(
            "/writing-style/teach/{session_id}/conversations",
            to_worker(),
        )
        .route(
            "/writing-style/teach/{session_id}/conversations/{conversation_id}/new-theme",
            to_worker(),
        )
        .route(
            "/writing-style/teach/{session_id}/conversations/{conversation_id}/wrap-up",
            to_worker(),
        )
        .route(
            "/writing-style/teach/{session_id}/conversations/{conversation_id}/dictated",
            to_worker(),
        )
        .route(
            "/writing-style/teach/{session_id}/conversations/{conversation_id}/restart-turn",
            to_worker(),
        )
        .route(
            "/writing-style/teach/{session_id}/conversations/{conversation_id}/replies",
            to_worker(),
        )
}
