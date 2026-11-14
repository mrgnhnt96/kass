//! Web links that open a page of the app. Python: `backend/routes/links.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/open/{*route}", to_worker())
}
