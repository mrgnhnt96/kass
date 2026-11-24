//! What cleanup in a writing style reads besides the transcript: the style's
//! flags and description, its learned punctuation habits, the user's own
//! examples and the rules summarized from older ones, and the words the
//! speaker taught for punctuation marks with the learned correction rules.
//!
//! Python keeps each in its own service and caches it in memory: `styles`,
//! `writing_style` (`writing-style.json`), `personal_examples` (corrections
//! in `kass.db` plus calibration rewrites), `correction_notes`
//! (`correction-notes.json`) and `correction_learning`
//! (`correction-learning.json`). Here [`load`] reads them all for one style,
//! fresh, so the dictation session calls it once per style it cleans up in.
//! A caller wanting every style's reads the corrections once
//! ([`CorrectionRows`]) and the files once ([`LearnedFiles`]) and builds
//! each style's from them ([`assemble`]).
//! The worker still writes the JSON files; this only reads them.

use std::path::Path;

use chrono::{NaiveDateTime, Timelike};
use kass_text::correction_rules::{Rule, Rules, MAX_RULES};
use kass_text::py;
use kass_text::refinement::{strip_stt_artifacts, Example, Flags, Punctuation};
use kass_text::spoken_punctuation::Learned;
use kass_text::writing_style::{as_spoken, Profile};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::captures::EXPLICIT_SOURCES;
use crate::styles::{Snapshot, Style, MIGRATED_STYLE};

/// Longest example, said or meant, that goes in the prompt.
pub const MAX_EXAMPLE_CHARS: usize = 800;
/// Enough to show how the user writes without bloating the prompt.
pub const MAX_PROMPT_EXAMPLES: usize = 16;
pub const MAX_PROMPT_CHARS: usize = 6000;

/// Everything cleanup in one style reads, read together.
pub struct CleanupInputs {
    pub style: Style,
    pub flags: Flags,
    /// The style's learned punctuation habits.
    pub profile: Profile,
    /// How the user says they write in the style's apps.
    pub description: String,
    /// The user's own examples in the prompt, oldest first.
    pub personal: Vec<Example>,
    /// The notes section of the cleanup prompt, when the style has notes.
    pub notes: Option<String>,
    /// The speaker's own words for punctuation marks.
    pub punctuation: Learned,
    /// The learned correction rules.
    pub rules: Rules,
}

/// The inputs for cleanup in `style_id`, or the default style for `None` or
/// a deleted style. `data_dir` is the folder `kass.db` is in.
pub fn load(
    conn: &Connection,
    data_dir: &Path,
    styles: &Snapshot,
    style_id: Option<&str>,
    self_correction: bool,
) -> rusqlite::Result<CleanupInputs> {
    let files = LearnedFiles::read(data_dir);
    let corrections = CorrectionRows::read(conn)?.examples(styles);
    Ok(assemble(
        &files,
        styles,
        style_id,
        self_correction,
        &corrections,
    ))
}

/// The inputs for cleanup in `style_id` from what [`load`] reads: the
/// learning files and every style's corrections
/// ([`CorrectionRows::examples`]). Needs no database, so a caller building
/// several styles' inputs reads the corrections once and builds them after
/// letting the database go.
pub fn assemble(
    files: &LearnedFiles,
    styles: &Snapshot,
    style_id: Option<&str>,
    self_correction: bool,
    corrections: &[(String, PersonalExample)],
) -> CleanupInputs {
    let style = styles.resolve(style_id).clone();
    let personal = examples_for(&files.writing, styles, &style.id, corrections);
    let notes = notes_from(files.notes.as_ref(), &style.id);
    let (rules, punctuation) = learned_from(files.learning.as_ref());
    CleanupInputs {
        flags: flags_for(&style, self_correction),
        profile: profile(&files.writing, &style.id),
        description: py::strip(&style.description).to_string(),
        personal: for_prompt(&in_prompt(&personal)),
        notes: notes_section(&notes),
        punctuation,
        rules,
        style,
    }
}

/// The cleanup flags for dictation in `style`; self-correction stays a
/// global setting. Filler removal and a capitalized first word are always
/// on: the base prompt removes fillers, and learned punctuation covers
/// lowercase starts.
pub fn flags_for(style: &Style, self_correction: bool) -> Flags {
    Flags {
        self_correction,
        preserve_technical: style.preserve_technical,
        punctuation: Punctuation::parse(&style.punctuation_style).unwrap_or_default(),
        style: Some(style.id.clone()),
        ..Flags::default()
    }
}

