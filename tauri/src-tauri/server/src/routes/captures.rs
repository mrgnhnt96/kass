//! Saved captures, their audio, feedback and events, and learning from them.
//! Python: `backend/routes/captures.py`.
//!
//! Captures, their audio, the apps they came from, the corrections reported
//! on them and the events stream are answered here from `kass.db`. Saving,
//! refining and retranscribing a capture and readiness run models or their
//! state, so they stay in the worker; learning is
//! `routes::learning`, and stats are `routes::usage`. A correction is
//! saved here and learned from there: the worker is told what changed
//! (`worker::reports_changed`).

use std::collections::{BTreeSet, HashMap};

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use chrono::NaiveDateTime;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::captures::store::{self, Apps, NewReport, Refused, Report};
use crate::captures::{
    self, int_text, iso, lax_bool, literal, optional_string, resolve_storage_path, string, Capture,
    Problems, Snapshot,
};
use crate::dictionary::store::delete_added_by;
use crate::events::{mac_of, query_pairs, Kind};
use crate::proxy::{forward, to_worker};
use crate::worker::{self, Learn};
use crate::AppState;

pub fn router() -> Router<AppState> {
    let cors = || axum::middleware::from_fn(crate::cors::layer);
    let answered_here = Router::new()
        .route("/captures/events", get(events))
        .route(
            "/captures/{capture_id}",
            get(get_capture).delete(delete_capture),
        )
        .route("/captures/{capture_id}/audio", get(audio))
        .route(
            "/captures/{capture_id}/feedback",
            get(list_reports).post(report),
        )
        .route(
            "/captures/{capture_id}/feedback/{report_id}",
            delete(withdraw),
        )
        .route("/capture/feedback/export", get(export))
        .route("/captures/apps", get(apps))
        .layer(cors());
    Router::new()
        .merge(answered_here)
        // Saving a capture runs the models: POST stays with the worker, as do
        // its CORS preflights.
        .route("/captures", get(list).layer(cors()).fallback(forward))
        .route("/capture/readiness", to_worker())
        .route("/captures/{capture_id}/refine", to_worker())
        .route("/captures/{capture_id}/retranscribe", to_worker())
}

// -- answers ------------------------------------------------------------------

fn detail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "detail": message }))).into_response()
}

fn capture_not_found() -> Response {
    detail(StatusCode::NOT_FOUND, "Capture not found")
}

fn failed(error: crate::db::DbError) -> Response {
    eprintln!("[server] captures: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

fn worker_down(error: String) -> Response {
    eprintln!("[server] the model worker didn't answer: {error}");
    detail(
        StatusCode::BAD_GATEWAY,
        "The Kass model worker isn't answering",
    )
}

fn invalid(problems: Problems) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "detail": problems.0 })),
    )
        .into_response()
}

/// A report as `CaptureFeedbackResponse` returns it.
#[derive(Serialize)]
struct ReportModel {
    id: String,
    capture_id: String,
    target: String,
    expected_text: String,
    notes: String,
    snapshot: Value,
    source: String,
    #[serde(serialize_with = "iso")]
    created_at: Option<NaiveDateTime>,
}

impl From<Report> for ReportModel {
    fn from(report: Report) -> Self {
        ReportModel {
            snapshot: captures::stored_snapshot(&report.snapshot),
            id: report.id,
            capture_id: report.capture_id,
            target: report.target,
            expected_text: report.expected_text,
            notes: report.notes,
            source: report.source,
            created_at: report.created_at,
        }
    }
}

fn reports(reports: Vec<Report>) -> Response {
    let reports: Vec<ReportModel> = reports.into_iter().map(ReportModel::from).collect();
    Json(reports).into_response()
}

// -- captures -----------------------------------------------------------------

/// The query a list asks with, checked as FastAPI checks it: the last value
/// of each parameter counts, blank ones too.
fn list_query(query: &str) -> Result<(i64, i64, Apps), Problems> {
    let params: HashMap<String, String> = query_pairs(query, true).into_iter().collect();
    let mut problems = Problems::default();
    let number = |name: &str, default: i64, problems: &mut Problems| match params.get(name) {
        None => Some(default),
        Some(text) => int_text(text, &[json!("query"), json!(name)], problems),
    };
    let limit = number("limit", 50, &mut problems);
    let offset = number("offset", 0, &mut problems);
    let unknown_app = match params.get("unknown_app") {
        None => Some(false),
        Some(word) => {
            let parsed = lax_bool(word);
            if parsed.is_none() {
                let msg = "Input should be a valid boolean, unable to interpret input";
                let loc = [json!("query"), json!("unknown_app")];
                problems.add("bool_parsing", &loc, msg, &json!(word));
            }
            parsed
        }
    };
    let (Some(limit), Some(offset), Some(unknown_app)) = (limit, offset, unknown_app) else {
        return Err(problems);
    };
    let apps = match params.get("app_bundle_id") {
        _ if unknown_app => Apps::Unknown,
        Some(bundle_id) if !bundle_id.is_empty() => Apps::One(bundle_id.clone()),
        _ => Apps::All,
    };
    Ok((limit, offset, apps))
}

