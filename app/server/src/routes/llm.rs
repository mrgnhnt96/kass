//! Text generation with the cleanup model. Python: `backend/routes/llm.py`.
//!
//! The server checks the request the way pydantic checked
//! `LLMGenerateRequest`; the worker only generates
//! (`/worker/cleanup/generate`, without a style's flags, so on the base
//! model and with no prompt cache).

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use super::request::{body, detail, text, Problems};
use crate::models::registry::{self, Engine};
use crate::stream::models::{self, CleanupPrompt, Generate};
use crate::{settings, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/llm/generate", post(generate))
}

const MAX_PROMPT_CHARS: usize = 50_000;
const MAX_SYSTEM_CHARS: usize = 4000;
const MAX_EXAMPLES: usize = 8;
const SIZE_PATTERN: &str = r"^(0\.6B|1\.7B|4B)$";
const SIZES: [&str; 3] = ["0.6B", "1.7B", "4B"];
/// Up to 4096 tokens on the largest model; Python set no limit.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(600);

/// `LLMGenerateRequest`, checked.
#[derive(Debug, PartialEq)]
struct GenerateRequest {
    prompt: String,
    system: Option<String>,
    /// None (sent as null) for the model loaded now.
    model_size: Option<String>,
    max_tokens: u32,
    temperature: f64,
    examples: Option<Vec<Vec<String>>>,
}

fn generate_request(fields: &Map<String, Value>) -> Result<GenerateRequest, Problems> {
    let mut problems = Problems::default();
    let prompt = prompt(fields, &mut problems);
    let system = text(fields, "system", MAX_SYSTEM_CHARS, &mut problems).flatten();
    let model_size = model_size(fields, &mut problems);
    let max_tokens = whole(fields, "max_tokens", 512, (1, 4096), &mut problems);
    let temperature = number(fields, "temperature", 0.7, (0.0, 2.0), &mut problems);
    let examples = examples(fields, &mut problems);
    let checked = GenerateRequest {
        prompt: prompt.unwrap_or_default(),
        system,
        model_size: model_size.unwrap_or(None),
        max_tokens: max_tokens.unwrap_or_default() as u32,
        temperature: temperature.unwrap_or_default(),
        examples: examples.unwrap_or(None),
    };
    problems.into_result(checked)
}

fn too_long(problems: &mut Problems, loc: &[Value], value: &Value, max: usize) {
    problems.add_ctx(
        "string_too_long",
        loc,
        format!("String should have at most {max} characters"),
        value,
        json!({ "max_length": max }),
    );
}

/// Required, 1 to 50,000 characters.
fn prompt(fields: &Map<String, Value>, problems: &mut Problems) -> Option<String> {
    let loc = [json!("prompt")];
    match fields.get("prompt") {
        None => {
            let whole = Value::Object(fields.clone());
            problems.add("missing", &loc, "Field required", &whole);
            None
        }
        Some(value @ Value::String(prompt)) => {
            let length = prompt.chars().count();
            if length == 0 {
                let msg = "String should have at least 1 character";
                problems.add_ctx(
                    "string_too_short",
                    &loc,
                    msg,
                    value,
                    json!({ "min_length": 1 }),
                );
                return None;
            }
            if length > MAX_PROMPT_CHARS {
                too_long(problems, &loc, value, MAX_PROMPT_CHARS);
                return None;
            }
            Some(prompt.clone())
        }
        Some(other) => {
            problems.add("string_type", &loc, "Input should be a valid string", other);
            None
        }
    }
}

/// "0.6B" when absent; null stays None, for the model loaded now.
fn model_size(fields: &Map<String, Value>, problems: &mut Problems) -> Option<Option<String>> {
    let loc = [json!("model_size")];
    match fields.get("model_size") {
        None => Some(Some("0.6B".into())),
        Some(Value::Null) => Some(None),
        Some(Value::String(size)) if SIZES.contains(&size.as_str()) => Some(Some(size.clone())),
        Some(value @ Value::String(_)) => {
            problems.add_ctx(
                "string_pattern_mismatch",
                &loc,
                format!("String should match pattern '{SIZE_PATTERN}'"),
                value,
                json!({ "pattern": SIZE_PATTERN }),
            );
            None
        }
        Some(other) => {
            problems.add("string_type", &loc, "Input should be a valid string", other);
            None
        }
    }
}