// -- files ---------------------------------------------------------------------

/// A JSON file the worker keeps in the data folder; `None` when it is
/// missing or unreadable.
fn read_json(data_dir: &Path, name: &str) -> Option<Value> {
    let path = data_dir.join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("[cleanup] couldn't read {}: {error}", path.display());
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!("[cleanup] {} isn't valid JSON: {error}", path.display());
            None
        }
    }
}

fn entries(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

/// The files the worker keeps that cleanup reads, read once for any number
/// of styles.
#[derive(Clone, Debug)]
pub struct LearnedFiles {
    /// `writing-style.json`, as [`read_writing_style`] reads it.
    pub writing: Value,
    /// `correction-notes.json`.
    pub notes: Option<Value>,
    /// `correction-learning.json`.
    pub learning: Option<Value>,
}

impl LearnedFiles {
    pub fn read(data_dir: &Path) -> Self {
        LearnedFiles {
            writing: read_writing_style(data_dir),
            notes: read_json(data_dir, NOTES_FILE),
            learning: read_json(data_dir, LEARNING_FILE),
        }
    }
}

const NOTES_FILE: &str = "correction-notes.json";
const LEARNING_FILE: &str = "correction-learning.json";

// -- writing-style.json --------------------------------------------------------

/// `writing-style.json` in version 2: `{"styles": {id: profile},
/// "hidden_examples": [...]}`. Version 1 had one profile, the style the
/// global settings moved into.
pub fn read_writing_style(data_dir: &Path) -> Value {
    let mut state = json!({"version": 2, "styles": {}, "hidden_examples": []});
    let Some(Value::Object(loaded)) = read_json(data_dir, "writing-style.json") else {
        return state;
    };
    if loaded.get("version").and_then(Value::as_i64).unwrap_or(1) >= 2 {
        for (key, value) in loaded {
            state[key] = value;
        }
        return state;
    }
    let profile: Map<String, Value> = ["runs", "last_run_at", "examples", "feedback_counts"]
        .into_iter()
        .filter_map(|key| Some((key.to_string(), loaded.get(key)?.clone())))
        .collect();
    state["styles"] = json!({ MIGRATED_STYLE: profile });
    if let Some(hidden) = loaded.get("hidden_examples") {
        state["hidden_examples"] = hidden.clone();
    }
    state
}

fn profile_json<'v>(writing: &'v Value, style_id: &str) -> &'v Value {
    &writing["styles"][style_id]
}

/// What `style_id` learned; empty for a style that hasn't learned anything.
pub fn profile(writing: &Value, style_id: &str) -> Profile {
    Profile::from_json(profile_json(writing, style_id))
}

/// The examples the user removed from their examples list.
pub fn hidden_examples(writing: &Value) -> Vec<String> {
    entries(&writing["hidden_examples"])
        .filter_map(|id| id.as_str().map(String::from))
        .collect()
}

// -- personal examples ---------------------------------------------------------

/// One of the user's "when I say this, I mean this" examples.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PersonalExample {
    pub id: String,
    /// "correction" or "calibration".
    pub source: &'static str,
    pub said: String,
    pub meant: String,
    pub created_at: Option<String>,
    /// Corrections only: the app the dictation was in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
}

