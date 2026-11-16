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

/// Learning from a correction recounts the writing style's corrections and
/// adds the words it fixed to the dictionary: database work, but more of it.
const LEARNING_TIMEOUT: Duration = Duration::from_secs(30);

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

/// A spoken correction as the user would have typed it: letters they
/// spelled ("M-E-G-H-A-N") joined into one word.
pub async fn join_spelling(proxy: &Proxy, text: &str) -> Result<String, String> {
    let answer = post(
        proxy,
        "/worker/captures/join-spelling",
        json!({ "text": text }),
    )
    .await?;
    answer
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the answer has no text".to_string())
}

/// Each of `newer` without the changes that turned `before` into `removed`,
/// keeping its own: the reports made after one the user withdraws.
pub async fn take_back(
    proxy: &Proxy,
    before: &str,
    removed: &str,
    newer: &[String],
) -> Result<Vec<String>, String> {
    if newer.is_empty() {
        return Ok(Vec::new());
    }
    let body = json!({ "before": before, "removed": removed, "newer": newer });
    let answer = post(proxy, "/worker/captures/take-back", body).await?;
    let texts: Vec<String> = answer
        .get("texts")
        .cloned()
        .and_then(|texts| serde_json::from_value(texts).ok())
        .ok_or_else(|| "the answer has no texts".to_string())?;
    if texts.len() != newer.len() {
        return Err(format!("{} texts for {} reports", texts.len(), newer.len()));
    }
    Ok(texts)
}

/// A report whose fixed words go in the dictionary.
pub struct Learn<'a> {
    pub report_id: &'a str,
    pub source: &'a str,
    pub original: &'a str,
    pub expected: &'a str,
}

/// Tell the worker reports changed, so it learns from them again: the
/// dictionary words the `learn` reports fixed, then for each (target,
/// source) changed, correction learning, known names, examples and the
/// writing style; and that a deleted capture's audio leaves the voice
/// training bank. Awaited, so the next dictation uses what was learned.
pub async fn reports_changed(
    proxy: &Proxy,
    learn: &[Learn<'_>],
    changed: &[(String, String)],
    withdrawn: bool,
    forgotten: Option<&str>,
) {
    let learn: Vec<Value> = learn
        .iter()
        .map(|report| {
            json!({
                "report_id": report.report_id,
                "source": report.source,
                "original": report.original,
                "expected": report.expected,
            })
        })
        .collect();
    let changed: Vec<Value> = changed
        .iter()
        .map(|(target, source)| json!({ "target": target, "source": source, "withdrawn": withdrawn }))
        .collect();
    let body = json!({ "learn": learn, "changed": changed, "forgotten": forgotten });
    if let Err(error) = post_within(
        proxy,
        "/worker/captures/reports-changed",
        body,
        LEARNING_TIMEOUT,
    )
    .await
    {
        eprintln!("[server] couldn't tell the model worker the corrections changed: {error}");
    }
}

/// Whether nothing is using the models (no dictation for a while, no
/// training running): when background learning may take the CPU. False
/// when the worker doesn't answer.
pub async fn models_idle(proxy: &Proxy) -> bool {
    match post(proxy, "/worker/learning/idle", json!({})).await {
        Ok(answer) => answer["idle"].as_bool().unwrap_or(false),
        Err(error) => {
            eprintln!("[server] couldn't ask the model worker whether it's idle: {error}");
            false
        }
    }
}

/// Tell the worker the learned corrections changed, so dictations use them
/// and the cleanup adapter is retested against them.
pub async fn learning_changed(proxy: &Proxy) {
    if let Err(error) = post_within(
        proxy,
        "/worker/learning/reload",
        json!({}),
        LEARNING_TIMEOUT,
    )
    .await
    {
        eprintln!("[server] couldn't tell the model worker the corrections changed: {error}");
    }
}

/// The model-improvement job's status (`manager.status()`).
pub async fn model_status(proxy: &Proxy) -> Result<Value, String> {
    post(proxy, "/worker/learning/model", json!({})).await
}

/// Start a model-improvement run if one is due (`manager.start()`).
pub async fn model_start(proxy: &Proxy) -> Result<Value, String> {
    post_within(
        proxy,
        "/worker/learning/model/start",
        json!({}),
        LEARNING_TIMEOUT,
    )
    .await
}

/// Go back to the previous adapters; `Ok(Err(why))` when there are none.
pub async fn model_rollback(proxy: &Proxy) -> Result<Result<Value, String>, String> {
    let answer = post_within(
        proxy,
        "/worker/learning/model/rollback",
        json!({}),
        LEARNING_TIMEOUT,
    )
    .await?;
    Ok(match answer["refused"].as_str() {
        Some(why) => Err(why.to_string()),
        None => Ok(answer),
    })
}