fn out_of_range<T: Into<Value> + PartialOrd + Copy + std::fmt::Display>(
    n: T,
    (min, max): (T, T),
    loc: &[Value],
    value: &Value,
    problems: &mut Problems,
) -> bool {
    if n < min {
        let msg = format!("Input should be greater than or equal to {min}");
        problems.add_ctx(
            "greater_than_equal",
            loc,
            msg,
            value,
            json!({ "ge": min.into() }),
        );
        return true;
    }
    if n > max {
        let msg = format!("Input should be less than or equal to {max}");
        problems.add_ctx(
            "less_than_equal",
            loc,
            msg,
            value,
            json!({ "le": max.into() }),
        );
        return true;
    }
    false
}

/// Pydantic's lax int: whole numbers, booleans and numeric strings.
fn whole(
    fields: &Map<String, Value>,
    name: &str,
    default: i64,
    range: (i64, i64),
    problems: &mut Problems,
) -> Option<i64> {
    let Some(value) = fields.get(name) else {
        return Some(default);
    };
    let loc = [json!(name)];
    let n = match value {
        Value::Bool(flag) => i64::from(*flag),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(n), _) => n,
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9.2e18 => f as i64,
            _ => {
                let msg = "Input should be a valid integer, got a number with a fractional part";
                problems.add("int_from_float", &loc, msg, value);
                return None;
            }
        },
        Value::String(text) => match text.trim().parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                let msg = "Input should be a valid integer, unable to parse string as an integer";
                problems.add("int_parsing", &loc, msg, value);
                return None;
            }
        },
        _ => {
            problems.add("int_type", &loc, "Input should be a valid integer", value);
            return None;
        }
    };
    (!out_of_range(n, range, &loc, value, problems)).then_some(n)
}

/// Pydantic's lax float: numbers, booleans and numeric strings.
fn number(
    fields: &Map<String, Value>,
    name: &str,
    default: f64,
    range: (f64, f64),
    problems: &mut Problems,
) -> Option<f64> {
    let Some(value) = fields.get(name) else {
        return Some(default);
    };
    let loc = [json!(name)];
    let n = match value {
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => match text.trim().parse::<f64>() {
            Ok(n) => n,
            Err(_) => {
                let msg = "Input should be a valid number, unable to parse string as a number";
                problems.add("float_parsing", &loc, msg, value);
                return None;
            }
        },
        _ => {
            problems.add("float_type", &loc, "Input should be a valid number", value);
            return None;
        }
    };
    (!out_of_range(n, range, &loc, value, problems)).then_some(n)
}

/// Up to 8 lists of strings; null for none.
fn examples(
    fields: &Map<String, Value>,
    problems: &mut Problems,
) -> Option<Option<Vec<Vec<String>>>> {
    let value = match fields.get("examples") {
        None | Some(Value::Null) => return Some(None),
        Some(value) => value,
    };
    let loc = [json!("examples")];
    let Value::Array(items) = value else {
        problems.add("list_type", &loc, "Input should be a valid list", value);
        return None;
    };
    let mut pairs = Vec::with_capacity(items.len());
    let mut valid = true;
    for (at, item) in items.iter().enumerate() {
        let Value::Array(parts) = item else {
            let loc = [json!("examples"), json!(at)];
            problems.add("list_type", &loc, "Input should be a valid list", item);
            valid = false;
            continue;
        };
        let mut pair = Vec::with_capacity(parts.len());
        for (part_at, part) in parts.iter().enumerate() {
            match part {
                Value::String(text) => pair.push(text.clone()),
                other => {
                    let loc = [json!("examples"), json!(at), json!(part_at)];
                    problems.add("string_type", &loc, "Input should be a valid string", other);
                    valid = false;
                }
            }
        }
        pairs.push(pair);
    }
    if !valid {
        return None;
    }
    if pairs.len() > MAX_EXAMPLES {
        let count = pairs.len();
        problems.add_ctx(
            "too_long",
            &loc,
            format!("List should have at most {MAX_EXAMPLES} items after validation, not {count}"),
            value,
            json!({ "field_type": "List", "max_length": MAX_EXAMPLES, "actual_length": count }),
        );
        return None;
    }
    Some(Some(pairs))
}

