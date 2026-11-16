//! Voice commands run on a selection. Python: `backend/routes/commands.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/commands/run", to_worker())
}
