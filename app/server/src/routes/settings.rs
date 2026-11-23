//! Capture settings and history retention. Python: `backend/routes/settings.py`.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use super::request::{body, detail, flag, text, Problems};
use crate::captures::retention;
use crate::dictionary::store::now;
use crate::settings::{self, Patch, Transform, RETENTION_DAYS};
use crate::{worker, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/settings/captures", get(get_settings).put(update_settings))
        .route("/settings/captures/retention-status", get(retention_status))
        .route(
            "/settings/captures/retention-preview",
            get(retention_preview),
        )
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

fn failed(error: crate::db::DbError) -> Response {
    eprintln!("[server] settings: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

async fn get_settings(State(state): State<AppState>) -> Response {
    match settings::load(&state.db).await {
        Ok(saved) => Json(saved).into_response(),
        Err(error) => failed(error),
    }
}

async fn update_settings(State(state): State<AppState>, bytes: Bytes) -> Response {
    let patch = match body(&bytes).and_then(|fields| patch(&fields)) {
        Ok(patch) => patch,
        Err(problems) => return problems.response(),
    };
    let swept = patch.history_retention_days.is_some();
    let switched = patch.shared_adapters.is_some();
    let sync = state.sync;
    let saved = match state
        .db
        .call(move |conn| settings::update(conn, patch, sync))
        .await
    {
        Ok(Ok(saved)) => saved,
        Ok(Err(message)) => return detail(StatusCode::BAD_REQUEST, &message),
        Err(error) => return failed(error),
    };
    if switched {
        worker::shared_adapters_switched(&state.proxy, saved.shared_adapters).await;
    }
    if swept {
        // A shorter window applies now, not at the next hourly sweep, so
        // the captures list the app reloads after saving is already trimmed.
        if let Err(error) = retention::sweep(&state, now()).await {
            eprintln!("[server] history retention sweep failed; keeping the captures: {error}");
        }
    }
    Json(saved).into_response()
}

async fn retention_status(State(state): State<AppState>) -> Response {
    let sync = state.sync;
    match state
        .db
        .call(move |conn| settings::retention_status(conn, sync))
        .await
    {
        Ok((days, confirmed, expiring)) => {
            Json(json!({ "days": days, "confirmed": confirmed, "expiring": expiring }))
                .into_response()
        }
        Err(error) => failed(error),
    }
}

/// `?days=`, read the way FastAPI reads an `int` query parameter; errs
/// with the 422 detail.
fn query_int(query: &HashMap<String, String>, name: &str) -> Result<i64, Value> {
    let problem = |kind: &str, msg: &str, input: Value| json!({ "detail": [{ "type": kind, "loc": ["query", name], "msg": msg, "input": input }] });
    let Some(raw) = query.get(name) else {
        return Err(problem("missing", "Field required", Value::Null));
    };
    let trimmed = raw.trim();
    trimmed
        .parse::<i64>()
        .ok()
        .or_else(|| {
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && n.fract() == 0.0)
                .map(|n| n as i64)
        })
        .ok_or_else(|| {
            problem(
                "int_parsing",
                "Input should be a valid integer, unable to parse string as an integer",
                json!(raw),
            )
        })
}

async fn retention_preview(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let days = match query_int(&query, "days") {
        Ok(days) => days,
        Err(problem) => return (StatusCode::UNPROCESSABLE_ENTITY, Json(problem)).into_response(),
    };
    if !RETENTION_DAYS.contains(&days) {
        return detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "days must be one of the retention choices",
        );
    }
    match state
        .db
        .call(move |conn| settings::count_expiring(conn, days))
        .await
    {
        Ok(expiring) => Json(json!({ "days": days, "expiring": expiring })).into_response(),
        Err(error) => failed(error),
    }
}

// -- the PUT body, checked the way pydantic checks `CaptureSettingsUpdate` ----

/// A string field matching one of `choices`, given as pydantic's pattern.
fn choice(
    fields: &Map<String, Value>,
    name: &str,
    choices: &[&str],
    pattern: &str,
    problems: &mut Problems,
) -> Option<String> {
    let value = text(fields, name, usize::MAX, problems)??;
    if choices.contains(&value.as_str()) {
        return Some(value);
    }
    problems.add_ctx(
        "string_pattern_mismatch",
        &[json!(name)],
        format!("String should match pattern '{pattern}'"),
        &json!(value),
        json!({ "pattern": pattern }),
    );
    None
}

/// Pydantic's lax float: numbers, booleans and numeric strings, within 0..=1.
fn volume(fields: &Map<String, Value>, name: &str, problems: &mut Problems) -> Option<f64> {
    let value = fields.get(name)?;
    let loc = [json!(name)];
    let number = match value {
        Value::Null => return None,
        Value::Number(n) => n.as_f64(),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::String(text) => match text.trim().parse::<f64>() {
            Ok(n) => Some(n),
            Err(_) => {
                let msg = "Input should be a valid number, unable to parse string as a number";
                problems.add("float_parsing", &loc, msg, value);
                return None;
            }
        },
        _ => None,
    };
    let Some(number) = number else {
        problems.add("float_type", &loc, "Input should be a valid number", value);
        return None;
    };
    if number < 0.0 {
        let msg = "Input should be greater than or equal to 0";
        problems.add_ctx("greater_than_equal", &loc, msg, value, json!({ "ge": 0.0 }));
        return None;
    }
    if number > 1.0 {
        let msg = "Input should be less than or equal to 1";
        problems.add_ctx("less_than_equal", &loc, msg, value, json!({ "le": 1.0 }));
        return None;
    }
    Some(number)
}

fn list<'a>(value: &'a Value, loc: &[Value], problems: &mut Problems) -> Option<&'a Vec<Value>> {
    match value {
        Value::Array(items) => Some(items),
        other => {
            problems.add("list_type", loc, "Input should be a valid list", other);
            None
        }
    }
}

