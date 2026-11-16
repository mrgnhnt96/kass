//! Capture settings and history retention. Python: `backend/routes/settings.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/settings/captures", to_worker())
        .route("/settings/captures/retention-status", to_worker())
        .route("/settings/captures/retention-preview", to_worker())
}