/// Run a single-turn Qwen3 completion.
async fn generate(State(state): State<AppState>, bytes: Bytes) -> Response {
    let request = match body(&bytes).and_then(|fields| generate_request(&fields)) {
        Ok(request) => request,
        Err(problems) => return problems.response(),
    };
    let model_size = match request.model_size {
        Some(size) => size,
        None => match loaded_or_chosen(&state).await {
            Ok(size) => size,
            Err(error) => return detail(StatusCode::INTERNAL_SERVER_ERROR, &error),
        },
    };
    let Some(model) = registry::sized(Engine::Qwen, &model_size) else {
        let message =
            format!("Invalid LLM size '{model_size}'. Must be one of: ['0.6B', '1.7B', '4B']");
        return detail(StatusCode::BAD_REQUEST, &message);
    };
    if !state.models.ready_or_fetch(&state.proxy, model).await {
        let message = format!("Qwen3 {model_size} is being downloaded. Please wait and try again.");
        let body = json!({ "message": message, "model_name": model.name, "downloading": true });
        return (StatusCode::ACCEPTED, Json(body)).into_response();
    }
    let mut examples = Vec::new();
    for pair in request.examples.unwrap_or_default() {
        let [user, assistant]: [String; 2] = match pair.try_into() {
            Ok(pair) => pair,
            Err(_) => {
                let msg = "Each example must be a [user, assistant] pair";
                return detail(StatusCode::BAD_REQUEST, msg);
            }
        };
        examples.push((user, assistant));
    }
    let generation = Generate {
        id: format!("llm-{}", uuid::Uuid::new_v4()),
        prompt: request.prompt,
        hint: None,
        max_tokens: request.max_tokens,
        temperature: request.temperature as f32,
        cleanup: CleanupPrompt {
            model_size: model_size.clone(),
            flags: None,
            // The worker's prompt treats no system prompt and "" alike.
            system: request.system.unwrap_or_default(),
            examples,
            cache_key: None,
        },
    };
    match models::generate_within(&state.proxy, &generation, GENERATE_TIMEOUT, |_| {}).await {
        Ok(generated) => {
            Json(json!({ "text": generated.text, "model_size": model_size })).into_response()
        }
        Err(error) => {
            // The worker's error can hold paths and stack frames: logged,
            // and the client gets a plain message.
            eprintln!("[server] LLM generate failed: {error}");
            detail(StatusCode::INTERNAL_SERVER_ERROR, "LLM generation failed")
        }
    }
}

/// For a null `model_size`: the cleanup model loaded now, else the one the
/// settings choose (the one loaded at start), as Python ran the size its
/// model held.
async fn loaded_or_chosen(state: &AppState) -> Result<String, String> {
    if let Some(size) = crate::models::loaded_size(&state.proxy, Engine::Qwen).await {
        return Ok(size.to_string());
    }
    let saved = state
        .db
        .call(settings::get)
        .await
        .map_err(|e| e.to_string())?;
    Ok(saved.llm_model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(body: Value) -> Result<GenerateRequest, Vec<Value>> {
        generate_request(body.as_object().unwrap()).map_err(|problems| problems.0)
    }

    #[test]
    fn defaults_like_pydantic() {
        let request = check(json!({ "prompt": "hi" })).unwrap();
        assert_eq!(
            request,
            GenerateRequest {
                prompt: "hi".into(),
                system: None,
                model_size: Some("0.6B".into()),
                max_tokens: 512,
                temperature: 0.7,
                examples: None,
            }
        );
        let request = check(
            json!({ "prompt": "hi", "model_size": null, "max_tokens": "64",
                                     "temperature": 1, "examples": [["a", "b"]] }),
        )
        .unwrap();
        assert_eq!(request.model_size, None);
        assert_eq!((request.max_tokens, request.temperature), (64, 1.0));
        assert_eq!(request.examples, Some(vec![vec!["a".into(), "b".into()]]));
    }

    #[test]
    fn problems_like_pydantic() {
        let problems = check(json!({ "prompt": "", "model_size": "2B", "max_tokens": 0,
                                     "temperature": 2.5, "examples": [["a"], 3] }))
        .unwrap_err();
        let kinds: Vec<_> = problems
            .iter()
            .map(|p| p["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "string_too_short",
                "string_pattern_mismatch",
                "greater_than_equal",
                "less_than_equal",
                "list_type"
            ]
        );
        assert_eq!(
            problems[1]["msg"],
            r"String should match pattern '^(0\.6B|1\.7B|4B)$'"
        );
        assert_eq!(
            problems[2]["msg"],
            "Input should be greater than or equal to 1"
        );
        assert_eq!(
            problems[3]["msg"],
            "Input should be less than or equal to 2"
        );
        assert_eq!(problems[4]["loc"], json!(["body", "examples", 1]));

        let problems = check(json!({ "max_tokens": 1.5, "examples": vec![json!(["a", "b"]); 9] }))
            .unwrap_err();
        assert_eq!(problems[0]["type"], "missing");
        assert_eq!(problems[1]["type"], "int_from_float");
        assert_eq!(
            problems[2]["msg"],
            "List should have at most 8 items after validation, not 9"
        );
    }
}
