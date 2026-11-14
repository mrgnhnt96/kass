//! The dictionary of words and names. Python: `backend/routes/dictionary.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dictionary", to_worker())
        .route("/dictionary/resolved", to_worker())
        .route("/dictionary/{entry_id}", to_worker())
}
