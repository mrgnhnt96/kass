//! Text generation with the cleanup model. Python: `backend/routes/llm.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/llm/generate", to_worker())
}
