//! Learning from the user's reports: the learned corrections
//! (`crate::learning`) and model improvement (`crate::model_improvement`),
//! both kept by this server, and the voice-training sounds' download
//! (`crate::voice_sounds`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::request::detail;
use crate::{learning, model_improvement, voice_sounds, worker, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/capture/learning", get(status))
        .route("/capture/learning/run", post(run))
        .route("/capture/learning/rollback", post(rollback))
        .route("/capture/learning/activity", post(activity))
        .route("/capture/learning/cancel", post(cancel))
        .route("/capture/learning/voice/sounds", post(sounds))
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
