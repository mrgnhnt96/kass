//! Usage reports. Python: `backend/routes/usage.py`.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Map, Value};

use super::request::{body, Problems};
use crate::{settings, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/usage/takes", post(record_take))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

/// How one take ended, for the daily usage report.
async fn record_take(State(state): State<AppState>, bytes: Bytes) -> Response {
    let (mode, outcome, latency_ms) = match body(&bytes).and_then(|fields| take(&fields)) {
        Ok(take) => take,
        Err(problems) => return problems.response(),
    };
    let sync = state.sync;
    match state
        .db
        .call(move |conn| settings::record_take(conn, mode, outcome, latency_ms, sync))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            eprintln!("[server] usage: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

/// A required field that must be one of `choices`.
fn literal(
    fields: &Map<String, Value>,
    name: &str,
    choices: &'static [&'static str],
    problems: &mut Problems,
) -> Option<&'static str> {
    let loc = [json!(name)];
    let Some(value) = fields.get(name) else {
        problems.add(
            "missing",
            &loc,
            "Field required",
            &Value::Object(fields.clone()),
        );
        return None;
    };
    if let Some(choice) = choices.iter().find(|choice| value.as_str() == Some(choice)) {
        return Some(choice);
    }
    let quoted: Vec<String> = choices.iter().map(|choice| format!("'{choice}'")).collect();
    let expected = match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    };
    problems.add_ctx(
        "literal_error",
        &loc,
        format!("Input should be {expected}"),
        value,
        json!({ "expected": expected }),
    );
    None
}

/// Pydantic's lax int, at least 0, or null.
fn latency(fields: &Map<String, Value>, problems: &mut Problems) -> Option<Option<i64>> {
    let name = "latency_ms";
    let loc = [json!(name)];
    let value = match fields.get(name) {
        None | Some(Value::Null) => return Some(None),
        Some(value) => value,
    };
    let number = match value {
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::Number(n) => match n.as_i64() {
            Some(n) => Some(n),
            None => match n.as_f64() {
                Some(f) if f.fract() == 0.0 => Some(f as i64),
                _ => {
                    let msg =
                        "Input should be a valid integer, got a number with a fractional part";
                    problems.add("int_from_float", &loc, msg, value);
                    return None;
                }
            },
        },
        Value::String(text) => match text.trim().parse::<i64>() {
            Ok(n) => Some(n),
            Err(_) => {
                let msg = "Input should be a valid integer, unable to parse string as an integer";
                problems.add("int_parsing", &loc, msg, value);
                return None;
            }
        },
        _ => None,
    };
    let Some(number) = number else {
        problems.add("int_type", &loc, "Input should be a valid integer", value);
        return None;
    };
    if number < 0 {
        let msg = "Input should be greater than or equal to 0";
        problems.add_ctx("greater_than_equal", &loc, msg, value, json!({ "ge": 0 }));
        return None;
    }
    Some(Some(number))
}

fn take(
    fields: &Map<String, Value>,
) -> Result<(&'static str, &'static str, Option<i64>), Problems> {
    let mut problems = Problems::default();
    let mode = literal(fields, "mode", &["dictation", "command"], &mut problems);
    let outcome = literal(fields, "outcome", &["delivered", "failed"], &mut problems);
    let latency_ms = latency(fields, &mut problems);
    match (mode, outcome, latency_ms) {
        (Some(mode), Some(outcome), Some(latency_ms)) if problems.0.is_empty() => {
            Ok((mode, outcome, latency_ms))
        }
        _ => Err(problems),
    }
}
