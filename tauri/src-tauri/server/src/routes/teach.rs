//! Teaching a writing style by replying: the sessions live here
//! (`crate::teach`); the worker only writes the other side's turns.
//! Python: the teach routes in `backend/routes/writing_style.py`.

use std::collections::HashSet;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::request::{body, detail, required, Problems};
use super::writing_style::{failed, resolve, StyleQuery, STYLE_NOT_FOUND};
use crate::learned;
use crate::settings;
use crate::stream::models::{self, CleanupPrompt, Generate};
use crate::styles;
use crate::teach::{self, catalog, preview, text, Dictated, Refused};
use crate::AppState;

const SESSION: &str = "/writing-style/teach/{session_id}";
const CONVERSATION: &str = "/writing-style/teach/{session_id}/conversations/{conversation_id}";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/writing-style/teach", post(start))
        .route(SESSION, delete(discard))
        .route(&format!("{SESSION}/finish"), post(finish))
        .route(&format!("{SESSION}/conversations"), post(add_conversation))
        .route(&format!("{CONVERSATION}/new-theme"), post(new_theme))
        .route(&format!("{CONVERSATION}/wrap-up"), post(wrap_up))
        .route(&format!("{CONVERSATION}/dictated"), get(dictated))
        .route(&format!("{CONVERSATION}/restart-turn"), post(restart_turn))
        .route(&format!("{CONVERSATION}/replies"), post(send_reply))
}

fn refused(refusal: Refused) -> Response {
    match refusal {
        Refused::Expired => detail(
            StatusCode::NOT_FOUND,
            "This teaching session expired. Start it again.",
        ),
        Refused::NoConversation => detail(StatusCode::NOT_FOUND, "Conversation not found"),
        Refused::Invalid(message) => detail(StatusCode::BAD_REQUEST, &message),
    }
}

/// The session as the app shows it; the worker hears the style being taught.
fn session(state: &AppState, answer: Result<Value, Refused>) -> Response {
    state.teach.tell_worker(&state.proxy);
    match answer {
        Ok(answer) => Json(answer).into_response(),
        Err(refusal) => refused(refusal),
    }
}

async fn start(State(state): State<AppState>, Query(query): Query<StyleQuery>) -> Response {
    let style = match resolve(&state, query).await {
        Ok(Some(style)) => style,
        Ok(None) => return detail(StatusCode::NOT_FOUND, STYLE_NOT_FOUND),
        Err(error) => return failed(error),
    };
    let dictionaries = state.dictionaries.clone();
    let teaching = state.teach.teaching_style();
    let found = style.clone();
    let looked_up = state
        .db
        .call(move |conn| {
            let snapshot = styles::load(conn, teaching)?;
            let apps = teach::style_apps(conn, &snapshot, &found)?;
            let terms = dictionaries
                .for_app(conn, None, Some(&found))?
                .terms
                .clone();
            Ok((catalog::kinds_for_apps(&apps), terms))
        })
        .await;
    match looked_up {
        Ok((own, terms)) => {
            let started = state.teach.start(&style, own, terms);
            session(&state, Ok(started))
        }
        Err(error) => failed(error),
    }
}

async fn discard(State(state): State<AppState>, Path(session_id): Path<String>) -> Response {
    state.teach.discard(&session_id);
    state.teach.tell_worker(&state.proxy);
    StatusCode::NO_CONTENT.into_response()
}

/// `kind`, one of the catalog's, as pydantic checks a `Literal`.
fn kind(bytes: &Bytes) -> Result<String, Problems> {
    let fields = body(bytes)?;
    let mut problems = Problems::default();
    let loc = [json!("kind")];
    let Some(value) = fields.get("kind") else {
        problems.add(
            "missing",
            &loc,
            "Field required",
            &Value::Object(fields.clone()),
        );
        return Err(problems);
    };
    if let Some(found) = value.as_str().and_then(catalog::kind) {
        return Ok(found.id.to_string());
    }
    let quoted: Vec<String> = catalog::KINDS
        .iter()
        .map(|kind| format!("'{}'", kind.id))
        .collect();
    let (last, rest) = quoted.split_last().expect("the catalog has kinds");
    let expected = format!("{} or {last}", rest.join(", "));
    problems.add_ctx(
        "literal_error",
        &loc,
        format!("Input should be {expected}"),
        value,
        json!({ "expected": expected }),
    );
    Err(problems)
}

async fn add_conversation(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    bytes: Bytes,
) -> Response {
    let kind = match kind(&bytes) {
        Ok(kind) => kind,
        Err(problems) => return problems.response(),
    };
    let added = state.teach.add_conversation(&session_id, &kind);
    session(&state, added)
}

async fn new_theme(
    State(state): State<AppState>,
    Path((session_id, conversation_id)): Path<(String, String)>,
) -> Response {
    let replaced = state.teach.new_theme(&session_id, &conversation_id);
    session(&state, replaced)
}

async fn wrap_up(
    State(state): State<AppState>,
    Path((session_id, conversation_id)): Path<(String, String)>,
) -> Response {
    let wrapped = state.teach.wrap_up(&session_id, &conversation_id);
    session(&state, wrapped)
}

