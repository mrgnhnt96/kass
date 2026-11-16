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

/// Sweeping history folds and deletes old captures in batches; a first
/// sweep after choosing a shorter window can take a while.
const SWEEP_TIMEOUT: Duration = Duration::from_secs(30);

async fn post(proxy: &Proxy, path: &str, body: Value) -> Result<Value, String> {
    post_within(proxy, path, body, TIMEOUT).await
}

async fn post_within(
    proxy: &Proxy,
    path: &str,
    body: Value,
    timeout: Duration,
) -> Result<Value, String> {
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
    tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| format!("no answer in {timeout:?}"))?
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

/// Delete captures past the history window now, so the list the app reloads
/// after saving a shorter window is already trimmed.
pub async fn sweep_history(proxy: &Proxy) {
    if let Err(error) = post_within(proxy, "/worker/history/sweep", json!({}), SWEEP_TIMEOUT).await
    {
        eprintln!("[server] couldn't sweep history in the model worker: {error}");
    }
}

/// The shared adapters setting changed: the worker switches the speech
/// model's adapter now and cleanup's on its next take.
pub async fn shared_adapters_switched(proxy: &Proxy, enabled: bool) {
    let body = json!({ "enabled": enabled });
    if let Err(error) = post(proxy, "/worker/shared-adapters/switched", body).await {
        eprintln!("[server] couldn't switch shared adapters in the model worker: {error}");
    }
}