/// The newest captures first. `app_bundle_id` keeps one app's;
/// `unknown_app` keeps those with no app recorded.
async fn list(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    let (limit, offset, apps) = match list_query(query.as_deref().unwrap_or("")) {
        Ok(query) => query,
        Err(problems) => return invalid(problems),
    };
    if !(1..=200).contains(&limit) {
        return detail(StatusCode::BAD_REQUEST, "limit must be between 1 and 200");
    }
    if offset < 0 {
        return detail(StatusCode::BAD_REQUEST, "offset must be >= 0");
    }
    match state
        .db
        .call(move |conn| store::list(conn, limit, offset, &apps))
        .await
    {
        Ok((items, total)) => Json(json!({ "items": items, "total": total })).into_response(),
        Err(error) => failed(error),
    }
}

/// How many captures each app has, most first, with the style each uses.
/// Kass's own window follows the style being taught, which the worker knows.
async fn apps(State(state): State<AppState>) -> Response {
    let teaching = worker::styles_state(&state.proxy).await.teaching_style;
    match state
        .db
        .call(move |conn| crate::styles::capture_apps(conn, teaching))
        .await
    {
        Ok(apps) => Json(apps).into_response(),
        Err(error) => failed(error),
    }
}

async fn find(state: &AppState, capture_id: String) -> Result<Option<Capture>, crate::db::DbError> {
    state
        .db
        .call(move |conn| store::get(conn, &capture_id))
        .await
}

async fn get_capture(State(state): State<AppState>, Path(capture_id): Path<String>) -> Response {
    match find(&state, capture_id).await {
        Ok(Some(capture)) => Json(capture).into_response(),
        Ok(None) => capture_not_found(),
        Err(error) => failed(error),
    }
}

/// Where a capture's audio is, beside `kass.db`.
fn audio_file(state: &AppState, capture: &Capture) -> Option<std::path::PathBuf> {
    let data_dir = state.db.path().parent()?;
    resolve_storage_path(data_dir, &capture.audio_path)
}

/// Python's `quote`, for the file name a download is saved as.
fn quoted(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' | b'/' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The original audio, as a download, with ranges for seeking.
async fn audio(
    State(state): State<AppState>,
    Path(capture_id): Path<String>,
    request: Request,
) -> Response {
    let capture = match find(&state, capture_id.clone()).await {
        Ok(Some(capture)) => capture,
        Ok(None) => return capture_not_found(),
        Err(error) => return failed(error),
    };
    let Some(path) = audio_file(&state, &capture).filter(|path| path.exists()) else {
        return detail(StatusCode::NOT_FOUND, "Audio file not found");
    };
    let served = match ServeFile::new(&path).oneshot(request).await {
        Ok(served) => served,
        Err(error) => {
            eprintln!(
                "[server] couldn't read capture audio {}: {error}",
                path.display()
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        }
    };
    let (mut parts, body) = served.into_parts();
    if parts.status.is_success() {
        let name = format!("capture_{capture_id}.wav");
        let disposition = if quoted(&name) == name {
            format!("attachment; filename=\"{name}\"")
        } else {
            format!("attachment; filename*=utf-8''{}", quoted(&name))
        };
        parts
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
        if let Ok(disposition) = HeaderValue::from_str(&disposition) {
            parts
                .headers
                .insert(header::CONTENT_DISPOSITION, disposition);
        }
    }
    Response::from_parts(parts, Body::new(body))
}

/// Delete a capture, its audio and its reports, with what they taught.
async fn delete_capture(State(state): State<AppState>, Path(capture_id): Path<String>) -> Response {
    let capture = match find(&state, capture_id.clone()).await {
        Ok(Some(capture)) => capture,
        Ok(None) => return capture_not_found(),
        Err(error) => return failed(error),
    };
    if let Some(path) = audio_file(&state, &capture).filter(|path| path.exists()) {
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!(
                "[server] couldn't remove capture audio {}: {error}",
                path.display()
            );
        }
    }
    let sync = state.sync;
    let id = capture_id.clone();
    let deleted = state
        .db
        .call(move |conn| {
            let Some(reports) = store::delete(conn, &id, sync)? else {
                return Ok(None);
            };
            // The words it spelled as a voice edit, and those its reports fixed.
            let mut unlearned = delete_added_by(conn, &id, sync)?;
            for (report_id, _, _) in &reports {
                unlearned |= delete_added_by(conn, report_id, sync)?;
            }
            Ok(Some((reports, unlearned)))
        })
        .await;
    let (forgotten, unlearned) = match deleted {
        Ok(Some(deleted)) => deleted,
        Ok(None) => return capture_not_found(),
        Err(error) => return failed(error),
    };
    state.events.publish(Kind::Changed);
    if !forgotten.is_empty() || unlearned {
        state.events.publish(Kind::Learned);
    }
    let changed: BTreeSet<(String, String)> = forgotten
        .into_iter()
        .map(|(_, target, source)| (target, source))
        .collect();
    let changed: Vec<_> = changed.into_iter().collect();
    worker::reports_changed(&state.proxy, &[], &changed, true, Some(&capture_id)).await;
    Json(json!({ "message": format!("Capture {capture_id} deleted") })).into_response()
}

async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    state.events.stream(mac_of(&headers, query.as_deref()))
}

