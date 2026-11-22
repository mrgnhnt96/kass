//! Web links that open a page of the app. Python: `backend/routes/links.py`.
//!
//! `http://127.0.0.1:17493/open/captures?capture=<id>` redirects to
//! `kass://captures?capture=<id>`, which the app opens (deep_link.rs).
//! Review docs strip custom-scheme links, but keep these.

use axum::extract::Path;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/open/{*route}", get(open_in_app))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

async fn open_in_app(Path(route): Path<String>, uri: Uri) -> Response {
    let query = uri.query().filter(|query| !query.is_empty());
    let target = match query {
        Some(query) => format!("kass://{route}?{query}"),
        None => format!("kass://{route}"),
    };
    // Starlette percent-encodes the location, keeping URL punctuation.
    let location = encode_location(&target);
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

/// Starlette's `quote(url, safe=":/%#?=@[]!$&'()*+,;")`.
fn encode_location(url: &str) -> String {
    const SAFE: &str = ":/%#?=@[]!$&'()*+,;";
    let mut out = String::with_capacity(url.len());
    for byte in url.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || "_.-~".contains(c) || SAFE.contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
