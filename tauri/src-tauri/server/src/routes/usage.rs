//! Usage reports. Python: `backend/routes/usage.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/usage/takes", to_worker())
}