// -- reports ------------------------------------------------------------------

/// A request body as FastAPI reads one: a JSON object, or a 422.
fn body(bytes: &Bytes) -> Result<Map<String, Value>, Problems> {
    let mut problems = Problems::default();
    if bytes.is_empty() {
        problems.add("missing", &[json!("body")], "Field required", &Value::Null);
        return Err(problems);
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(fields)) => Ok(fields),
        Ok(other) => {
            let msg = "Input should be a valid dictionary or object to extract fields from";
            problems.add("model_attributes_type", &[json!("body")], msg, &other);
            Err(problems)
        }
        Err(error) => {
            problems.add_ctx(
                "json_invalid",
                &[json!("body"), json!(error.column())],
                "JSON decode error",
                &json!({}),
                json!({ "error": error.to_string() }),
            );
            Err(problems)
        }
    }
}

/// A report request (`CaptureFeedbackCreate`).
struct ReportRequest {
    target: String,
    expected_text: String,
    notes: String,
    snapshot: Snapshot,
    source: String,
    replaces: Option<String>,
}

fn one_of(
    allowed: &'static [&'static str],
) -> impl Fn(&Value, &[Value], &mut Problems) -> Option<String> {
    move |value, loc, problems| match value.as_str() {
        Some(text) if allowed.contains(&text) => Some(text.to_string()),
        _ => {
            literal(allowed, value, loc, problems);
            None
        }
    }
}

fn at_most(max: usize) -> impl Fn(&Value, &[Value], &mut Problems) -> Option<String> {
    move |value, loc, problems| {
        let text = string(value, loc, problems)?;
        if text.chars().count() <= max {
            return Some(text);
        }
        let msg = format!("String should have at most {max} characters");
        problems.add_ctx(
            "string_too_long",
            loc,
            msg,
            value,
            json!({ "max_length": max }),
        );
        None
    }
}

fn report_request(fields: Map<String, Value>) -> Result<ReportRequest, Problems> {
    let whole = Value::Object(fields);
    let mut problems = Problems::default();
    let field = |name: &str, default: Option<Value>, problems: &mut Problems| -> Option<Value> {
        match (whole.get(name), default) {
            (Some(value), _) => Some(value.clone()),
            (None, Some(default)) => Some(default),
            (None, None) => {
                let loc = [json!("body"), json!(name)];
                problems.add("missing", &loc, "Field required", &whole);
                None
            }
        }
    };
    let loc = |name: &str| [json!("body"), json!(name)];
    let target = field("target", None, &mut problems)
        .and_then(|v| one_of(&["raw", "refined"])(&v, &loc("target"), &mut problems));
    let expected_text = field("expected_text", None, &mut problems)
        .and_then(|v| at_most(100_000)(&v, &loc("expected_text"), &mut problems));
    let notes = field("notes", Some(json!("")), &mut problems)
        .and_then(|v| at_most(5000)(&v, &loc("notes"), &mut problems));
    let snapshot = field("snapshot", None, &mut problems)
        .and_then(|v| captures::snapshot(&v, &loc("snapshot"), &mut problems));
    let source = field("source", Some(json!("manual")), &mut problems).and_then(|v| {
        one_of(&["manual", "voice_fix", "redictation"])(&v, &loc("source"), &mut problems)
    });
    let replaces = field("replaces", Some(Value::Null), &mut problems)
        .and_then(|v| optional_string(&v, &loc("replaces"), &mut problems));
    match (target, expected_text, notes, snapshot, source, replaces) {
        (
            Some(target),
            Some(expected_text),
            Some(notes),
            Some(snapshot),
            Some(source),
            Some(replaces),
        ) if problems.is_empty() => Ok(ReportRequest {
            target,
            expected_text,
            notes,
            snapshot,
            source,
            replaces,
        }),
        _ => Err(problems),
    }
}

