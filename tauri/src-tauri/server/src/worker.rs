//! Calls to the Python model worker's own endpoints (`/worker/...` routes in
//! `backend/routes/`), which only this server makes: the proxy never
//! passes a `/worker` path on from outside.

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request};
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::proxy::Proxy;
use crate::AppState;

/// A loaded worker answers these in milliseconds; one that's busy loading a
/// model shouldn't hold up the request that called it for long.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Loading a model the first time compiles it for the GPU; a big one on a
/// busy Mac takes a while.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Unloading waits for a dictation using the model to finish.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Checking trained models reads and hashes their files.
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
    send(proxy, request, timeout).await
}

async fn send(proxy: &Proxy, request: Request<Body>, timeout: Duration) -> Result<Value, String> {
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

/// The worker's own `/health`, which the app also checks when it starts the
/// worker: which Whisper model is loaded (`model_loaded`, `model_size`) and
/// the worker process's `pid`, `started_at` and `peak_memory_mb`.
pub async fn health(proxy: &Proxy) -> Result<Value, String> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("http://{}/health", proxy.worker()))
        .body(Body::empty())
        .map_err(|e| e.to_string())?;
    send(proxy, request, TIMEOUT).await
}

/// Ask the worker to exit (its own `POST /shutdown`). It answers first,
/// then stops a moment later.
pub async fn shutdown(proxy: &Proxy) -> Result<(), String> {
    post(proxy, "/shutdown", json!({})).await.map(drop)
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

/// The shared adapters setting changed: the worker switches the speech
/// model's adapter now and cleanup's on its next take.
pub async fn shared_adapters_switched(proxy: &Proxy, enabled: bool) {
    let body = json!({ "enabled": enabled });
    if let Err(error) = post(proxy, "/worker/shared-adapters/switched", body).await {
        eprintln!("[server] couldn't switch shared adapters in the model worker: {error}");
    }
}

/// Reports changed: withdrawn ones may be in the cleanup adapter's training
/// data, so it retrains at the next idle moment; a deleted capture's audio
/// (`forgotten`) leaves the voice training bank.
pub async fn reports_changed(
    state: &AppState,
    changed: &[(String, String)],
    withdrawn: bool,
    forgotten: Option<&str>,
) {
    if withdrawn && !changed.is_empty() {
        state.model_improvement.retrain();
    }
    let Some(capture_id) = forgotten else {
        return;
    };
    let body = json!({ "capture_id": capture_id });
    if let Err(error) = post(&state.proxy, "/worker/learning/model/forget", body).await {
        eprintln!("[server] couldn't drop a deleted capture from the voice bank: {error}");
    }
}

/// Whether nothing is using the models (no dictation for a while, no
/// training running): when background learning may take the CPU. False
/// when the worker doesn't answer.
pub async fn models_idle(proxy: &Proxy) -> bool {
    idle_answer(proxy).await["idle"].as_bool().unwrap_or(false)
}

/// The worker's foreground generation while the models are idle, else
/// None: it changes with every dictation, so a background job that started
/// at it sees with [`foreground_generation`] whether one interrupted it.
/// Python: `manager.idle_generation()`.
pub async fn idle_generation(proxy: &Proxy) -> Option<u64> {
    let answer = idle_answer(proxy).await;
    match answer["idle"].as_bool() {
        Some(true) => answer["generation"].as_u64(),
        _ => None,
    }
}

/// The worker's foreground generation now; None when it doesn't answer.
pub async fn foreground_generation(proxy: &Proxy) -> Option<u64> {
    idle_answer(proxy).await["generation"].as_u64()
}

async fn idle_answer(proxy: &Proxy) -> Value {
    post(proxy, "/worker/learning/idle", json!({}))
        .await
        .unwrap_or_else(|error| {
            eprintln!("[server] couldn't ask the model worker whether it's idle: {error}");
            Value::Null
        })
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

/// Whether a dictation is recording or finishing right now.
pub async fn models_dictating(proxy: &Proxy) -> Result<bool, String> {
    let answer = post(proxy, "/worker/learning/idle", json!({})).await?;
    Ok(answer["dictating"].as_bool().unwrap_or(false))
}

/// Planning a run reads every report's recording and the voice bank.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(600);

/// The worker gives training and evaluation 30 minutes, then judges it.
const TRAIN_TIMEOUT: Duration = Duration::from_secs(35 * 60);

/// Plan a model-improvement run (`manager.prepare`): whether it's ready,
/// its fingerprint and run id, and the counts the state keeps.
pub async fn model_prepare(proxy: &Proxy, body: Value) -> Result<Value, String> {
    post_within(
        proxy,
        "/worker/learning/model/prepare",
        body,
        PREPARE_TIMEOUT,
    )
    .await
}

/// Train, evaluate and judge the run the worker prepared (`manager.train`).
/// Answers once the run ends.
pub async fn model_train(proxy: &Proxy, body: Value) -> Result<Value, String> {
    post_within(proxy, "/worker/learning/model/train", body, TRAIN_TIMEOUT).await
}

fn active_of(answer: Value) -> Result<serde_json::Map<String, Value>, String> {
    match answer.get("active") {
        Some(Value::Object(active)) => Ok(active.clone()),
        _ => Err(format!("the worker's trained models didn't read: {answer}")),
    }
}

/// The trained models in `active` that pass their checks.
pub async fn model_check(
    proxy: &Proxy,
    active: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    let body = json!({ "active": active });
    active_of(
        post_within(
            proxy,
            "/worker/learning/model/check",
            body,
            LEARNING_TIMEOUT,
        )
        .await?,
    )
}

/// Dictation uses the trained models in `active` from now on; the ones it
/// uses (those that pass their checks).
pub async fn model_use(
    proxy: &Proxy,
    active: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    let body = json!({ "active": active });
    active_of(post_within(proxy, "/worker/learning/model/use", body, LEARNING_TIMEOUT).await?)
}

/// Stop a model-improvement run in progress.
pub async fn model_stop(proxy: &Proxy) -> Result<(), String> {
    post(proxy, "/worker/learning/model/stop", json!({}))
        .await
        .map(|_| ())
}

/// The user started recording: stop a run, and leave the models to the
/// dictation for a while (the worker's recording window).
pub async fn recording_started(proxy: &Proxy) -> Result<(), String> {
    let body = json!({ "recording": true });
    post(proxy, "/worker/learning/model/stop", body)
        .await
        .map(|_| ())
}

/// The trained models the worker stopped using since the last call.
pub async fn model_reports(proxy: &Proxy) -> Result<Vec<Value>, String> {
    let answer = post(proxy, "/worker/learning/model/reports", json!({})).await?;
    Ok(answer["reports"].as_array().cloned().unwrap_or_default())
}

/// The voice bank's recordings.
pub async fn model_voice(proxy: &Proxy) -> Result<Value, String> {
    post(proxy, "/worker/learning/model/voice", json!({})).await
}

/// What each of `prompts` takes in the cleanup model's prompt cache, in
/// bytes, once `model_size` is loaded to count it; None while the worker
/// doesn't answer or can't tell.
pub async fn cache_bytes(
    proxy: &Proxy,
    model_size: &str,
    prompts: &[(String, Vec<(String, String)>)],
) -> Option<Vec<Option<u64>>> {
    let prompts: Vec<Value> = prompts
        .iter()
        .map(|(system, examples)| json!({ "system": system, "examples": examples }))
        .collect();
    let body = json!({ "model_size": model_size, "prompts": prompts });
    match post(proxy, "/worker/cleanup/cache-bytes", body).await {
        Ok(answer) => serde_json::from_value(answer.get("bytes")?.clone()).ok(),
        Err(error) => {
            eprintln!(
                "[server] couldn't measure the styles' prompt caches in the model worker: {error}"
            );
            None
        }
    }
}

/// The models loaded now, by name (`whisper-turbo`, `qwen3-4b`).
pub async fn loaded_models(proxy: &Proxy) -> Result<Vec<String>, String> {
    let answer = post(proxy, "/worker/models/loaded", json!({})).await?;
    serde_json::from_value(answer["loaded"].clone()).map_err(|e| e.to_string())
}

/// Load a model whose files are downloaded; the worker's error when it
/// can't.
pub async fn load_model(proxy: &Proxy, name: &str) -> Result<(), String> {
    let body = json!({ "model_name": name });
    let answer = post_within(proxy, "/worker/models/load", body, LOAD_TIMEOUT).await?;
    match answer["error"].as_str() {
        Some(error) => Err(error.to_string()),
        None => Ok(()),
    }
}

/// Unload a model; whether it was loaded, or the worker's error.
pub async fn unload_model(proxy: &Proxy, name: &str) -> Result<bool, String> {
    let body = json!({ "model_name": name });
    let answer = post_within(proxy, "/worker/models/unload", body, UNLOAD_TIMEOUT).await?;
    if let Some(error) = answer["error"].as_str() {
        return Err(error.to_string());
    }
    Ok(answer["was_loaded"].as_bool().unwrap_or(false))
}

/// Load the models dictation starts with, as the worker does on start
/// unless it stands in for a shared Kass: the app now dictates here.
pub async fn load_startup_models(proxy: &Proxy) -> Result<(), String> {
    post_within(proxy, "/worker/models/startup", json!({}), LOAD_TIMEOUT)
        .await
        .map(|_| ())
}

/// A stand-in copied a shared Kass's settings, dictionary, learning and
/// trained models: the worker uses `active` (already copied here) and
/// rereads everything else. The ones it uses (those that pass their checks).
pub async fn use_mirrored(
    proxy: &Proxy,
    active: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    let body = json!({ "active": active });
    active_of(post_within(proxy, "/worker/sync/mirrored", body, LEARNING_TIMEOUT).await?)
}