/// A chord: key names, `min..=6` of them.
fn chord(
    fields: &Map<String, Value>,
    name: &str,
    min: usize,
    problems: &mut Problems,
) -> Option<Vec<String>> {
    const MAX: usize = 6;
    let value = fields.get(name)?;
    if value.is_null() {
        return None;
    }
    let loc = [json!(name)];
    let items = list(value, &loc, problems)?;
    let mut keys = Vec::with_capacity(items.len());
    let mut valid = true;
    for (at, item) in items.iter().enumerate() {
        match item {
            Value::String(key) => keys.push(key.clone()),
            other => {
                let loc = [json!(name), json!(at)];
                problems.add("string_type", &loc, "Input should be a valid string", other);
                valid = false;
            }
        }
    }
    if !valid {
        return None;
    }
    let count = keys.len();
    if count < min {
        let s = if min == 1 { "" } else { "s" };
        problems.add_ctx(
            "too_short",
            &loc,
            format!("List should have at least {min} item{s} after validation, not {count}"),
            value,
            json!({ "field_type": "List", "min_length": min, "actual_length": count }),
        );
        return None;
    }
    if count > MAX {
        problems.add_ctx(
            "too_long",
            &loc,
            format!("List should have at most {MAX} items after validation, not {count}"),
            value,
            json!({ "field_type": "List", "max_length": MAX, "actual_length": count }),
        );
        return None;
    }
    Some(keys)
}

fn retention_days(fields: &Map<String, Value>, problems: &mut Problems) -> Option<i64> {
    let name = "history_retention_days";
    let value = fields.get(name)?;
    let days = match value {
        Value::Null => return None,
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|n| n.fract() == 0.0).map(|n| n as i64)),
        _ => None,
    };
    if let Some(days) = days.filter(|days| RETENTION_DAYS.contains(days)) {
        return Some(days);
    }
    let expected = "0, 7, 30, 90 or 365";
    problems.add_ctx(
        "literal_error",
        &[json!(name)],
        format!("Input should be {expected}"),
        value,
        json!({ "expected": expected }),
    );
    None
}

