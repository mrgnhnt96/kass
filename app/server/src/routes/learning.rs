//! Learning from the user's reports: the learned corrections
//! (`crate::learning`) and model improvement (`crate::model_improvement`),
//! both kept by this server, the voice-training sounds' download
//! (`crate::voice_sounds`), and for the worker, the reports and dictations a
//! model-improvement run plans from (`crate::training_data`) and the cleanup
//! text it trains on (`crate::training_cases`).

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use super::request::detail;
use crate::{
    learning, model_improvement, training_cases, training_data, voice_sounds, worker, AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/capture/learning", get(status))
        .route("/capture/learning/run", post(run))
        .route("/capture/learning/rollback", post(rollback))
        .route("/capture/learning/activity", post(activity))
        .route("/capture/learning/cancel", post(cancel))
        .route("/capture/learning/voice/sounds", post(sounds))
        .route("/capture/learning/training/prompts", post(training_prompts))
        .route("/capture/learning/training/fixed", post(training_fixed))
        .route("/capture/learning/training/reports", post(training_reports))
        .route(
            "/capture/learning/training/corrections",
            post(training_corrections),
        )
        .route(
            "/capture/learning/training/dictations",
            post(training_dictations),
        )
}

/// Only the worker, on this Mac and not in a browser, asks for training
/// text and rows: they carry what the user dictated and the styles'
/// descriptions.
fn from_this_mac(headers: &HeaderMap, peer: Option<Extension<ConnectInfo<SocketAddr>>>) -> bool {
    if headers.contains_key("origin") {
        return false;
    }
    match peer.map(|Extension(ConnectInfo(peer))| peer.ip()) {
        Some(IpAddr::V6(address)) => address
            .to_ipv4_mapped()
            .map_or(address.is_loopback(), |address| address.is_loopback()),
        Some(IpAddr::V4(address)) => address.is_loopback(),
        None => false,
    }
}

fn cases(body: &Value) -> Vec<Value> {
    body["cases"].as_array().cloned().unwrap_or_default()
}

/// `{"cases": [{"text", "flags"}]}` → `{"cases": [{"text", "bypass",
/// "system", "examples"}]}`, in order.
async fn training_prompts(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<Value>,
) -> Response {
    if !from_this_mac(&headers, peer) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(dir) = state.db.path().parent().map(ToOwned::to_owned) else {
        return detail(StatusCode::INTERNAL_SERVER_ERROR, "No data folder");
    };
    let cases = cases(&body);
    match state
        .db
        .call(move |conn| training_cases::prompts(conn, &dir, &cases))
        .await
    {
        Ok(cases) => Json(json!({ "cases": cases })).into_response(),
        Err(error) => {
            eprintln!("[learning] couldn't prepare training text: {error}");
            detail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    }
}

/// `{"rules": [...], "cases": [{"text", "language"}]}` → `{"texts": [...]}`:
/// each text with the run's learned fixes applied.
async fn training_fixed(
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<Value>,
) -> Response {
    if !from_this_mac(&headers, peer) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let cases = cases(&body);
    match tokio::task::spawn_blocking(move || training_cases::fixed(&body["rules"], &cases)).await {
        Ok(texts) => Json(json!({ "texts": texts })).into_response(),
        Err(error) => detail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

/// The rows `read` gives, as `{key: rows}`, for the worker on this Mac.
async fn training_rows<F>(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    key: &'static str,
    read: F,
) -> Response
where
    F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<Vec<Value>> + Send + 'static,
{
    if !from_this_mac(headers, peer) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.db.read(read).await {
        Ok(rows) => Json(json!({ key: rows })).into_response(),
        Err(error) => {
            eprintln!("[learning] couldn't read the training {key}: {error}");
            detail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    }
}

/// `{"limit"}` → `{"reports": [...]}`, the newest reports first.
async fn training_reports(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<Value>,
) -> Response {
    let limit = body["limit"].as_i64().unwrap_or(0);
    training_rows(&state, &headers, peer, "reports", move |conn| {
        training_data::reports(conn, limit)
    })
    .await
}

/// `{"capture_ids": [...]}` → `{"corrections": [...]}`: their explicit
/// corrections.
async fn training_corrections(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body["capture_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect();
    training_rows(&state, &headers, peer, "corrections", move |conn| {
        training_data::corrections(conn, &ids)
    })
    .await
}

/// `{"after", "limit"}` → `{"dictations": [...]}`, oldest first.
async fn training_dictations(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<Value>,
) -> Response {
    let after = body["after"].as_str().map(str::to_string);
    let limit = body["limit"].as_i64().unwrap_or(0);
    training_rows(&state, &headers, peer, "dictations", move |conn| {
        training_data::dictations(conn, after.as_deref(), limit)
    })
    .await
}

fn failed(error: learning::Failed) -> Response {
    eprintln!("[learning] {error}");
    detail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
}

fn no_worker(error: String) -> Response {
    eprintln!("[server] the model worker didn't answer: {error}");
    detail(
        StatusCode::BAD_GATEWAY,
        "The Kass model worker isn't answering",
    )
}

fn with_model(mut status: Value, model: Value) -> Response {
    status["model"] = model;
    Json(status).into_response()
}

async fn status(State(state): State<AppState>) -> Response {
    let corrections = match learning::current(&state).await {
        Ok(corrections) => corrections,
        Err(error) => return failed(error),
    };
    match model_improvement::status(&state).await {
        Ok(model) => with_model(corrections, model),
        Err(error) => no_worker(error),
    }
}

async fn run(State(state): State<AppState>) -> Response {
    let corrections = match learning::run(&state).await {
        Ok((corrections, changed)) => {
            if changed {
                worker::learning_changed(&state.proxy).await;
            }
            corrections
        }
        Err(error) => return failed(error),
    };
    match model_improvement::start(&state).await {
        Ok(model) => with_model(corrections, model),
        Err(error) => no_worker(error),
    }
}

/// The adapters' last change goes back first, then the corrections'.
async fn rollback(State(state): State<AppState>) -> Response {
    if model_improvement::can_rollback(&state).await {
        let model = match model_improvement::rollback(&state).await {
            Ok(Ok(model)) => model,
            Ok(Err(why)) => return detail(StatusCode::CONFLICT, &why),
            Err(error) => return no_worker(error),
        };
        return match learning::current(&state).await {
            Ok(corrections) => with_model(corrections, model),
            Err(error) => failed(error),
        };
    }
    match learning::rollback(&state).await {
        Ok(corrections) => {
            worker::learning_changed(&state.proxy).await;
            Json(corrections).into_response()
        }
        Err(learning::Failed::NothingToRollBack) => detail(
            StatusCode::CONFLICT,
            &learning::Failed::NothingToRollBack.to_string(),
        ),
        Err(error) => failed(error),
    }
}

async fn cancel(State(state): State<AppState>) -> Response {
    match model_improvement::cancel(&state).await {
        Ok(model) => Json(model).into_response(),
        Err(error) => no_worker(error),
    }
}

/// The user started recording: the worker stops a run and keeps the models
/// for the dictation for a while, its own background work included.
async fn activity(State(state): State<AppState>) -> Response {
    match worker::recording_started(&state.proxy).await {
        Ok(()) => Json(json!({ "paused": true })).into_response(),
        Err(error) => no_worker(error),
    }
}

/// Start the one-time download of background sounds for voice training.
async fn sounds(State(state): State<AppState>) -> Response {
    Json(voice_sounds::start(&state).await).into_response()
}
