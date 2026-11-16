//! Learning from the user's reports: the learned corrections, which this
//! server keeps (`crate::learning`), and the model improvement the worker
//! runs. Python: `backend/routes/captures.py`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use super::request::detail;
use crate::proxy::to_worker;
use crate::{learning, worker, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/capture/learning", get(status))
        .route("/capture/learning/run", post(run))
        .route("/capture/learning/rollback", post(rollback))
        .route("/capture/learning/activity", to_worker())
        .route("/capture/learning/cancel", to_worker())
        .route("/capture/learning/voice/sounds", to_worker())
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
    match worker::model_status(&state.proxy).await {
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
    match worker::model_start(&state.proxy).await {
        Ok(model) => with_model(corrections, model),
        Err(error) => no_worker(error),
    }
}

/// The adapters' last change goes back first, then the corrections'.
async fn rollback(State(state): State<AppState>) -> Response {
    let model = match worker::model_status(&state.proxy).await {
        Ok(model) => model,
        Err(error) => return no_worker(error),
    };
    if model["can_rollback"].as_bool().unwrap_or(false) {
        let model = match worker::model_rollback(&state.proxy).await {
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
