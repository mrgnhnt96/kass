//! The learned writing style: examples, correction notes and teaching
//! sessions. Python: `backend/routes/writing_style.py`.
//!
//! What each style learned is answered and changed here (`learned`); the
//! teach sessions are in `routes::teach`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::request::detail;
use crate::cleanup_inputs::{self, PersonalExample};
use crate::learned::{self, Files};
use crate::styles::{self, Snapshot};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/writing-style", get(status).delete(reset))
        .route("/writing-style/examples", get(examples))
        .route("/writing-style/examples/{example_id}", delete(hide_example))
        .route("/writing-style/notes", get(notes))
        .route("/writing-style/notes/{note_id}", delete(remove_note))
}

/// `?style=`: a style id, or the default style when left out or empty.
#[derive(Deserialize)]
pub(super) struct StyleQuery {
    pub(super) style: Option<String>,
}

pub(super) const STYLE_NOT_FOUND: &str = "Style not found";

/// A known style's id, as Python's `_style`.
fn known(styles: &Snapshot, style: Option<&str>) -> Option<String> {
    match style.filter(|style| !style.is_empty()) {
        Some(style) => styles.get(Some(style)).map(|found| found.id.clone()),
        None => Some(styles.default_style().id.clone()),
    }
}

pub(super) fn failed(error: impl std::fmt::Display) -> Response {
    eprintln!("[server] learned writing style: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

/// The style `query` names, read from `kass.db`; `None` when it doesn't exist.
pub(super) async fn resolve(state: &AppState, query: StyleQuery) -> Result<Option<String>, String> {
    state
        .db
        .call(move |conn| Ok(known(&styles::load(conn, None)?, query.style.as_deref())))
        .await
        .map_err(|error| error.to_string())
}

async fn status(State(state): State<AppState>, Query(query): Query<StyleQuery>) -> Response {
    let style = match resolve(&state, query).await {
        Ok(Some(style)) => style,
        Ok(None) => return detail(StatusCode::NOT_FOUND, STYLE_NOT_FOUND),
        Err(error) => return failed(error),
    };
    let dir = learned::data_dir(&state);
    let writing = cleanup_inputs::read_writing_style(&dir);
    Json(learned::status(&writing, &style)).into_response()
}

/// Forget the style's taught replies, habits and rules; other styles keep
/// theirs.
async fn reset(State(state): State<AppState>, Query(query): Query<StyleQuery>) -> Response {
    let style = match resolve(&state, query).await {
        Ok(Some(style)) => style,
        Ok(None) => return detail(StatusCode::NOT_FOUND, STYLE_NOT_FOUND),
        Err(error) => return failed(error),
    };
    let resetting = style.clone();
    let reset = learned::change(&state, Some(&style), move |_, files| {
        files.reset(&resetting);
        files.forget_notes(&resetting);
        Ok(learned::status(&files.writing, &resetting))
    })
    .await;
    match reset {
        Ok(status) => Json(status).into_response(),
        Err(error) => failed(error),
    }
}

/// An example as Python's `PersonalExample` model answers it: a calibration
/// example's app is null.
fn example_json(example: &PersonalExample) -> Value {
    json!({
        "id": example.id,
        "source": example.source,
        "said": example.said,
        "meant": example.meant,
        "created_at": example.created_at,
        "app_bundle_id": example.app_bundle_id,
        "app_name": example.app_name,
    })
}

/// The style's examples, and the styles they were grouped with: Kass's own
/// window teaches the style being taught.
async fn style_examples(
    state: &AppState,
    query: StyleQuery,
) -> Result<Option<(String, Vec<PersonalExample>)>, String> {
    let teaching = state.teach.teaching_style();
    let dir = learned::data_dir(state);
    state
        .db
        .call(move |conn| {
            let styles = styles::load(conn, teaching)?;
            let Some(style) = known(&styles, query.style.as_deref()) else {
                return Ok(None);
            };
            let writing = cleanup_inputs::read_writing_style(&dir);
            let all = cleanup_inputs::all_examples(conn, &writing, &styles, &style)?;
            Ok(Some((style, all)))
        })
        .await
        .map_err(|error| error.to_string())
}

async fn examples(State(state): State<AppState>, Query(query): Query<StyleQuery>) -> Response {
    match style_examples(&state, query).await {
        Ok(Some((_, all))) => {
            Json(all.iter().map(example_json).collect::<Vec<_>>()).into_response()
        }
        Ok(None) => detail(StatusCode::NOT_FOUND, STYLE_NOT_FOUND),
        Err(error) => failed(error),
    }
}

/// Stop using an example; corrections stay recorded for the personal model.
async fn hide_example(State(state): State<AppState>, Path(example_id): Path<String>) -> Response {
    let teaching = state.teach.teaching_style();
    let hidden = learned::change(&state, None, move |conn, files: &mut Files| {
        let styles = styles::load(conn, teaching)?;
        let known = learned::known_example(conn, &files.writing, &styles, &example_id)?;
        if known {
            files.hide(&example_id);
        }
        Ok(known)
    })
    .await;
    match hidden {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => detail(StatusCode::NOT_FOUND, "Example not found"),
        Err(error) => failed(error),
    }
}

async fn notes(State(state): State<AppState>, Query(query): Query<StyleQuery>) -> Response {
    match style_examples(&state, query).await {
        Ok(Some((style, all))) => {
            let notes = learned::read_notes(&learned::data_dir(&state));
            Json(learned::notes_status(&notes, &style, &all)).into_response()
        }
        Ok(None) => detail(StatusCode::NOT_FOUND, STYLE_NOT_FOUND),
        Err(error) => failed(error),
    }
}

async fn remove_note(
    State(state): State<AppState>,
    Path(note_id): Path<String>,
    Query(query): Query<StyleQuery>,
) -> Response {
    let style = match resolve(&state, query).await {
        Ok(Some(style)) => style,
        Ok(None) => return detail(StatusCode::NOT_FOUND, STYLE_NOT_FOUND),
        Err(error) => return failed(error),
    };
    let removed = learned::change(&state, None, move |_, files| {
        Ok(files.remove_note(&style, &note_id))
    })
    .await;
    match removed {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => detail(StatusCode::NOT_FOUND, "Rule not found"),
        Err(error) => failed(error),
    }
}
