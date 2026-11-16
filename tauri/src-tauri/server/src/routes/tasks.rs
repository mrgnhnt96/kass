//! Background tasks in progress. Python: `backend/routes/tasks.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tasks/active", to_worker())
        .route("/tasks/clear", to_worker())
}