/// What was dictated in Kass's window for the conversation's current turn.
async fn dictated_now(
    state: &AppState,
    session_id: &str,
    conversation_id: &str,
) -> Result<Option<Dictated>, Box<Response>> {
    let (since, used): (_, HashSet<String>) = state
        .teach
        .turn_window(session_id, conversation_id)
        .map_err(|refusal| Box::new(refused(refusal)))?;
    state.teach.tell_worker(&state.proxy);
    state
        .db
        .call(move |conn| teach::dictated(conn, since, &used))
        .await
        .map_err(|error| Box::new(failed(error)))
}

/// The cleanup of what was dictated in Kass's window this turn, for the
/// reply box.
async fn dictated(
    State(state): State<AppState>,
    Path((session_id, conversation_id)): Path<(String, String)>,
) -> Response {
    match dictated_now(&state, &session_id, &conversation_id).await {
        Ok(found) => Json(json!({ "text": found.map(|found| found.shown) })).into_response(),
        Err(response) => *response,
    }
}

/// The user cleared the reply box or opened this conversation: earlier
/// dictation is no longer part of its reply.
async fn restart_turn(
    State(state): State<AppState>,
    Path((session_id, conversation_id)): Path<(String, String)>,
) -> Response {
    let restarted = state.teach.start_turn_now(&session_id, &conversation_id);
    state.teach.tell_worker(&state.proxy);
    match restarted {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => refused(refusal),
    }
}

async fn send_reply(
    State(state): State<AppState>,
    Path((session_id, conversation_id)): Path<(String, String)>,
    bytes: Bytes,
) -> Response {
    let written = match body(&bytes).and_then(|fields| {
        let mut problems = Problems::default();
        let written = required(&fields, "written", teach::MAX_REPLY_CHARS, &mut problems);
        problems.into_result(written)
    }) {
        Ok(Some(written)) => written,
        Ok(None) => unreachable!("a missing reply is a problem"),
        Err(problems) => return problems.response(),
    };
    if let Err(refusal) = state.teach.check(&session_id, Some(&conversation_id)) {
        return refused(refusal);
    }
    let spoken = match dictated_now(&state, &session_id, &conversation_id).await {
        Ok(spoken) => spoken,
        Err(response) => return *response,
    };
    if let Err(refusal) = state
        .teach
        .record_reply(&session_id, &conversation_id, &written, spoken)
    {
        return refused(refusal);
    }
    let turn = match state.teach.next_turn(&session_id, &conversation_id) {
        Ok(turn) => turn,
        Err(refusal) => return refused(refusal),
    };
    let model = match state.db.call(settings::get).await {
        Ok(saved) => saved.llm_model,
        Err(error) => return failed(error),
    };
    let parsed = other_side(&state, &turn, &model).await;
    let answer = state
        .teach
        .take_turn(&session_id, &conversation_id, turn.trick, parsed);
    session(&state, answer)
}

/// The other side's next message from the base model; a repeat of an
/// earlier one gets one more try. None when it can't write a usable one.
async fn other_side(
    state: &AppState,
    turn: &teach::Turn,
    model: &str,
) -> Option<(String, text::Note)> {
    for _ in 0..2 {
        let request = Generate {
            id: uuid::Uuid::new_v4().to_string(),
            prompt: turn.prompt.clone(),
            hint: None,
            max_tokens: 320,
            temperature: 0.7,
            cleanup: CleanupPrompt {
                model_size: model.to_string(),
                flags: None,
                system: turn.system.clone(),
                examples: Vec::new(),
                cache_key: Some(teach::PROMPT_CACHE_KEY.to_string()),
            },
        };
        let output = match models::generate(&state.proxy, &request, |_| {}).await {
            Ok(generated) => generated.text,
            Err(error) => {
                eprintln!("[teach] couldn't write the next turn; using a written one: {error}");
                return None;
            }
        };
        let parsed = text::parse_turn(&output)?;
        if !text::repeats_earlier(&turn.messages, &parsed.0) {
            return Some(parsed);
        }
        eprintln!("[teach] the next turn repeated an earlier message; asking again");
    }
    None
}

/// End the session: keep its replies in the style's profile, and show one
/// of the user's dictations cleaned up before and after.
async fn finish(State(state): State<AppState>, Path(session_id): Path<String>) -> Response {
    let finished = state.teach.finish(&session_id);
    state.teach.tell_worker(&state.proxy);
    let finished = match finished {
        Ok(finished) => finished,
        Err(refusal) => return refused(refusal),
    };
    let replies = finished.examples.len();
    let spoken: Vec<String> = finished
        .examples
        .iter()
        .filter_map(|example| example["said"].as_str().map(str::to_string))
        .filter(|said| !said.is_empty())
        .collect();
    let style = finished.style.clone();
    let examples = finished.examples;
    let saved = learned::change(&state, None, move |_, files| {
        files.save_run(&style, examples);
        Ok(learned::status(&files.writing, &style))
    })
    .await;
    let status = match saved {
        Ok(status) => status,
        Err(error) => return failed(error),
    };
    // The longest dictated reply; the first of equals, as Python's `max`.
    let fallback = spoken
        .iter()
        .fold(None::<&String>, |longest, said| match longest {
            Some(longest) if longest.chars().count() >= said.chars().count() => Some(longest),
            _ => Some(said),
        })
        .cloned();
    let (before, after) = match preview::preview(&state, &finished.style, fallback).await {
        Ok(shown) => shown,
        Err(error) => return failed(error),
    };
    Json(json!({
        "status": status,
        "before": before,
        "after": after,
        "replies": replies,
        "dictated": spoken.len(),
    }))
    .into_response()
}
