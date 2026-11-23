//! Writing styles and the style each app uses (docs/plans/PER_APP_STYLE.md),
//! answered from `kass.db` as `backend/routes/styles.py` answered them.
//! After each change what each style learned is regrouped (`learned`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use kass_text::py;
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::request::{body, detail, flag, required, text, Problems};
use crate::cleanup_inputs::{CorrectionRows, LearnedFiles};
use crate::dictionary::store as dictionary;
use crate::stream::session::cleanup_prompt;
use crate::styles::{self, Corrections, Refused, Style, StylePatch, StyledApp, MAX_STYLES};
use crate::{cleanup_inputs, learned, settings, worker, AppState};

/// Longest app bundle id or name stored with a capture.
const MAX_APP_FIELD_CHARS: usize = 255;
/// Most new apps one confirm takes.
const MAX_CONFIRM: usize = 500;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/writing-styles", get(list_styles).post(create_style))
        .route(
            "/writing-styles/{style_id}",
            patch(update_style).delete(delete_style),
        )
        .route("/writing-styles/apps/confirm", post(confirm_apps))
        .route("/writing-styles/apps/{bundle_id}", put(assign_app))
        .layer(axum::middleware::from_fn(crate::cors::layer))
}

// -- answers ------------------------------------------------------------------

#[derive(Serialize)]
struct StylesResponse {
    styles: Vec<Style>,
    apps: Vec<StyledApp>,
    max_styles: i64,
    cache_mb_per_style: Option<i64>,
}

fn not_found() -> Response {
    detail(StatusCode::NOT_FOUND, "Style not found")
}

fn refused(refusal: Refused) -> Response {
    match refusal {
        Refused::Invalid(message) => detail(StatusCode::BAD_REQUEST, &message),
        Refused::NotFound => not_found(),
    }
}

