//! The dictionary of words and names (docs/plans/DICTIONARIES.md), answered
//! from `kass.db` as `backend/routes/dictionary.py` answered it. After each
//! change the worker is told to read the entries again for dictation, and
//! every open app hears `learned` so it shows the new entries.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use chrono::{NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Map, Value};

use super::request::{body, detail, flag, text, Problems};
use crate::dictionary::store::{self, GroupPatch, NewGroup, PlaceInput};
use crate::dictionary::{self as words, EditError, Group, Place, PROMPT_TOKENS};
use crate::events::Kind;
use crate::{worker, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dictionary", get(list_entries).post(add_entry))
        .route("/dictionary/resolved", get(resolved))
        .route(
            "/dictionary/{entry_id}",
            patch(update_entry).delete(delete_entry),
        )
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

// -- answers ------------------------------------------------------------------

/// A datetime as pydantic writes one: ISO 8601, microseconds only when set.
fn iso<S: Serializer>(at: &Option<NaiveDateTime>, serializer: S) -> Result<S::Ok, S::Error> {
    match at {
        None => serializer.serialize_none(),
        Some(at) if at.nanosecond() / 1000 == 0 => {
            serializer.serialize_str(&at.format("%Y-%m-%dT%H:%M:%S").to_string())
        }
        Some(at) => serializer.serialize_str(&at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
    }
}

#[derive(Serialize)]
struct EntryModel {
    id: String,
    written: String,
    spoken: Option<String>,
    places: Vec<Place>,
    #[serde(serialize_with = "iso")]
    created_at: Option<NaiveDateTime>,
    match_sound: bool,
    source: String,
    phrase: bool,
}

impl From<Group> for EntryModel {
    fn from(group: Group) -> Self {
        EntryModel {
            id: group.id,
            written: group.written,
            spoken: group.spoken,
            places: group.places,
            created_at: group.created_at,
            match_sound: group.match_sound,
            source: group.source,
            phrase: group.phrase,
        }
    }
}

#[derive(Serialize)]
struct ResolvedEntry {
    id: String,
    scope: String,
    scope_id: Option<String>,
    app_name: Option<String>,
    written: String,
    spoken: Option<String>,
    #[serde(serialize_with = "iso")]
    created_at: Option<NaiveDateTime>,
    match_sound: bool,
    phrase: bool,
    overridden: bool,
}

fn not_found() -> Response {
    detail(StatusCode::NOT_FOUND, "Dictionary entry not found")
}

fn refused(error: EditError) -> Response {
    let status = match error {
        EditError::Duplicate(_) => StatusCode::CONFLICT,
        EditError::Invalid(_) => StatusCode::BAD_REQUEST,
    };
    detail(status, error.message())
}

fn failed(error: crate::db::DbError) -> Response {
    eprintln!("[server] dictionary: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

// -- requests, checked the way pydantic checks them ---------------------------

fn place(value: &Value, at: usize, problems: &mut Problems) -> Option<PlaceInput> {
    let loc = |field: &str| [json!("places"), json!(at), json!(field)];
    let Value::Object(fields) = value else {
        let msg = "Input should be a valid dictionary or instance of DictionaryPlace";
        problems.add("model_type", &[json!("places"), json!(at)], msg, value);
        return None;
    };
    let scope = match fields.get("scope") {
        None => {
            problems.add("missing", &loc("scope"), "Field required", value);
            None
        }
        Some(Value::String(scope)) if ["global", "style", "app"].contains(&scope.as_str()) => {
            Some(scope.clone())
        }
        Some(other) => {
            let msg = "Input should be 'global', 'style' or 'app'";
            problems.add("literal_error", &loc("scope"), msg, other);
            None
        }
    };
    let mut optional = |name: &str| -> Option<String> {
        match fields.get(name) {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.chars().count() > 255 => {
                let msg = "String should have at most 255 characters";
                problems.add("string_too_long", &loc(name), msg, &json!(text));
                None
            }
            Some(Value::String(text)) => Some(text.clone()),
            Some(other) => {
                problems.add(
                    "string_type",
                    &loc(name),
                    "Input should be a valid string",
                    other,
                );
                None
            }
        }
    };
    let scope_id = optional("scope_id");
    let app_name = optional("app_name");
    Some(PlaceInput {
        scope: scope?,
        scope_id,
        app_name,
    })
}

/// `places`, absent (`None`), null (`Some(None)`) or 1 to 100 of them.
fn places(fields: &Map<String, Value>, problems: &mut Problems) -> Option<Option<Vec<PlaceInput>>> {
    let value = fields.get("places")?;
    let list = match value {
        Value::Null => return Some(None),
        Value::Array(list) => list,
        other => {
            problems.add(
                "list_type",
                &[json!("places")],
                "Input should be a valid list",
                other,
            );
            return None;
        }
    };
    if list.is_empty() {
        let msg = "List should have at least 1 item after validation, not 0";
        problems.add("too_short", &[json!("places")], msg, value);
        return None;
    }
    if list.len() > 100 {
        let msg = format!(
            "List should have at most 100 items after validation, not {}",
            list.len()
        );
        problems.add("too_long", &[json!("places")], msg, value);
        return None;
    }
    let checked: Vec<Option<PlaceInput>> = list
        .iter()
        .enumerate()
        .map(|(at, value)| place(value, at, problems))
        .collect();
    Some(checked.into_iter().collect::<Option<Vec<_>>>())
}

fn new_group(fields: &Map<String, Value>) -> Result<NewGroup, Problems> {
    let mut problems = Problems::default();
    let written = text(fields, "written", 2000, &mut problems);
    let spoken = text(fields, "spoken", 1000, &mut problems);
    let places = places(fields, &mut problems);
    let match_sound = flag(fields, "match_sound", &mut problems);
    let phrase = flag(fields, "phrase", &mut problems);
    let whole = Value::Object(fields.clone());
    let written = match written {
        Some(Some(written)) => Some(written),
        Some(None) => {
            problems.add(
                "string_type",
                &[json!("written")],
                "Input should be a valid string",
                &Value::Null,
            );
            None
        }
        None if !fields.contains_key("written") => {
            problems.add("missing", &[json!("written")], "Field required", &whole);
            None
        }
        None => None,
    };
    let places = match places {
        Some(Some(places)) => Some(places),
        Some(None) => {
            problems.add(
                "list_type",
                &[json!("places")],
                "Input should be a valid list",
                &Value::Null,
            );
            None
        }
        None if !fields.contains_key("places") => {
            problems.add("missing", &[json!("places")], "Field required", &whole);
            None
        }
        None => None,
    };
    let invalid_flag = |name: &str, problems: &mut Problems| {
        let msg = "Input should be a valid boolean";
        problems.add("bool_type", &[json!(name)], msg, &Value::Null);
    };
    if matches!(match_sound, Some(None)) {
        invalid_flag("match_sound", &mut problems);
    }
    if matches!(phrase, Some(None)) {
        invalid_flag("phrase", &mut problems);
    }
    match (written, places) {
        (Some(written), Some(places)) if problems.is_empty() => {
            let mut new = NewGroup::new(written, spoken.flatten(), places);
            new.match_sound = match_sound.flatten().unwrap_or(true);
            new.phrase = phrase.flatten().unwrap_or(false);
            Ok(new)
        }
        _ => Err(problems),
    }
}

fn group_patch(fields: &Map<String, Value>) -> Result<GroupPatch, Problems> {
    let mut problems = Problems::default();
    let patch = GroupPatch {
        written: text(fields, "written", 2000, &mut problems).flatten(),
        spoken: text(fields, "spoken", 1000, &mut problems),
        places: places(fields, &mut problems).flatten(),
        match_sound: flag(fields, "match_sound", &mut problems).flatten(),
    };
    problems.into_result(patch)
}

// -- handlers -----------------------------------------------------------------

async fn list_entries(State(state): State<AppState>) -> Response {
    match state.db.call(|conn| store::list_groups(conn)).await {
        Ok(groups) => {
            let entries: Vec<EntryModel> = groups.into_iter().map(EntryModel::from).collect();
            Json(json!({ "entries": entries })).into_response()
        }
        Err(error) => failed(error),
    }
}

async fn add_entry(State(state): State<AppState>, bytes: Bytes) -> Response {
    let new = match body(&bytes).and_then(|fields| new_group(&fields)) {
        Ok(new) => new,
        Err(problems) => return problems.response(),
    };
    let sync = state.sync;
    match state
        .db
        .call(move |conn| store::add_group(conn, new, sync))
        .await
    {
        Ok(Ok(group)) => {
            state.events.publish(Kind::Learned);
            Json(EntryModel::from(group)).into_response()
        }
        Ok(Err(error)) => refused(error),
        Err(error) => failed(error),
    }
}

#[derive(Deserialize)]
struct ResolvedQuery {
    bundle_id: Option<String>,
}

async fn resolved(State(state): State<AppState>, Query(query): Query<ResolvedQuery>) -> Response {
    let bundle_id = query.bundle_id.filter(|id| !id.is_empty());
    let read = state
        .db
        .call(move |conn| {
            let style_id = store::style_for_app(conn, bundle_id.as_deref())?;
            let entries = store::list_entries(conn)?;
            Ok(words::resolve(
                &entries,
                bundle_id.as_deref(),
                Some(&style_id),
            ))
        })
        .await;
    let resolved = match read {
        Ok(resolved) => resolved,
        Err(error) => return failed(error),
    };
    let terms = words::terms(&resolved);
    let counts = worker::token_counts(&state.proxy, &terms).await;
    let counted: HashMap<&str, usize> = match &counts {
        Some(counts) => terms
            .iter()
            .map(String::as_str)
            .zip(counts.iter().copied())
            .collect(),
        None => HashMap::new(),
    };
    let count = |term: &str| {
        counted
            .get(term)
            .copied()
            .unwrap_or_else(|| words::estimated_tokens(term))
    };
    let (fit, dropped) = words::fit_terms(&terms, count, PROMPT_TOKENS);
    let entries: Vec<ResolvedEntry> = resolved
        .into_iter()
        .map(|(entry, overridden)| ResolvedEntry {
            id: entry.group_id,
            scope: entry.scope,
            scope_id: entry.scope_id,
            app_name: entry.app_name,
            written: entry.written,
            spoken: entry.spoken,
            created_at: entry.created_at,
            match_sound: entry.match_sound,
            phrase: entry.phrase,
            overridden,
        })
        .collect();
    Json(json!({ "entries": entries, "prompt_terms": fit, "dropped_terms": dropped }))
        .into_response()
}

async fn update_entry(
    State(state): State<AppState>,
    Path(entry_id): Path<String>,
    bytes: Bytes,
) -> Response {
    let patch = match body(&bytes).and_then(|fields| group_patch(&fields)) {
        Ok(patch) => patch,
        Err(problems) => return problems.response(),
    };
    let sync = state.sync;
    let updated = state
        .db
        .call(move |conn| store::update_group(conn, &entry_id, patch, sync))
        .await;
    match updated {
        Ok(Ok(Some(group))) => {
            state.events.publish(Kind::Learned);
            Json(EntryModel::from(group)).into_response()
        }
        Ok(Ok(None)) => not_found(),
        Ok(Err(error)) => refused(error),
        Err(error) => failed(error),
    }
}

async fn delete_entry(State(state): State<AppState>, Path(entry_id): Path<String>) -> Response {
    let sync = state.sync;
    match state
        .db
        .call(move |conn| store::delete_group(conn, &entry_id, sync))
        .await
    {
        Ok(true) => {
            state.events.publish(Kind::Learned);
            Json(json!({ "deleted": true })).into_response()
        }
        Ok(false) => not_found(),
        Err(error) => failed(error),
    }
}