fn transforms(fields: &Map<String, Value>, problems: &mut Problems) -> Option<Vec<Transform>> {
    let name = "command_transforms";
    let value = fields.get(name)?;
    if value.is_null() {
        return None;
    }
    let items = list(value, &[json!(name)], problems)?;
    let before = problems.0.len();
    let mut transforms = Vec::with_capacity(items.len());
    for (at, item) in items.iter().enumerate() {
        let Value::Object(transform) = item else {
            problems.add_ctx(
                "model_type",
                &[json!(name), json!(at)],
                "Input should be a valid dictionary or instance of Transform",
                item,
                json!({ "class_name": "Transform" }),
            );
            continue;
        };
        let mut field = |key: &str, required: bool| -> Option<Option<String>> {
            let loc = [json!(name), json!(at), json!(key)];
            match transform.get(key) {
                None if required => {
                    problems.add("missing", &loc, "Field required", item);
                    None
                }
                None | Some(Value::Null) if !required => Some(None),
                Some(Value::String(text)) => Some(Some(text.clone())),
                Some(other) => {
                    problems.add("string_type", &loc, "Input should be a valid string", other);
                    None
                }
                None => None,
            }
        };
        let id = field("id", false);
        let transform_name = field("name", true);
        let instruction = field("instruction", true);
        if let (Some(id), Some(Some(name)), Some(Some(instruction))) =
            (id, transform_name, instruction)
        {
            transforms.push(Transform {
                id,
                name,
                instruction,
            });
        }
    }
    (problems.0.len() == before).then_some(transforms)
}

/// Every field is optional; a null for a setting that can't be empty is
/// ignored, and fields the model doesn't know are too.
fn patch(fields: &Map<String, Value>) -> Result<Patch, Problems> {
    let mut problems = Problems::default();
    let p = &mut problems;
    let on = |name: &str, p: &mut Problems| flag(fields, name, p).flatten();
    let patch = Patch {
        stt_model: choice(
            fields,
            "stt_model",
            &["base", "small", "medium", "large", "turbo"],
            "^(base|small|medium|large|turbo)$",
            p,
        ),
        language: text(fields, "language", usize::MAX, p).flatten(),
        auto_refine: on("auto_refine", p),
        llm_model: choice(
            fields,
            "llm_model",
            &["0.6B", "1.7B", "4B"],
            "^(0\\.6B|1\\.7B|4B)$",
            p,
        ),
        smart_cleanup: on("smart_cleanup", p),
        self_correction: on("self_correction", p),
        preserve_technical: on("preserve_technical", p),
        punctuation_style: choice(
            fields,
            "punctuation_style",
            &["standard", "casual", "learned"],
            "^(standard|casual|learned)$",
            p,
        ),
        allow_auto_paste: on("allow_auto_paste", p),
        live_text: on("live_text", p),
        voice_edits: on("voice_edits", p),
        expressive: on("expressive", p),
        sound_cues: on("sound_cues", p),
        sound_cue_volume: volume(fields, "sound_cue_volume", p),
        input_device_id: text(fields, "input_device_id", usize::MAX, p),
        hotkey_enabled: on("hotkey_enabled", p),
        chord_push_to_talk_keys: chord(fields, "chord_push_to_talk_keys", 1, p),
        chord_toggle_to_talk_keys: chord(fields, "chord_toggle_to_talk_keys", 1, p),
        // Empty turns Command Mode's chord off.
        chord_command_keys: chord(fields, "chord_command_keys", 0, p),
        command_transforms: transforms(fields, p),
        history_retention_days: retention_days(fields, p),
        onboarding_completed: on("onboarding_completed", p),
        discard_audio: on("discard_audio", p),
        share_usage: on("share_usage", p),
        shared_adapters: on("shared_adapters", p),
    };
    problems.into_result(patch)
}
