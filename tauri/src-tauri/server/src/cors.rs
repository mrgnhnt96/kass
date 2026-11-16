//! CORS for the routes this server answers itself, as the worker's
//! Starlette `CORSMiddleware` answers them (`backend/app.py`): the app's
//! webview and the dev UI may call the API with credentials, and nothing
//! else. Routes still proxied get the worker's own headers.

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

const DEFAULT_ORIGINS: [&str; 7] = [
    "http://localhost:5173",
    "http://127.0.0.1:5173",
    "http://localhost:17493",
    "http://127.0.0.1:17493",
    "tauri://localhost",
    "https://tauri.localhost",
    "http://tauri.localhost",
];

/// The worker's `allowed_origins()`: the defaults, and any listed in
/// `KASS_CORS_ORIGINS` (comma-separated).
fn allowed(origin: &str) -> bool {
    let configured = std::env::var("KASS_CORS_ORIGINS").unwrap_or_default();
    DEFAULT_ORIGINS.contains(&origin)
        || configured
            .split(',')
            .map(str::trim)
            .any(|allowed| !allowed.is_empty() && allowed == origin)
}

fn add_vary_origin(headers: &mut HeaderMap) {
    let vary = match headers.get(header::VARY).and_then(|v| v.to_str().ok()) {
        Some(existing) => format!("{existing}, Origin"),
        None => "Origin".into(),
    };
    if let Ok(vary) = HeaderValue::from_str(&vary) {
        headers.insert(header::VARY, vary);
    }
}

fn preflight(origin: &HeaderValue, requested_headers: Option<HeaderValue>) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    if let Some(requested) = requested_headers {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested);
    }
    let ok = origin.to_str().is_ok_and(allowed);
    if ok {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        add_vary_origin(&mut headers);
    }
    let (status, text) = if ok {
        (StatusCode::OK, "OK")
    } else {
        (StatusCode::BAD_REQUEST, "Disallowed CORS origin")
    };
    let mut response = (status, Body::from(text)).into_response();
    response.headers_mut().extend(headers);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// Answer preflights and mark responses for allowed origins.
pub async fn layer(request: Request, next: Next) -> Response {
    let Some(origin) = request.headers().get(header::ORIGIN).cloned() else {
        return next.run(request).await;
    };
    if request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        let requested = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .cloned();
        return preflight(&origin, requested);
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    if origin.to_str().is_ok_and(allowed) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        add_vary_origin(headers);
    }
    response
}
