//! Model downloads, status, storage and unloading. Python: `backend/routes/models.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/models/status", to_worker())
        .route("/models/download", to_worker())
        .route("/models/download/cancel", to_worker())
        .route("/models/progress/{model_name}", to_worker())
        .route("/models/cache-dir", to_worker())
        .route("/models/disk-space", to_worker())
        .route("/models/migrate", to_worker())
        .route("/models/migrate/progress", to_worker())
        .route("/models/{model_name}", to_worker())
        .route("/models/{model_name}/unload", to_worker())
}
