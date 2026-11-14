//! Sync between a shared Kass and a Mac standing in for it. Python: `backend/routes/sync.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sync/snapshot", to_worker())
        .route("/sync/adapters/{kind}/{name}", to_worker())
        .route("/sync/import", to_worker())
        .route("/sync/status", to_worker())
        .route("/sync/wake", to_worker())
}