/// A calibration example's id: Python hashes `f"{created_at}|{shown}|{written}"`.
fn calibration_id(created_at: Option<&str>, shown: &str, written: &str) -> String {
    let key = format!("{}|{shown}|{written}", created_at.unwrap_or("None"));
    let digest = Sha256::digest(key.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("calibration:{}", &hex[..16])
}

/// `style_id`'s calibration rewrites and teach replies the user edited, as
/// "said, meant" examples, in the order kept.
pub fn calibration_examples(writing: &Value, style_id: &str) -> Vec<PersonalExample> {
    entries(&profile_json(writing, style_id)["examples"])
        .filter_map(|example| {
            let shown = example["shown"].as_str()?;
            let written = example["written"].as_str()?;
            if py::strip(written) == py::strip(shown) {
                return None;
            }
            let created_at = example["created_at"].as_str();
            // Spoken-jumble paragraphs are kept as said; punctuation ones as shown.
            let said = example["said"]
                .as_str()
                .filter(|said| !said.is_empty())
                .map_or_else(|| as_spoken(shown), String::from);
            Some(PersonalExample {
                id: calibration_id(created_at, shown, written),
                source: "calibration",
                said,
                meant: written.to_string(),
                created_at: created_at.map(String::from),
                app_bundle_id: None,
                app_name: None,
            })
        })
        .collect()
}

/// A stored datetime as Python's `isoformat` writes it: microseconds only
/// when set.
pub(crate) fn isoformat(stored: &str) -> String {
    match NaiveDateTime::parse_from_str(stored, "%Y-%m-%d %H:%M:%S%.f") {
        Ok(at) if at.nanosecond() / 1000 == 0 => at.format("%Y-%m-%dT%H:%M:%S").to_string(),
        Ok(at) => at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
        Err(_) => stored.to_string(),
    }
}

/// One explicit correction as stored: its id, capture, the text meant, the
/// capture as it was, when, and the style it teaches if it says.
type CorrectionRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// The explicit corrections to cleaned-up text as stored, newest first. One
/// query reads them; [`CorrectionRows::examples`] makes them examples after
/// the database is let go, since cleaning each said text up takes far
/// longer than reading it.
#[derive(Clone, Debug, Default, Hash)]
pub struct CorrectionRows(Vec<CorrectionRow>);

impl CorrectionRows {
    pub fn read(conn: &Connection) -> rusqlite::Result<Self> {
        let sources = EXPLICIT_SOURCES
            .map(|source| format!("'{source}'"))
            .join(", ");
        // A correction whose capture history retention deleted keeps its own
        // copy of the style it teaches.
        let mut statement = conn.prepare(&format!(
            "SELECT f.id, f.capture_id, f.expected_text, f.snapshot, f.created_at, \
             COALESCE(c.teaches_style_id, f.teaches_style_id) \
             FROM capture_feedback f LEFT OUTER JOIN captures c ON c.id = f.capture_id \
             WHERE f.target = 'refined' AND f.source IN ({sources}) \
             ORDER BY f.created_at DESC, f.id DESC"
        ))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(CorrectionRows(rows))
    }

    /// The corrections as examples, newest first, the latest per capture,
    /// with the style each teaches.
    pub fn examples(&self, styles: &Snapshot) -> Vec<(String, PersonalExample)> {
        let mut seen = std::collections::HashSet::new();
        let mut examples = Vec::new();
        for (id, capture_id, meant, snapshot, created_at, teaches) in &self.0 {
            // The latest correction of a capture replaces earlier ones.
            if !seen.insert(capture_id) {
                continue;
            }
            let Ok(Value::Object(snapshot)) = serde_json::from_str::<Value>(snapshot) else {
                continue;
            };
            let text = |key: &str| snapshot.get(key).and_then(Value::as_str).map(String::from);
            // Captures saved before Whisper's loops were stripped at the source.
            let said = strip_stt_artifacts(&text("transcript_raw").unwrap_or_default());
            if said.is_empty() || meant.is_empty() {
                continue;
            }
            let app_bundle_id = text("app_bundle_id");
            let style = styles
                .correction_style(app_bundle_id.as_deref(), teaches.as_deref())
                .to_string();
            examples.push((
                style,
                PersonalExample {
                    id: format!("correction:{id}"),
                    source: "correction",
                    said,
                    meant: meant.clone(),
                    created_at: created_at.as_deref().map(isoformat),
                    app_bundle_id,
                    app_name: text("app_name"),
                },
            ));
        }
        examples
    }
}

/// Every example cleanup in `style_id` may use, newest first: its
/// calibration rewrites and the corrections that teach it, less the hidden
/// ones, the too long ones and the ones that changed nothing.
pub fn all_examples(
    conn: &Connection,
    writing: &Value,
    styles: &Snapshot,
    style_id: &str,
) -> rusqlite::Result<Vec<PersonalExample>> {
    if styles.get(Some(style_id)).is_none() {
        return Ok(Vec::new());
    }
    let corrections = CorrectionRows::read(conn)?.examples(styles);
    Ok(examples_for(writing, styles, style_id, &corrections))
}

/// [`all_examples`] from every style's corrections, already read.
fn examples_for(
    writing: &Value,
    styles: &Snapshot,
    style_id: &str,
    corrections: &[(String, PersonalExample)],
) -> Vec<PersonalExample> {
    if styles.get(Some(style_id)).is_none() {
        return Vec::new();
    }
    let hidden: std::collections::HashSet<String> = hidden_examples(writing).into_iter().collect();
    let mut examples = calibration_examples(writing, style_id);
    examples.extend(
        corrections
            .iter()
            .filter(|(style, _)| style == style_id)
            .map(|(_, example)| example.clone()),
    );
    examples.retain(|example| {
        !hidden.contains(&example.id)
            && py::len(&example.said).max(py::len(&example.meant)) <= MAX_EXAMPLE_CHARS
            && example.said != example.meant
    });
    // Stable, like Python's sort with reverse=True: ties keep their order.
    examples.sort_by(|a, b| {
        let a = a.created_at.as_deref().unwrap_or("");
        let b = b.created_at.as_deref().unwrap_or("");
        b.cmp(a)
    });
    examples
}

/// The most recent of `all` (newest first) that fit the prompt budget,
/// newest first. Older ones are summarized into correction notes instead.
pub fn in_prompt(all: &[PersonalExample]) -> Vec<PersonalExample> {
    let mut size = 0;
    all.iter()
        .take(MAX_PROMPT_EXAMPLES)
        .take_while(|example| {
            size += py::len(&example.said) + py::len(&example.meant);
            size <= MAX_PROMPT_CHARS
        })
        .cloned()
        .collect()
}

/// The examples every cleanup in a style shows the model, oldest first, so
/// a new one goes at the end and the cached prompt before it still matches.
pub fn for_prompt(in_prompt: &[PersonalExample]) -> Vec<Example> {
    in_prompt
        .iter()
        .rev()
        .map(|example| (example.said.clone(), example.meant.clone()))
        .collect()
}

// -- correction-notes.json -----------------------------------------------------

/// `style_id`'s rules summarized from its older examples. Version 1 of the
/// file held one list, the style the global settings moved into.
pub fn correction_notes(data_dir: &Path, style_id: &str) -> Vec<String> {
    notes_from(read_json(data_dir, NOTES_FILE).as_ref(), style_id)
}

/// [`correction_notes`] from the file, already read.
fn notes_from(loaded: Option<&Value>, style_id: &str) -> Vec<String> {
    let Some(loaded) = loaded else {
        return Vec::new();
    };
    let notes = match loaded["version"].as_i64() {
        Some(1) if style_id == MIGRATED_STYLE => &loaded["notes"],
        Some(2) if loaded["styles"].is_object() => &loaded["styles"][style_id]["notes"],
        Some(1) => return Vec::new(),
        _ => {
            eprintln!("[cleanup] unsupported correction notes; cleaning up without them");
            return Vec::new();
        }
    };
    entries(notes)
        .filter_map(|note| note.as_str().map(String::from))
        .filter(|note| !crate::correction_notes::about_exclamation(note))
        .collect()
}

/// The cleanup prompt's section for `notes`, when there are any.
pub fn notes_section(notes: &[String]) -> Option<String> {
    if notes.is_empty() {
        return None;
    }
    let lines: Vec<String> = notes.iter().map(|note| format!("- {note}")).collect();
    Some(format!(
        "Rules this speaker taught in earlier corrections. Follow them:\n{}",
        lines.join("\n")
    ))
}

// -- correction-learning.json --------------------------------------------------

/// The learned correction rules and the speaker's words for punctuation
/// marks. A state Python would refuse (another version, too many rules, a
/// malformed rule) gives neither.
pub fn learned_corrections(data_dir: &Path) -> (Rules, Learned) {
    learned_from(read_json(data_dir, LEARNING_FILE).as_ref())
}

/// [`learned_corrections`] from the file, already read.
fn learned_from(state: Option<&Value>) -> (Rules, Learned) {
    let Some(state) = state else {
        return (Rules::new(Vec::new()), Learned::default());
    };
    let usable = state["version"] == json!(1)
        && state["rules"].as_array().is_some_and(|rules| {
            rules.len() <= MAX_RULES && rules.iter().all(|rule| Rule::from_json(rule).is_some())
        });
    if !usable {
        eprintln!("[cleanup] correction learning isn't usable; using no learned rules");
        return (Rules::new(Vec::new()), Learned::default());
    }
    let learned = &state["punctuation"];
    let punctuation = Learned {
        aliases: learned["aliases"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(said, mark)| Some((said.clone(), mark.as_str()?.to_string())))
            .collect(),
        kept: entries(&learned["kept"])
            .filter_map(|kept| kept.as_str().map(String::from))
            .collect(),
    };
    (Rules::from_state(state), punctuation)
}