fn failed(error: crate::db::DbError) -> Response {
    eprintln!("[server] writing styles: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

/// Each style's cleanup prompt, as `(system, examples)`.
type Prompts = Arc<Vec<(String, Vec<(String, String)>)>>;

/// The styles' cleanup prompts as last built, and the inputs they were built
/// from ([`fingerprint`]), so listing the styles again while nothing changed
/// doesn't build them again. The worker still counts them each time: what
/// it answers depends on whether the cleanup model is downloaded and loaded.
#[derive(Default)]
pub struct StylePrompts(Mutex<Option<(u64, Prompts)>>);

impl StylePrompts {
    fn get(&self, inputs: u64) -> Option<Prompts> {
        let kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.as_ref()
            .filter(|(built_from, _)| *built_from == inputs)
            .map(|(_, prompts)| Arc::clone(prompts))
    }

    fn keep(&self, inputs: u64, prompts: &Prompts) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some((inputs, Arc::clone(prompts)));
    }
}

/// What the styles' cleanup prompts are built from, read from the database.
struct PromptReads {
    settings: settings::Settings,
    styles: styles::Snapshot,
    corrections: CorrectionRows,
}

fn prompt_reads(
    conn: &mut rusqlite::Connection,
    teaching: Option<String>,
) -> rusqlite::Result<PromptReads> {
    Ok(PromptReads {
        settings: settings::get(conn)?,
        styles: styles::load(conn, teaching)?,
        corrections: CorrectionRows::read(conn)?,
    })
}

/// Everything the styles' cleanup prompts are built from, hashed: a style,
/// a correction, a learning file or the model changing changes it.
fn fingerprint(reads: &PromptReads, files: &LearnedFiles) -> u64 {
    let mut hasher = DefaultHasher::new();
    reads.settings.llm_model.hash(&mut hasher);
    reads.settings.self_correction.hash(&mut hasher);
    reads.styles.styles.hash(&mut hasher);
    reads.styles.apps.hash(&mut hasher);
    reads.styles.teaching.hash(&mut hasher);
    reads.corrections.hash(&mut hasher);
    for file in [
        Some(&files.writing),
        files.notes.as_ref(),
        files.learning.as_ref(),
    ] {
        file.map(Value::to_string).hash(&mut hasher);
    }
    hasher.finish()
}

/// What one style's cached prompt takes in the cleanup model, in MB: the
/// largest current style's, since a style's prompt grows as it learns. None
/// until the worker can count it. The database is only read; the prompts are
/// built after, and not again while nothing they are built from changed.
async fn cache_mb_per_style(state: &AppState, reads: rusqlite::Result<PromptReads>) -> Option<i64> {
    let reads = match reads {
        Ok(reads) => reads,
        Err(error) => {
            eprintln!("[server] couldn't read the styles' cleanup prompts: {error}");
            return None;
        }
    };
    let dir = learned::data_dir(state);
    let model = reads.settings.llm_model.clone();
    let built = Arc::clone(&state.style_prompts);
    let prompts = tokio::task::spawn_blocking(move || {
        let files = LearnedFiles::read(&dir);
        let inputs = fingerprint(&reads, &files);
        if let Some(prompts) = built.get(inputs) {
            return prompts;
        }
        let corrections = reads.corrections.examples(&reads.styles);
        let prompts: Prompts = Arc::new(
            reads
                .styles
                .styles
                .iter()
                .map(|style| {
                    let inputs = cleanup_inputs::assemble(
                        &files,
                        &reads.styles,
                        Some(&style.id),
                        reads.settings.self_correction,
                        &corrections,
                    );
                    let prompt = cleanup_prompt(&inputs, &reads.settings.llm_model);
                    (prompt.system, prompt.examples)
                })
                .collect(),
        );
        built.keep(inputs, &prompts);
        prompts
    })
    .await;
    let prompts = match prompts {
        Ok(prompts) => prompts,
        Err(error) => {
            eprintln!("[server] couldn't build the styles' cleanup prompts: {error}");
            return None;
        }
    };
    let largest = worker::cache_bytes(&state.proxy, &model, &prompts)
        .await?
        .into_iter()
        .flatten()
        .filter(|bytes| *bytes > 0)
        .max()?;
    Some((largest as f64 / 1e6).round_ties_even() as i64)
}

/// Every style and every app with the style it uses, as the page shows them.
/// One read of the database answers both the list and what the prompts are
/// built from.
async fn listing(state: &AppState) -> Response {
    let teaching = state.teach.teaching_style();
    let read = state
        .db
        .call(move |conn| {
            let listed = styles::listing(conn, teaching.clone())?;
            Ok((listed, prompt_reads(conn, teaching)))
        })
        .await;
    match read {
        Ok((listed, reads)) => Json(StylesResponse {
            styles: listed.styles,
            apps: listed.apps,
            max_styles: MAX_STYLES,
            cache_mb_per_style: cache_mb_per_style(state, reads).await,
        })
        .into_response(),
        Err(error) => failed(error),
    }
}

/// A capture's target app field as stored: trimmed, at most 255 characters,
/// None when blank (`services/captures.py`, `_clean_app_field`).
fn app_field(value: Option<&str>) -> Option<String> {
    let cleaned = py::head(py::strip(value?), MAX_APP_FIELD_CHARS);
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

// -- requests, checked the way pydantic checks them ---------------------------

fn new_style(fields: &Map<String, Value>) -> Result<String, Problems> {
    let mut problems = Problems::default();
    let name = required(fields, "name", 80, &mut problems);
    problems.into_result(name.unwrap_or_default())
}

fn style_patch(fields: &Map<String, Value>) -> Result<StylePatch, Problems> {
    let mut problems = Problems::default();
    let name = text(fields, "name", 80, &mut problems).flatten();
    let punctuation_style = match text(fields, "punctuation_style", usize::MAX, &mut problems) {
        Some(Some(chosen)) if !styles::PUNCTUATION_STYLES.contains(&chosen.as_str()) => {
            let pattern = "^(standard|casual|learned)$";
            problems.add_ctx(
                "string_pattern_mismatch",
                &[json!("punctuation_style")],
                format!("String should match pattern '{pattern}'"),
                &json!(chosen),
                json!({ "pattern": pattern }),
            );
            None
        }
        chosen => chosen.flatten(),
    };
    let patch = StylePatch {
        name,
        punctuation_style,
        preserve_technical: flag(fields, "preserve_technical", &mut problems).flatten(),
        description: text(fields, "description", 2000, &mut problems).flatten(),
        is_default: flag(fields, "is_default", &mut problems).flatten(),
    };
    problems.into_result(patch)
}

struct Assignment {
    style_id: String,
    app_name: Option<String>,
    corrections: Corrections,
}

fn assignment(fields: &Map<String, Value>) -> Result<Assignment, Problems> {
    let mut problems = Problems::default();
    let style_id = required(fields, "style_id", usize::MAX, &mut problems);
    let app_name = text(fields, "app_name", MAX_APP_FIELD_CHARS, &mut problems).flatten();
    let corrections = match fields.get("corrections") {
        None => Some(Corrections::Bring),
        Some(Value::String(chosen)) if chosen == "bring" => Some(Corrections::Bring),
        Some(Value::String(chosen)) if chosen == "leave" => Some(Corrections::Leave),
        Some(other) => {
            problems.add_ctx(
                "literal_error",
                &[json!("corrections")],
                "Input should be 'bring' or 'leave'",
                other,
                json!({ "expected": "'bring' or 'leave'" }),
            );
            None
        }
    };
    match (style_id, corrections) {
        (Some(style_id), Some(corrections)) if problems.is_empty() => Ok(Assignment {
            style_id,
            app_name,
            corrections,
        }),
        _ => Err(problems),
    }
}

/// The apps to confirm, as (bundle id, name) pairs, before cleaning.
fn apps_to_confirm(fields: &Map<String, Value>) -> Result<Vec<(String, Option<String>)>, Problems> {
    let mut problems = Problems::default();
    let list = match fields.get("apps") {
        None => {
            let whole = Value::Object(fields.clone());
            problems.add("missing", &[json!("apps")], "Field required", &whole);
            return Err(problems);
        }
        Some(Value::Array(list)) => list,
        Some(other) => {
            problems.add(
                "list_type",
                &[json!("apps")],
                "Input should be a valid list",
                other,
            );
            return Err(problems);
        }
    };
    if list.len() > MAX_CONFIRM {
        problems.add_ctx(
            "too_long",
            &[json!("apps")],
            format!(
                "List should have at most {MAX_CONFIRM} items after validation, not {}",
                list.len()
            ),
            &Value::Array(list.clone()),
            json!({ "field_type": "List", "max_length": MAX_CONFIRM, "actual_length": list.len() }),
        );
        return Err(problems);
    }
    let mut apps = Vec::new();
    for (at, value) in list.iter().enumerate() {
        let Value::Object(app) = value else {
            let msg = "Input should be a valid dictionary or instance of AppToConfirm";
            problems.add_ctx(
                "model_type",
                &[json!("apps"), json!(at)],
                msg,
                value,
                json!({ "class_name": "AppToConfirm" }),
            );
            continue;
        };
        let mut inner = Problems::default();
        let bundle_id = required(app, "bundle_id", MAX_APP_FIELD_CHARS, &mut inner);
        if let Some(bundle) = &bundle_id {
            if bundle.is_empty() {
                inner.add_ctx(
                    "string_too_short",
                    &[json!("bundle_id")],
                    "String should have at least 1 character",
                    &json!(bundle),
                    json!({ "min_length": 1 }),
                );
            }
        }
        let app_name = text(app, "app_name", MAX_APP_FIELD_CHARS, &mut inner).flatten();
        for mut problem in inner.0 {
            // Each problem's place, under this item of the list.
            if let Some(Value::Array(loc)) = problem.get_mut("loc") {
                loc.splice(1..1, [json!("apps"), json!(at)]);
            }
            problems.0.push(problem);
        }
        if let Some(bundle) = bundle_id {
            apps.push((bundle, app_name));
        }
    }
    problems.into_result(apps)
}

// -- handlers -----------------------------------------------------------------

async fn list_styles(State(state): State<AppState>) -> Response {
    listing(&state).await
}

async fn create_style(State(state): State<AppState>, bytes: Bytes) -> Response {
    let name = match body(&bytes).and_then(|fields| new_style(&fields)) {
        Ok(name) => name,
        Err(problems) => return problems.response(),
    };
    let sync = state.sync;
    match state
        .db
        .call(move |conn| styles::create_style(conn, &name, sync))
        .await
    {
        Ok(Ok(style)) => Json(style).into_response(),
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => failed(error),
    }
}

async fn update_style(
    State(state): State<AppState>,
    Path(style_id): Path<String>,
    bytes: Bytes,
) -> Response {
    let patch = match body(&bytes).and_then(|fields| style_patch(&fields)) {
        Ok(patch) => patch,
        Err(problems) => return problems.response(),
    };
    let sync = state.sync;
    match state
        .db
        .call(move |conn| styles::update_style(conn, &style_id, patch, sync))
        .await
    {
        Ok(Ok(style)) => Json(style).into_response(),
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => failed(error),
    }
}

async fn delete_style(State(state): State<AppState>, Path(style_id): Path<String>) -> Response {
    let sync = state.sync;
    let deleting = style_id.clone();
    let deleted = state
        .db
        .call(move |conn| {
            let default = match styles::delete_style(conn, &deleting, sync)? {
                Ok(default) => default,
                Err(refusal) => return Ok(Err(refusal)),
            };
            // Its dictionary entries join the default's.
            dictionary::move_style(conn, &deleting, &default, sync)?;
            Ok(Ok(()))
        })
        .await;
    match deleted {
        Ok(Ok(())) => {
            learned::forget_style(&state, &style_id).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => failed(error),
    }
}

async fn confirm_apps(State(state): State<AppState>, bytes: Bytes) -> Response {
    let apps = match body(&bytes).and_then(|fields| apps_to_confirm(&fields)) {
        Ok(apps) => apps,
        Err(problems) => return problems.response(),
    };
    let apps: Vec<(String, Option<String>)> = apps
        .into_iter()
        .filter_map(|(bundle, name)| Some((app_field(Some(&bundle))?, app_field(name.as_deref()))))
        .collect();
    let teaching = state.teach.teaching_style();
    let sync = state.sync;
    match state
        .db
        .call(move |conn| styles::confirm_apps(conn, &apps, teaching, sync))
        .await
    {
        Ok(_) => listing(&state).await,
        Err(error) => failed(error),
    }
}

async fn assign_app(
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
    bytes: Bytes,
) -> Response {
    let assignment = match body(&bytes).and_then(|fields| assignment(&fields)) {
        Ok(assignment) => assignment,
        Err(problems) => return problems.response(),
    };
    let Some(bundle) = app_field(Some(&bundle_id)) else {
        return detail(StatusCode::BAD_REQUEST, "Name an app");
    };
    let name = app_field(assignment.app_name.as_deref());
    let teaching = state.teach.teaching_style();
    let sync = state.sync;
    let assigned = state
        .db
        .call(move |conn| {
            styles::assign_app(
                conn,
                &bundle,
                name.as_deref(),
                &assignment.style_id,
                assignment.corrections,
                teaching,
                sync,
            )
        })
        .await;
    match assigned {
        Ok(Ok(())) => {
            learned::recount_habits(&state).await;
            listing(&state).await
        }
        Ok(Err(refusal)) => refused(refusal),
        Err(error) => failed(error),
    }
}
