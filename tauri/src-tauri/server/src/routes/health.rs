//! The root, health, shutdown and internal-build checks. Python: `backend/routes/health.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", to_worker())
        .route("/shutdown", to_worker())
        .route("/health", to_worker())
        .route("/health/filesystem", to_worker())
        .route("/internal", to_worker())
}