async fn list_reports(State(state): State<AppState>, Path(capture_id): Path<String>) -> Response {
    let listed = state
        .db
        .call(move |conn| match store::get(conn, &capture_id)? {
            Some(_) => store::list_reports(conn, Some(&capture_id)).map(Some),
            None => Ok(None),
        })
        .await;
    match listed {
        Ok(Some(listed)) => reports(listed),
        Ok(None) => capture_not_found(),
        Err(error) => failed(error),
    }
}

/// Every report, for evaluation: audio stays on each capture.
async fn export(State(state): State<AppState>) -> Response {
    match state.db.call(|conn| store::list_reports(conn, None)).await {
        Ok(listed) => reports(listed),
        Err(error) => failed(error),
    }
}

fn conflict(message: &str) -> Response {
    detail(StatusCode::CONFLICT, message)
}

/// Keep a correction of a capture as the user saw it, in place of the one
/// it amends, and learn from it.
async fn report(
    State(state): State<AppState>,
    Path(capture_id): Path<String>,
    bytes: Bytes,
) -> Response {
    let request = match body(&bytes).and_then(report_request) {
        Ok(request) => request,
        Err(problems) => return invalid(problems),
    };
    let capture = match find(&state, capture_id.clone()).await {
        Ok(Some(capture)) => capture,
        Ok(None) => return capture_not_found(),
        Err(error) => return failed(error),
    };
    if !request.snapshot.is(&capture) {
        return conflict(Refused::Changed.message());
    }
    let Some(original) = capture.output(&request.target).map(str::to_string) else {
        return conflict("This capture has no refined output to report.");
    };
    // Spoken fixes come from speech: letters the user spelled ("M-E-G-H-A-N")
    // are one word, as they would have typed it.
    let expected = if request.source == "manual" {
        request.expected_text
    } else {
        match worker::join_spelling(&state.proxy, &request.expected_text).await {
            Ok(joined) => joined,
            Err(error) => return worker_down(error),
        }
    };
    if expected == original {
        return conflict("Expected output must differ from the model output.");
    }
    let new = NewReport {
        capture_id,
        target: request.target,
        expected_text: expected,
        notes: request.notes.trim().to_string(),
        source: request.source,
        replaces: request.replaces,
        seen: capture,
        filed_by: None,
    };
    let sync = state.sync;
    let saved = state
        .db
        .call(move |conn| {
            let saved = match store::save_report(conn, new, sync)? {
                Ok(saved) => saved,
                Err(refused) => return Ok(Err(refused)),
            };
            if let Some(replaced) = &saved.replaced {
                delete_added_by(conn, &replaced.id, sync)?;
            }
            Ok(Ok(saved))
        })
        .await;
    let saved = match saved {
        Ok(Ok(saved)) => saved,
        Ok(Err(Refused::NoCapture)) => return capture_not_found(),
        Ok(Err(refused)) => return conflict(refused.message()),
        Err(error) => return failed(error),
    };
    state.events.publish(Kind::Learned);
    let row = &saved.report;
    let learn = [Learn {
        report_id: &row.id,
        source: &row.source,
        original: &original,
        expected: &row.expected_text,
    }];
    let changed = [(row.target.clone(), row.source.clone())];
    worker::reports_changed(
        &state.proxy,
        &learn,
        &changed,
        saved.replaced.is_some(),
        None,
    )
    .await;
    Json(ReportModel::from(saved.report)).into_response()
}

