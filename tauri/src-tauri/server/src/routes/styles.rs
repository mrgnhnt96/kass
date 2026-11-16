//! Writing styles and the style each app uses. Python: `backend/routes/styles.py`.

use axum::Router;

use crate::proxy::to_worker;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/writing-styles", to_worker())
        .route("/writing-styles/{style_id}", to_worker())
        .route("/writing-styles/apps/confirm", to_worker())
        .route("/writing-styles/apps/{bundle_id}", to_worker())
}
