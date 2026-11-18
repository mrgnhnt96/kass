//! Saved captures and the corrections the user reports on them
//! (docs/plans/CORRECTION_LEARNING.md). Python: `backend/services/captures.py`
//! and `backend/services/capture_feedback.py`.
//!
//! A capture is one dictation, recording or uploaded file: its audio, the
//! raw transcript and the cleaned-up text. A report is the user's correction
//! of one of the two, kept with a snapshot of the capture as they saw it.
//! A capture's reports of one target stack, each made from the one before.
//!
//! This module holds the shapes and the checks pydantic made on them;
//! [`store`] reads and writes `kass.db`. What a report teaches (dictionary
//! words, the writing style, correction learning) is still learned in the
//! Python worker, which this server tells whenever reports change.

pub mod in_use;
pub mod retention;
pub mod store;

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, Timelike};
use serde::{Serialize, Serializer};
use serde_json::{json, Value};

/// Reports the user made on purpose, in Captures or by voice. A redictation
/// is weaker evidence: it teaches correction learning only.
pub const EXPLICIT_SOURCES: [&str; 2] = ["manual", "voice_fix"];

/// A datetime as pydantic writes one: ISO 8601, microseconds only when set.
pub(crate) fn iso<S: Serializer>(
    at: &Option<NaiveDateTime>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match at {
        None => serializer.serialize_none(),
        Some(at) if at.nanosecond() / 1000 == 0 => {
            serializer.serialize_str(&at.format("%Y-%m-%dT%H:%M:%S").to_string())
        }
        Some(at) => serializer.serialize_str(&at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
    }
}

/// The toggles a capture was cleaned up with (`RefinementFlagsModel`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Flags {
    pub smart_cleanup: bool,
    pub self_correction: bool,
    pub preserve_technical: bool,
    pub punctuation_style: String,
    pub capitalize_first: bool,
    pub style: Option<String>,
}

/// Why a capture's cleanup is flagged for the user to check
/// (`RefinementReviewModel`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Review {
    pub outcome: String,
    pub added: Vec<String>,
    pub missing: Vec<String>,
    pub reasons: Vec<String>,
}

/// A capture as the API returns it (`CaptureResponse`), fields in its order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Capture {
    pub id: String,
    pub audio_path: String,
    pub source: String,
    pub language: Option<String>,
    pub duration_ms: Option<i64>,
    pub transcript_raw: String,
    pub transcript_refined: Option<String>,
    pub stt_model: Option<String>,
    pub llm_model: Option<String>,
    pub refinement_flags: Option<Flags>,
    pub refinement_review: Option<Review>,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub command_selection: Option<String>,
    pub command_instruction: Option<String>,
    pub command_transform: Option<String>,
    pub style_id: Option<String>,
    pub audio_deleted: bool,
    pub mac_id: Option<String>,
    pub mac_name: Option<String>,
    #[serde(serialize_with = "iso")]
    pub created_at: Option<NaiveDateTime>,
}

impl Capture {
    /// The text a report of `target` corrects; None for "refined" when the
    /// capture wasn't cleaned up.
    pub fn output(&self, target: &str) -> Option<&str> {
        if target == "raw" {
            Some(&self.transcript_raw)
        } else {
            self.transcript_refined.as_deref()
        }
    }
}

/// The capture a report's request says the user saw. One whose time has a
/// UTC offset is never the stored capture, whose time has none: pydantic's
/// comparison says so too.
pub struct Snapshot {
    pub capture: Capture,
    zoned: bool,
}

impl Snapshot {
    pub fn is(&self, capture: &Capture) -> bool {
        !self.zoned && self.capture == *capture
    }
}

// -- pydantic's checks --------------------------------------------------------

/// What a request got wrong, as FastAPI's 422 lists it.
#[derive(Debug, Default)]
pub(crate) struct Problems(pub Vec<Value>);

impl Problems {
    pub fn add(&mut self, kind: &str, loc: &[Value], msg: impl Into<String>, input: &Value) {
        self.0
            .push(json!({ "type": kind, "loc": loc, "msg": msg.into(), "input": input }));
    }