/// Withdraw one report and what it taught. A capture's reports of one
/// target stack, each made from the one before, so the newer ones lose this
/// one's changes and keep their own; one left changing nothing goes too.
async fn withdraw(
    State(state): State<AppState>,
    Path((capture_id, report_id)): Path<(String, String)>,
) -> Response {
    let not_found = || detail(StatusCode::NOT_FOUND, "Report not found");
    let id = report_id.clone();
    let (stack, at) = match state
        .db
        .call(move |conn| store::stack(conn, &capture_id, &id))
        .await
    {
        Ok(Some(found)) => found,
        Ok(None) => return not_found(),
        Err(error) => return failed(error),
    };
    let row = stack[at].clone();
    let snapshot: Value = serde_json::from_str(&row.snapshot).unwrap_or(Value::Null);
    let field = if row.target == "raw" {
        "transcript_raw"
    } else {
        "transcript_refined"
    };
    let original = snapshot
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let before = match at {
        0 => original.clone(),
        _ => stack[at - 1].expected_text.clone(),
    };
    let newer = &stack[at + 1..];
    let texts: Vec<String> = newer.iter().map(|r| r.expected_text.clone()).collect();
    let texts = match worker::take_back(&state.proxy, &before, &row.expected_text, &texts).await {
        Ok(texts) => texts,
        Err(error) => return worker_down(error),
    };
    let updates: Vec<(String, Option<String>)> = newer
        .iter()
        .zip(texts)
        .map(|(report, text)| (report.id.clone(), (text != original).then_some(text)))
        .collect();
    let sync = state.sync;
    let (id, kept) = (row.id.clone(), updates.clone());
    let withdrawn = state
        .db
        .call(move |conn| {
            if !store::withdraw(conn, &id, &kept, sync)? {
                return Ok(false);
            }
            delete_added_by(conn, &id, sync)?;
            Ok(true)
        })
        .await;
    match withdrawn {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(error) => return failed(error),
    }
    state.events.publish(Kind::Learned);
    // A newer report that still has the fix keeps its word.
    let learn: Vec<Learn> = newer
        .iter()
        .zip(&updates)
        .filter_map(|(report, (_, text))| {
            Some(Learn {
                report_id: &report.id,
                source: &report.source,
                original: &original,
                expected: text.as_deref()?,
            })
        })
        .collect();
    let changed: BTreeSet<(String, String)> = std::iter::once(&row)
        .chain(newer)
        .map(|report| (report.target.clone(), report.source.clone()))
        .collect();
    let changed: Vec<_> = changed.into_iter().collect();
    worker::reports_changed(&state.proxy, &learn, &changed, true, None).await;
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_query_is_read_as_fastapi_reads_it() {
        let (limit, offset, apps) =
            list_query("limit=5&limit=7&offset=2&app_bundle_id=com.a").unwrap();
        assert_eq!((limit, offset), (7, 2));
        assert!(matches!(apps, Apps::One(id) if id == "com.a"));
        assert!(matches!(
            list_query("app_bundle_id=&unknown_app=no").unwrap().2,
            Apps::All
        ));
        assert!(matches!(
            list_query("app_bundle_id=x&unknown_app=yes").unwrap().2,
            Apps::Unknown
        ));
        let problems = list_query("limit=&offset=x&unknown_app=maybe").unwrap_err();
        let locs: Vec<_> = problems.0.iter().map(|p| p["loc"][1].clone()).collect();
        assert_eq!(
            locs,
            vec![json!("limit"), json!("offset"), json!("unknown_app")]
        );
    }

    #[test]
    fn a_report_request_lists_its_problems_in_field_order() {
        let fields = json!({ "target": "both", "notes": "x".repeat(5001), "source": "typed", "replaces": 3 });
        let Value::Object(fields) = fields else {
            unreachable!()
        };
        let problems = report_request(fields).err().unwrap();
        let kinds: Vec<_> = problems
            .0
            .iter()
            .map(|p| (p["type"].as_str().unwrap().to_string(), p["loc"][1].clone()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("literal_error".into(), json!("target")),
                ("missing".into(), json!("expected_text")),
                ("string_too_long".into(), json!("notes")),
                ("missing".into(), json!("snapshot")),
                ("literal_error".into(), json!("source")),
                ("string_type".into(), json!("replaces")),
            ]
        );
        assert_eq!(
            problems.0[4]["msg"],
            json!("Input should be 'manual', 'voice_fix' or 'redictation'")
        );
    }

    #[test]
    fn download_names_are_quoted_like_starlette() {
        assert_eq!(quoted("capture_ab-1.wav"), "capture_ab-1.wav");
        assert_eq!(quoted("capture_a b.wav"), "capture_a%20b.wav");
    }
}
