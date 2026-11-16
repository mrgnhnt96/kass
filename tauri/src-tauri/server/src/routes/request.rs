//! Request bodies checked the way pydantic checks them, and the errors
//! FastAPI answers with, for the routes this server answers itself.

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

pub(crate) fn detail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "detail": message }))).into_response()
}

/// What a request body got wrong, as FastAPI's 422 lists it.
#[derive(Default)]
pub(crate) struct Problems(pub(crate) Vec<Value>);

impl Problems {
    pub(crate) fn add(&mut self, kind: &str, loc: &[Value], msg: impl Into<String>, input: &Value) {
        let mut at = vec![json!("body")];
        at.extend_from_slice(loc);
        self.0
            .push(json!({ "type": kind, "loc": at, "msg": msg.into(), "input": input }));
    }

    /// A problem with pydantic's `ctx`, the limit or choices it broke.
    pub(crate) fn add_ctx(
        &mut self,
        kind: &str,
        loc: &[Value],
        msg: impl Into<String>,
        input: &Value,
        ctx: Value,
    ) {
        self.add(kind, loc, msg, input);
        if let Some(Value::Object(problem)) = self.0.last_mut() {
            problem.insert("ctx".into(), ctx);
        }
    }

    pub(crate) fn response(self) -> Response {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": self.0 })),
        )
            .into_response()
    }

    pub(crate) fn into_result<T>(self, value: T) -> Result<T, Problems> {
        if self.0.is_empty() {
            Ok(value)
        } else {
            Err(self)
        }
    }
}

pub(crate) fn body(bytes: &Bytes) -> Result<Map<String, Value>, Problems> {
    let mut problems = Problems::default();
    if bytes.is_empty() {
        problems.add("missing", &[], "Field required", &Value::Null);
        return Err(problems);
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(fields)) => Ok(fields),
        Ok(other) => {
            problems.add(
                "model_attributes_type",
                &[],
                "Input should be a valid dictionary or object to extract fields from",
                &other,
            );
            Err(problems)
        }
        Err(error) => {
            let mut problems = Problems::default();
            problems.0.push(json!({
                "type": "json_invalid",
                "loc": ["body", error.column()],
                "msg": "JSON decode error",
                "input": {},
                "ctx": { "error": error.to_string() },
            }));
            Err(problems)
        }
    }
}

/// A field that may be absent (`None`) or null (`Some(None)`).
pub(crate) fn text(
    fields: &Map<String, Value>,
    name: &str,
    max: usize,
    problems: &mut Problems,
) -> Option<Option<String>> {
    let loc = [json!(name)];
    match fields.get(name)? {
        Value::Null => Some(None),
        Value::String(text) if text.chars().count() > max => {
            problems.add(
                "string_too_long",
                &loc,
                format!("String should have at most {max} characters"),
                &json!(text),
            );
            None
        }
        Value::String(text) => Some(Some(text.clone())),
        other => {
            problems.add("string_type", &loc, "Input should be a valid string", other);
            None
        }
    }
}

/// Pydantic's lax booleans: true/false, 0/1, and words like "yes" or "off".
pub(crate) fn flag(
    fields: &Map<String, Value>,
    name: &str,
    problems: &mut Problems,
) -> Option<Option<bool>> {
    let value = fields.get(name)?;
    let parsed = match value {
        Value::Null => return Some(None),
        Value::Bool(flag) => Some(*flag),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(false),
        Value::Number(n) if n.as_f64() == Some(1.0) => Some(true),
        Value::String(word) => match word.to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    };
    match parsed {
        Some(flag) => Some(Some(flag)),
        None => {
            let msg = if value.is_string() {
                "Input should be a valid boolean, unable to interpret input"
            } else {
                "Input should be a valid boolean"
            };
            problems.add("bool_parsing", &[json!(name)], msg, value);
            None
        }
    }
}