    /// One with the `ctx` pydantic gives it (the pattern, the limit, the
    /// values expected).
    pub fn add_ctx(
        &mut self,
        kind: &str,
        loc: &[Value],
        msg: impl Into<String>,
        input: &Value,
        ctx: Value,
    ) {
        self.0.push(
            json!({ "type": kind, "loc": loc, "msg": msg.into(), "input": input, "ctx": ctx }),
        );
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

type Parse<T> = fn(&Value, &[Value], &mut Problems) -> Option<T>;

fn at(loc: &[Value], name: &str) -> Vec<Value> {
    let mut loc = loc.to_vec();
    loc.push(json!(name));
    loc
}

/// A field of `object`: `default` when absent, or "missing" when there is none.
fn field<T>(
    object: &Value,
    loc: &[Value],
    name: &str,
    default: Option<T>,
    parse: Parse<T>,
    problems: &mut Problems,
) -> Option<T> {
    match (object.get(name), default) {
        (Some(value), _) => parse(value, &at(loc, name), problems),
        (None, Some(default)) => Some(default),
        (None, None) => {
            problems.add("missing", &at(loc, name), "Field required", object);
            None
        }
    }
}

pub(crate) fn string(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        other => {
            problems.add("string_type", loc, "Input should be a valid string", other);
            None
        }
    }
}

pub(crate) fn optional_string(
    value: &Value,
    loc: &[Value],
    problems: &mut Problems,
) -> Option<Option<String>> {
    match value {
        Value::Null => Some(None),
        other => string(other, loc, problems).map(Some),
    }
}

/// Pydantic's lax booleans: true/false, 0/1, and words like "yes" or "off".
pub(crate) fn boolean(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<bool> {
    let parsed = match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(false),
        Value::Number(n) if n.as_f64() == Some(1.0) => Some(true),
        Value::String(word) => lax_bool(word),
        _ => None,
    };
    if parsed.is_none() {
        let msg = if value.is_string() {
            "Input should be a valid boolean, unable to interpret input"
        } else {
            "Input should be a valid boolean"
        };
        problems.add("bool_parsing", loc, msg, value);
    }
    parsed
}

pub(crate) fn lax_bool(word: &str) -> Option<bool> {
    match word.to_lowercase().as_str() {
        "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
        "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
        _ => None,
    }
}

pub(crate) fn integer(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<i64> {
    match value {
        Value::Number(n) if n.as_i64().is_some() => n.as_i64(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => Some(f as i64),
            _ => {
                let msg = "Input should be a valid integer, got a number with a fractional part";
                problems.add("int_from_float", loc, msg, value);
                None
            }
        },
        Value::String(text) => int_text(text, loc, problems),
        other => {
            problems.add("int_type", loc, "Input should be a valid integer", other);
            None
        }
    }
}

pub(crate) fn int_text(text: &str, loc: &[Value], problems: &mut Problems) -> Option<i64> {
    let parsed = text.trim().parse::<i64>().ok();
    if parsed.is_none() {
        let msg = "Input should be a valid integer, unable to parse string as an integer";
        problems.add("int_parsing", loc, msg, &json!(text));
    }
    parsed
}

fn optional_integer(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<Option<i64>> {
    match value {
        Value::Null => Some(None),
        other => integer(other, loc, problems).map(Some),
    }
}

fn strings(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<Vec<String>> {
    let Value::Array(items) = value else {
        problems.add("list_type", loc, "Input should be a valid list", value);
        return None;
    };
    let mut out = Vec::new();
    let mut ok = true;
    for (index, item) in items.iter().enumerate() {
        let mut loc = loc.to_vec();
        loc.push(json!(index));
        match string(item, &loc, problems) {
            Some(text) => out.push(text),
            None => ok = false,
        }
    }
    ok.then_some(out)
}

/// When, and whether it carried a UTC offset (pydantic keeps one).
fn time(
    value: &Value,
    loc: &[Value],
    problems: &mut Problems,
) -> Option<(Option<NaiveDateTime>, bool)> {
    match value {
        Value::Null => Some((None, false)),
        // A Unix time: pydantic reads it as UTC.
        Value::Number(_) => Some((None, true)),
        Value::String(text) => {
            let naive = [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%dT%H:%M",
            ]
            .iter()
            .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok());
            if let Some(naive) = naive {
                return Some((Some(naive), false));
            }
            let zoned = DateTime::parse_from_rfc3339(text)
                .or_else(|_| DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f%:z"))
                .is_ok();
            if zoned {
                return Some((None, true));
            }
            problems.add(
                "datetime_from_date_parsing",
                loc,
                "Input should be a valid datetime",
                value,
            );
            None
        }
        other => {
            problems.add(
                "datetime_type",
                loc,
                "Input should be a valid datetime",
                other,
            );
            None
        }
    }
}

fn punctuation(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<String> {
    let style = string(value, loc, problems)?;
    if ["standard", "casual", "learned"].contains(&style.as_str()) {
        return Some(style);
    }
    let pattern = "^(standard|casual|learned)$";
    let msg = format!("String should match pattern '{pattern}'");
    problems.add_ctx(
        "string_pattern_mismatch",
        loc,
        msg,
        value,
        json!({ "pattern": pattern }),
    );
    None
}

fn flags(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<Flags> {
    if !value.is_object() {
        let msg = "Input should be a valid dictionary or instance of RefinementFlagsModel";
        problems.add("model_type", loc, msg, value);
        return None;
    }
    let smart_cleanup = field(value, loc, "smart_cleanup", Some(true), boolean, problems);
    let self_correction = field(value, loc, "self_correction", Some(true), boolean, problems);
    let preserve_technical = field(
        value,
        loc,
        "preserve_technical",
        Some(true),
        boolean,
        problems,
    );
    let punctuation_style = field(
        value,
        loc,
        "punctuation_style",
        Some("standard".to_string()),
        punctuation,
        problems,
    );
    let capitalize_first = field(
        value,
        loc,
        "capitalize_first",
        Some(true),
        boolean,
        problems,
    );
    let style = field(value, loc, "style", Some(None), optional_string, problems);
    Some(Flags {
        smart_cleanup: smart_cleanup?,
        self_correction: self_correction?,
        preserve_technical: preserve_technical?,
        punctuation_style: punctuation_style?,
        capitalize_first: capitalize_first?,
        style: style?,
    })
}

pub(crate) fn optional_flags(
    value: &Value,
    loc: &[Value],
    problems: &mut Problems,
) -> Option<Option<Flags>> {
    match value {
        Value::Null => Some(None),
        other => flags(other, loc, problems).map(Some),
    }
}

fn outcome(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<String> {
    match value.as_str() {
        Some(outcome @ ("review" | "reject")) => Some(outcome.to_string()),
        _ => {
            literal(&["review", "reject"], value, loc, problems);
            None
        }
    }
}

/// A value not among a `Literal`'s.
pub(crate) fn literal(allowed: &[&str], value: &Value, loc: &[Value], problems: &mut Problems) {
    let quoted: Vec<String> = allowed.iter().map(|a| format!("'{a}'")).collect();
    let expected = match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    };
    let msg = format!("Input should be {expected}");
    problems.add_ctx(
        "literal_error",
        loc,
        msg,
        value,
        json!({ "expected": expected }),
    );
}

fn review(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<Review> {
    if !value.is_object() {
        let msg = "Input should be a valid dictionary or instance of RefinementReviewModel";
        problems.add("model_type", loc, msg, value);
        return None;
    }
    let outcome = field(value, loc, "outcome", None, outcome, problems);
    let added = field(value, loc, "added", Some(Vec::new()), strings, problems);
    let missing = field(value, loc, "missing", Some(Vec::new()), strings, problems);
    let reasons = field(value, loc, "reasons", Some(Vec::new()), strings, problems);
    Some(Review {
        outcome: outcome?,
        added: added?,
        missing: missing?,
        reasons: reasons?,
    })
}

fn optional_review(
    value: &Value,
    loc: &[Value],
    problems: &mut Problems,
) -> Option<Option<Review>> {
    match value {
        Value::Null => Some(None),
        other => review(other, loc, problems).map(Some),
    }
}

/// A capture as pydantic reads `CaptureResponse` from JSON.
pub(crate) fn snapshot(value: &Value, loc: &[Value], problems: &mut Problems) -> Option<Snapshot> {
    if !value.is_object() {
        let msg = "Input should be a valid dictionary or instance of CaptureResponse";
        problems.add("model_type", loc, msg, value);
        return None;
    }
    let text =
        |name: &str, problems: &mut Problems| field(value, loc, name, None, string, problems);
    let id = text("id", problems);
    let audio_path = text("audio_path", problems);
    let source = text("source", problems);
    let optional = |name: &str, problems: &mut Problems| {
        field(value, loc, name, Some(None), optional_string, problems)
    };
    let language = optional("language", problems);
    let duration_ms = field(
        value,
        loc,
        "duration_ms",
        Some(None),
        optional_integer,
        problems,
    );
    let transcript_raw = text("transcript_raw", problems);
    let transcript_refined = optional("transcript_refined", problems);
    let stt_model = optional("stt_model", problems);
    let llm_model = optional("llm_model", problems);
    let refinement_flags = field(
        value,
        loc,
        "refinement_flags",
        Some(None),
        optional_flags,
        problems,
    );
    let refinement_review = field(
        value,
        loc,
        "refinement_review",
        Some(None),
        optional_review,
        problems,
    );
    let app_bundle_id = optional("app_bundle_id", problems);
    let app_name = optional("app_name", problems);
    let command_selection = optional("command_selection", problems);
    let command_instruction = optional("command_instruction", problems);
    let command_transform = optional("command_transform", problems);
    let style_id = optional("style_id", problems);
    let audio_deleted = field(value, loc, "audio_deleted", Some(false), boolean, problems);
    let mac_id = optional("mac_id", problems);
    let mac_name = optional("mac_name", problems);
    let created_at = field(
        value,
        loc,
        "created_at",
        Some((None, false)),
        time,
        problems,
    );
    let (created_at, zoned) = created_at?;
    Some(Snapshot {
        capture: Capture {
            id: id?,
            audio_path: audio_path?,
            source: source?,
            language: language?,
            duration_ms: duration_ms?,
            transcript_raw: transcript_raw?,
            transcript_refined: transcript_refined?,
            stt_model: stt_model?,
            llm_model: llm_model?,
            refinement_flags: refinement_flags?,
            refinement_review: refinement_review?,
            app_bundle_id: app_bundle_id?,
            app_name: app_name?,
            command_selection: command_selection?,
            command_instruction: command_instruction?,
            command_transform: command_transform?,
            style_id: style_id?,
            audio_deleted: audio_deleted?,
            mac_id: mac_id?,
            mac_name: mac_name?,
            created_at,
        },
        zoned,
    })
}

/// A stored column of JSON read as pydantic's `Model(**json.loads(text))`:
/// None when it isn't valid.
fn stored_model<T>(text: Option<&str>, parse: Parse<T>) -> Option<T> {
    let value: Value = serde_json::from_str(text.filter(|t| !t.is_empty())?).ok()?;
    let mut problems = Problems::default();
    let parsed = parse(&value, &[], &mut problems);
    parsed.filter(|_| problems.is_empty())
}

pub fn stored_flags(text: Option<&str>) -> Option<Flags> {
    stored_model(text, flags)
}

pub fn stored_review(text: Option<&str>) -> Option<Review> {
    stored_model(text, review)
}

/// A report's stored snapshot as `CaptureResponse` returns it, with fields
/// added since filled in; as stored if it no longer reads as one.
pub fn stored_snapshot(text: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Value::String(text.to_string());
    };
    let mut problems = Problems::default();
    match snapshot(&value, &[], &mut problems) {
        Some(snapshot) if problems.is_empty() => {
            serde_json::to_value(snapshot.capture).unwrap_or(value)
        }
        _ => value,
    }
}

// -- files --------------------------------------------------------------------

/// Where a stored audio path is, as the worker's `config.resolve_storage_path`
/// finds it. Paths are kept relative to the data folder (`kass.db`'s), but
/// older ones are absolute or start with "data/", and a data folder that
/// moved still holds them.
pub fn resolve_storage_path(data_dir: &Path, stored: &str) -> Option<PathBuf> {
    let path = Path::new(stored);
    let parts: Vec<Component> = path
        .components()
        .filter(|part| *part != Component::CurDir)
        .collect();
    if parts.is_empty() {
        return None;
    }
    if path.is_absolute() {
        if let Some(index) = parts
            .iter()
            .position(|part| *part == Component::Normal("data".as_ref()))
        {
            let tail: PathBuf = parts[index + 1..].iter().collect();
            let candidate = resolved(&data_dir.join(tail));
            if candidate.exists() || !path.exists() {
                return Some(candidate);
            }
        }
        return Some(path.to_path_buf());
    }
    let skip = usize::from(parts[0] == Component::Normal("data".as_ref()));
    let tail: PathBuf = parts[skip..].iter().collect();
    Some(resolved(&data_dir.join(tail)))
}

/// `Path.resolve()`: the real path when it exists, else the path with `..`
/// worked out.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> Value {
        json!({
            "id": "take", "audio_path": "captures/take.wav", "source": "dictation",
            "transcript_raw": "wrong words", "created_at": "2026-03-04T05:06:07.000123",
        })
    }

    #[test]
    fn a_snapshot_fills_defaults_and_keeps_its_time() {
        let mut problems = Problems::default();
        let snapshot = snapshot(&capture(), &[], &mut problems).unwrap();
        assert!(problems.is_empty());
        let out = serde_json::to_value(&snapshot.capture).unwrap();
        assert_eq!(out["audio_deleted"], json!(false));
        assert_eq!(out["refinement_flags"], Value::Null);
        assert_eq!(out["created_at"], json!("2026-03-04T05:06:07.000123"));
        assert!(snapshot.is(&snapshot.capture.clone()));
    }

    #[test]
    fn a_snapshot_with_an_offset_is_never_the_capture() {
        let mut value = capture();
        value["created_at"] = json!("2026-03-04T05:06:07Z");
        let snapshot = snapshot(&value, &[], &mut Problems::default()).unwrap();
        let mut same = snapshot.capture.clone();
        same.created_at = None;
        assert!(!snapshot.is(&same));
    }

    #[test]
    fn a_snapshot_lists_every_problem() {
        let mut value = capture();
        value.as_object_mut().unwrap().remove("id");
        value["duration_ms"] = json!(1.5);
        value["refinement_flags"] = json!({"punctuation_style": "loud"});
        let mut problems = Problems::default();
        assert!(snapshot(&value, &[json!("body"), json!("snapshot")], &mut problems).is_none());
        let kinds: Vec<_> = problems
            .0
            .iter()
            .map(|p| (p["type"].clone(), p["loc"].clone()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (json!("missing"), json!(["body", "snapshot", "id"])),
                (
                    json!("int_from_float"),
                    json!(["body", "snapshot", "duration_ms"])
                ),
                (
                    json!("string_pattern_mismatch"),
                    json!(["body", "snapshot", "refinement_flags", "punctuation_style"])
                ),
            ]
        );
    }

    #[test]
    fn stored_flags_fill_defaults_or_are_dropped() {
        let flags =
            stored_flags(Some(r#"{"smart_cleanup": 0, "style": "code", "extra": 1}"#)).unwrap();
        assert!(!flags.smart_cleanup && flags.capitalize_first);
        assert_eq!(flags.punctuation_style, "standard");
        assert_eq!(flags.style.as_deref(), Some("code"));
        assert_eq!(stored_flags(Some(r#"{"smart_cleanup": null}"#)), None);
        assert_eq!(stored_flags(Some("[1]")), None);
        assert_eq!(stored_flags(Some("not json")), None);
        assert_eq!(stored_flags(Some("")), None);
        assert_eq!(stored_review(Some(r#"{"added": ["x"]}"#)), None);
        assert_eq!(
            stored_review(Some(r#"{"outcome": "review", "reasons": ["names"]}"#))
                .unwrap()
                .reasons,
            vec!["names"]
        );
    }

    #[test]
    fn storage_paths_resolve_like_the_worker() {
        let data = std::env::temp_dir().join(format!("kass-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(data.join("captures")).unwrap();
        std::fs::write(data.join("captures/take.wav"), b"RIFF").unwrap();
        let data = std::fs::canonicalize(&data).unwrap();
        let take = data.join("captures/take.wav");
        assert_eq!(resolve_storage_path(&data, ""), None);
        assert_eq!(resolve_storage_path(&data, "."), None);
        assert_eq!(
            resolve_storage_path(&data, "captures/take.wav"),
            Some(take.clone())
        );
        assert_eq!(
            resolve_storage_path(&data, "data/captures/take.wav"),
            Some(take.clone())
        );
        // An absolute path from a data folder that moved.
        let moved = "/Volumes/Old/Kass/data/captures/take.wav";
        assert_eq!(resolve_storage_path(&data, moved), Some(take.clone()));
        // An absolute path elsewhere that exists stays.
        let outside = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(
            resolve_storage_path(&data, outside.to_str().unwrap()),
            Some(outside.clone())
        );
        std::fs::remove_dir_all(&data).unwrap();
    }
}
