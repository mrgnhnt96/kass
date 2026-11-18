//! Voice commands run on a selection. Python: `backend/routes/commands.py`
//! and `services/commands.py`.
//!
//! The server reads the request, finds the instruction (a transform's name
//! becomes its instruction), checks it and saves the command capture. The
//! worker only writes the rewrite (`/worker/command/rewrite`), which keeps
//! the prompt and makes sure the cleanup model is downloaded.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use kass_text::py;
use serde_json::{json, Map, Value};

use super::capture_stream::app_field;
use super::request::{body, detail, text, Problems};
use crate::captures::store::{self, Command, CommandCapture};
use crate::settings::{self, MAX_INSTRUCTION_CHARS};
use crate::stream::models;
use crate::stream::session::{resolve_instruction, MAX_SELECTION_CHARS};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/commands/run", post(run))
}

/// `CommandRunRequest`.
#[derive(Debug, PartialEq)]
struct RunRequest {
    selection: String,
    instruction: Option<String>,
    capture_id: Option<String>,
    app_bundle_id: Option<Value>,
    app_name: Option<Value>,
}

fn run_request(fields: &Map<String, Value>) -> Result<RunRequest, Problems> {
    let mut problems = Problems::default();
    let selection = match fields.get("selection") {
        None => {
            let whole = Value::Object(fields.clone());
            problems.add("missing", &[json!("selection")], "Field required", &whole);
            None
        }
        Some(Value::String(selection)) => Some(selection.clone()),
        Some(other) => {
            let loc = [json!("selection")];
            problems.add("string_type", &loc, "Input should be a valid string", other);
            None
        }
    };
    let mut optional = |name: &str| text(fields, name, usize::MAX, &mut problems).flatten();
    let instruction = optional("instruction");
    let capture_id = optional("capture_id");
    let app_bundle_id = optional("app_bundle_id").map(Value::String);
    let app_name = optional("app_name").map(Value::String);
    problems.into_result(RunRequest {
        selection: selection.unwrap_or_default(),
        instruction,
        capture_id,
        app_bundle_id,
        app_name,
    })
}

/// Python: `commands.validate_request`, with its messages.
fn validate(selection: &str, instruction: &str) -> Result<(), String> {
    if py::strip(selection).is_empty() {
        return Err("Select text to rewrite first".into());
    }
    if py::len(selection) > MAX_SELECTION_CHARS {
        return Err(format!(
            "Selection is too long for Command Mode ({} characters at most)",
            thousands(MAX_SELECTION_CHARS)
        ));
    }
    if py::strip(instruction).is_empty() {
        return Err("No instruction was heard".into());
    }
    if py::len(instruction) > MAX_INSTRUCTION_CHARS {
        return Err("Instruction is too long".into());
    }
    Ok(())
}

/// Python's `f"{n:,}"`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn failed(error: impl std::fmt::Display) -> Response {
    eprintln!("[server] commands: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

/// Rewrite a selection by an instruction or a transform's name, and save it
/// as a command capture. With `capture_id`, the instruction is that
/// capture's transcript: a command recording that went through the batch
/// upload.
async fn run(State(state): State<AppState>, bytes: Bytes) -> Response {
    let request = match body(&bytes).and_then(|fields| run_request(&fields)) {
        Ok(request) => request,
        Err(problems) => return problems.response(),
    };
    let saved = match state.db.call(settings::get).await {
        Ok(saved) => saved,
        Err(error) => return failed(error),
    };
    let (spoken, capture) = match request.capture_id.filter(|id| !id.is_empty()) {
        Some(id) => {
            let lookup = id.clone();
            match state.db.call(move |conn| store::get(conn, &lookup)).await {
                Ok(Some(row)) => (row.transcript_raw, CommandCapture::Existing(id)),
                Ok(None) => return detail(StatusCode::NOT_FOUND, "Capture not found"),
                Err(error) => return failed(error),
            }
        }
        None => {
            let spoken = request.instruction.unwrap_or_default();
            let new = CommandCapture::New {
                id: uuid::Uuid::new_v4().to_string(),
                spoken: py::strip(&spoken).to_string(),
                app_bundle_id: app_field(request.app_bundle_id.as_ref()),
                app_name: app_field(request.app_name.as_ref()),
            };
            (spoken, new)
        }
    };
    let transforms = settings::normalize_transforms(saved.command_transforms).unwrap_or_default();
    let (instruction, transform) = resolve_instruction(&spoken, &transforms);
    let selection = request.selection;
    if let Err(message) = validate(&selection, &instruction) {
        return detail(StatusCode::BAD_REQUEST, &message);
    }
    let (text, model) =
        match models::rewrite(&state.proxy, &selection, &instruction, &saved.llm_model).await {
            Ok(rewritten) => rewritten,
            // A model that isn't downloaded, a rewrite that came back empty.
            Err(failure) if failure.refused() => {
                return detail(StatusCode::BAD_REQUEST, &failure.detail)
            }
            Err(failure) => {
                eprintln!("[server] Command failed: {}", failure.detail);
                return detail(StatusCode::INTERNAL_SERVER_ERROR, &failure.detail);
            }
        };
    let command = Command {
        selection,
        instruction,
        transform: transform.map(|t| t.name),
        text,
        llm_model: model,
    };
    let sync = state.sync;
    let saved = state
        .db
        .call(move |conn| store::save_command(conn, &capture, &command, sync))
        .await;
    match saved {
        Ok(Some(capture)) => Json(capture).into_response(),
        Ok(None) => detail(StatusCode::NOT_FOUND, "Capture not found"),
        Err(error) => failed(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_required_text() {
        let missing = run_request(&Map::new()).err().unwrap();
        assert_eq!(missing.0[0]["type"], "missing");
        assert_eq!(missing.0[0]["loc"], json!(["body", "selection"]));
        let fields = json!({ "selection": null, "instruction": 3 });
        let wrong = run_request(fields.as_object().unwrap()).err().unwrap();
        assert_eq!(wrong.0.len(), 2);
        assert_eq!(wrong.0[0]["type"], "string_type");
        assert_eq!(wrong.0[1]["loc"], json!(["body", "instruction"]));
    }

    #[test]
    fn optional_fields_may_be_null() {
        let fields = json!({ "selection": "hi", "instruction": null, "app_name": " Mail " });
        let request = run_request(fields.as_object().unwrap()).ok().unwrap();
        assert_eq!(request.selection, "hi");
        assert_eq!(request.instruction, None);
        assert_eq!(app_field(request.app_name.as_ref()), Some("Mail".into()));
    }

    #[test]
    fn checks_match_python() {
        assert_eq!(
            validate("  ", "x").unwrap_err(),
            "Select text to rewrite first"
        );
        let long = "x".repeat(MAX_SELECTION_CHARS + 1);
        assert_eq!(
            validate(&long, "x").unwrap_err(),
            "Selection is too long for Command Mode (16,000 characters at most)"
        );
        assert_eq!(validate("hi", " ").unwrap_err(), "No instruction was heard");
        let long = "x".repeat(MAX_INSTRUCTION_CHARS + 1);
        assert_eq!(
            validate("hi", &long).unwrap_err(),
            "Instruction is too long"
        );
        assert!(validate("hi", "shorter").is_ok());
        // Code points, as Python counts them.
        assert!(validate(&"é".repeat(MAX_SELECTION_CHARS), "x").is_ok());
    }

    #[test]
    fn thousands_like_python() {
        assert_eq!(thousands(16_000), "16,000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
