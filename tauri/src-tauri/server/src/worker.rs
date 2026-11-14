//! Calls to the Python model worker's own endpoints (`/worker/...` routes in
//! `backend/routes/`), which only this server makes: the proxy never
//! passes a `/worker` path on from outside.

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request};
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::proxy::Proxy;

/// A loaded worker answers these in milliseconds; one that's busy loading a
/// model shouldn't hold up the request that called it for long.
const TIMEOUT: Duration = Duration::from_secs(3);

async fn post(proxy: &Proxy, path: &str, body: Value) -> Result<Value, String> {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("http://{}{path}", proxy.worker()))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .map_err(|e| e.to_string())?;
    let call = async {
        let response = proxy
            .client()
            .request(request)
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        if !status.is_success() {
            return Err(format!("{status}: {}", String::from_utf8_lossy(&bytes)));
        }
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    };
    tokio::time::timeout(TIMEOUT, call)
        .await
        .map_err(|_| format!("no answer in {TIMEOUT:?}"))?
}

/// Tell the worker the dictionary changed, so the next dictation reads it
/// again. Awaited, so a dictation started after an edit uses it.
pub async fn dictionary_changed(proxy: &Proxy) {
    if let Err(error) = post(proxy, "/worker/dictionary/changed", json!({})).await {
        eprintln!("[server] couldn't tell the model worker the dictionary changed: {error}");
    }
}

/// Whisper's token count for " <term>", each of `terms`; None while Whisper
/// isn't loaded or the worker doesn't answer.
pub async fn token_counts(proxy: &Proxy, terms: &[String]) -> Option<Vec<usize>> {
    if terms.is_empty() {
        return Some(Vec::new());
    }
    let answer = post(
        proxy,
        "/worker/dictionary/token-counts",
        json!({ "terms": terms }),
    )
    .await
    .map_err(|error| {
        eprintln!("[server] couldn't count prompt tokens in the model worker: {error}")
    })
    .ok()?;
    let counts: Vec<usize> = serde_json::from_value(answer.get("counts")?.clone()).ok()?;
    (counts.len() == terms.len()).then_some(counts)
}
