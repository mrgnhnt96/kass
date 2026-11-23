//! Usage: take reports for the daily usage report, and the stats behind the
//! Captures summary card and the Insights tab. Python:
//! `backend/routes/usage.py`, and `GET /captures/stats` in
//! `backend/routes/captures.py`.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use super::request::{body, detail, Problems};
use crate::captures::lax_bool;
use crate::captures::store::Apps;
use crate::events::query_pairs;
use crate::usage::stats::{self, Period};
use crate::{settings, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/usage/takes", post(record_take))
        .route("/captures/stats", get(capture_stats))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

/// The stats query, checked as FastAPI checks it: the last value of each
/// parameter counts. Returns the period asked for, still to be checked, and
/// which apps' captures count.
fn stats_query(query: &str) -> Result<(String, Apps), Problems> {
    let params: HashMap<String, String> = query_pairs(query, true).into_iter().collect();
    let unknown_app = match params.get("unknown_app") {
        None => false,
        Some(word) => match lax_bool(word) {
            Some(flag) => flag,
            None => {
                let mut problems = Problems::default();
                let msg = "Input should be a valid boolean, unable to interpret input";
                let loc = [json!("query"), json!("unknown_app")];
                problems.add_at("bool_parsing", &loc, msg, &json!(word));
                return Err(problems);
            }
        },
    };
    let apps = match params.get("app_bundle_id") {
        _ if unknown_app => Apps::Unknown,
        Some(bundle_id) if !bundle_id.is_empty() => Apps::One(bundle_id.clone()),
        _ => Apps::All,
    };
    let period = params.get("period").cloned().unwrap_or_else(|| "7d".into());
    Ok((period, apps))
}

/// Dictation stats for one period (today, 7d, 30d, all), in the Mac's local
/// days. `app_bundle_id` and `unknown_app` narrow them like the capture list.
async fn capture_stats(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    let (period, apps) = match stats_query(query.as_deref().unwrap_or("")) {
        Ok(query) => query,
        Err(problems) => return problems.response(),
    };
    let Some(period) = Period::parse(&period) else {
        let message = format!("period must be one of {}", Period::NAMES.join(", "));
        return detail(StatusCode::BAD_REQUEST, &message);
    };
    let now = chrono::Local::now().naive_local();
    match state
        .db
        .read(move |conn| stats::usage_stats(conn, period, &apps, now))
        .await
    {
        Ok(stats) => Json(stats).into_response(),
        Err(error) => {
            eprintln!("[server] usage stats: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
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
        (Some(mode), Some(outcome), Some(latency_ms)) if problems.is_empty() => {
            Ok((mode, outcome, latency_ms))
        }
        _ => Err(problems),
    }
}
