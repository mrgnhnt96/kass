//! One streamed dictation, from its start message to its saved capture:
//! phrases recognized as the user speaks, the open sentence cleaned and
//! settled as it finishes, a style asked for by name, voice edits, Command
//! Mode, provisional text after release and how it was said. Python:
//! `backend/services/capture_stream.py` (`StreamingCapture`).
//!
//! Its state sits behind one lock, held only between awaits: the socket
//! appends audio and sets the app or context while the run recognizes and
//! cleans. Events go out through a channel, in order, to the socket.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use kass_text::content_check::{self, Summary, Verdict};
use kass_text::dictionary::Dictionary;
use kass_text::phrase_seams::{self, Names};
use kass_text::py;
use kass_text::re;
use kass_text::refinement::{self, Flags, Punctuation};
use kass_text::sentence_tail;
use kass_text::spoken_corrections::retracted_by;
use kass_text::spoken_punctuation::period_after_closers;
use kass_text::voice_commands::{commands_alone, mark_commands};
use kass_text::voice_edits::{self, Declined, Planned};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use super::intake::{Intake, Next, Source, Start};
use super::models::{self, CleanupPrompt, Foreground, Generate, Recognize, RecognizeFile};
use super::prosody::{self, measure_voice, Baseline, Expression, Learned as Taught, Said, Voice};
use crate::captures::store::{self as captures, NewCapture, NewReport};
use crate::cleanup_inputs::{self, CleanupInputs};
use crate::events::{ascii_json, Kind};
use crate::settings::{self, Settings, Transform};
use crate::styles::{self, Snapshot, Style};
use crate::{learned, worker, AppState};

/// Words provisional text holds back while learned corrections may still
/// change them.
const CORRECTION_HOLDBACK: usize = 4;
/// The earlier text Whisper continues a phrase from.
const PHRASE_CONTEXT_CHARS: usize = 600;
/// The field's text before the caret that is kept.
const FIELD_CONTEXT_CHARS: usize = 600;
/// The most of the last take a voice edit may change.
const LAST_TAKE_CHARS: usize = 1000;
/// The most selected text Command Mode rewrites.
pub const MAX_SELECTION_CHARS: usize = 16_000;
/// Speech before the first look at the opening words for a style request.
const STYLE_PEEK_AFTER_S: f64 = 1.0;
/// The pause that look waits for.
const STYLE_PEEK_PAUSE_S: f64 = 0.2;
/// The look happens by then, pause or not.
const STYLE_PEEK_BY_S: f64 = 2.5;

/// A phrase that may correct earlier words ("no wait").
fn correction_cue(text: &str) -> bool {
    re!(
        r"\b(actually|no wait|no actually|make that|I mean|scratch that)\b",
        py::I
    )
    .is_match(text)
}

// -- text helpers -------------------------------------------------------------

/// Remove only matching boundary tokens from overlapping recognition
/// windows.
pub fn join_overlap(prefix: &str, tail: &str) -> String {
    let left = py::split(prefix);
    let right = py::split(tail);
    let key = |word: &str| py::lower(&re!(r"[^\w]").replace(word, ""));
    let mut count = 32.min(left.len()).min(right.len());
    while count > 0 {
        let ends = left[left.len() - count..].iter().map(|w| key(w));
        let starts = right[..count].iter().map(|w| key(w));
        if ends.eq(starts) {
            let joined: Vec<&str> = left.iter().chain(&right[count..]).copied().collect();
            return joined.join(" ");
        }
        count -= 1;
    }
    [prefix, tail]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The start of `text` minus its last `holdback` words and the punctuation
/// before them: those can still change.
pub fn stable_prefix(text: &str, holdback: usize) -> String {
    let words = re!(r"\S+").finditer(text);
    if words.len() < holdback || (holdback > 0 && words.len() == holdback) {
        return String::new();
    }
    let kept = if holdback > 0 {
        use kass_text::py::Groups;
        &text[..words[words.len() - holdback].start()]
    } else {
        text
    };
    py::rstrip(&re!(r"[^\w\s]+$").replace(py::rstrip(kept), "")).to_string()
}

/// `str[:n]` with Python's negative and zero bounds, in characters.
fn python_head(text: &str, n: isize) -> String {
    if n >= 0 {
        py::head(text, n as usize).to_string()
    } else {
        py::drop_last(text, n.unsigned_abs()).to_string()
    }
}

fn joined(left: &str, right: &str) -> String {
    py::strip(&format!("{left} {right}")).to_string()
}

// -- Command Mode transforms ----------------------------------------------------

/// The name an instruction asks for, without the words around it ("ok run
/// polish on this please" → "polish"). Python: `commands._requested_name`.
fn requested_name(instruction: &str) -> String {
    let leading = re!(
        r"^(?:(?:ok(?:ay)?|so|um+|uh+|hey|please|can you|could you|would you|let's|lets|go ahead and|run|apply|use|do|give it|give this|give me)\s+)+"
    );
    let trailing = re!(
        r"(?:\s+(?:on|to|for|over)?\s*(?:this|that|it|these|those|the (?:text|selection|selected text|paragraph|message|email|draft))|\s+(?:please|transform|now))+$"
    );
    let mut key = settings::spoken_key(instruction);
    loop {
        let next = py::strip(&trailing.replace(&leading.replace(&key, ""), "")).to_string();
        if next == key {
            return key;
        }
        key = next;
    }
}

/// The transform `instruction` asks for by name, or None.
fn match_transform<'t>(instruction: &str, transforms: &'t [Transform]) -> Option<&'t Transform> {
    let requested = requested_name(instruction);
    if requested.is_empty() {
        return None;
    }
    transforms.iter().find(|transform| {
        let name = settings::spoken_key(&transform.name);
        !name.is_empty() && (requested == name || requested == requested_name(&transform.name))
    })
}

/// The instruction to run for what was said, and the transform it named.
pub fn resolve_instruction(spoken: &str, transforms: &[Transform]) -> (String, Option<Transform>) {
    match match_transform(spoken, transforms) {
        Some(transform) => (transform.instruction.clone(), Some(transform.clone())),
        None => (py::strip(spoken).to_string(), None),
    }
}

// -- what's saved, as Python's json.dumps wrote it -----------------------------

fn json_list(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| ascii_json(item)).collect();
    format!("[{}]", items.join(", "))
}

/// The cleanup flags as saved with a capture (`RefinementFlags.to_dict`).
pub fn flags_json(flags: &Flags) -> String {
    let yes = |value: bool| if value { "true" } else { "false" };
    let mut fields = vec![
        format!("\"smart_cleanup\": {}", yes(flags.smart_cleanup)),
        format!("\"self_correction\": {}", yes(flags.self_correction)),
        format!("\"preserve_technical\": {}", yes(flags.preserve_technical)),
    ];
    if flags.punctuation != Punctuation::Standard {
        fields.push(format!(
            "\"punctuation_style\": {}",
            ascii_json(flags.punctuation.as_str())
        ));
    }
    if !flags.capitalize_first {
        fields.push("\"capitalize_first\": false".to_string());
    }
    if let Some(style) = &flags.style {
        fields.push(format!("\"style\": {}", ascii_json(style)));
    }
    format!("{{{}}}", fields.join(", "))
}

/// The content review as saved with a capture.
pub fn review_json(summary: &Summary) -> String {
    format!(
        "{{\"outcome\": {}, \"added\": {}, \"missing\": {}, \"reasons\": {}}}",
        ascii_json(&summary.outcome),
        json_list(&summary.added),
        json_list(&summary.missing),
        json_list(&summary.reasons),
    )
}

/// The cleanup prompt for `inputs`' style, run on `model`.
pub(crate) fn cleanup_prompt(inputs: &CleanupInputs, model: &str) -> CleanupPrompt {
    let (system, examples) = refinement::prompt(
        &inputs.flags,
        &inputs.profile,
        &inputs.description,
        &inputs.personal,
        inputs.notes.as_deref(),
    );
    CleanupPrompt {
        model_size: model.to_string(),
        flags: Some(inputs.flags.to_json()),
        system,
        examples,
        cache_key: Some(refinement::cache_key(&inputs.flags, true, false)),
    }
}

// -- the session --------------------------------------------------------------

/// A cleanup the release may stop: the speaker went on after it started.
#[derive(Default)]
struct Stop {
    flag: AtomicBool,
    /// The generation running, to stop in the worker.
    generating: Mutex<Option<String>>,
}

/// What the latest settle replaced: settled text, its gap, the raw words
/// settled and the reviews then.
type Settled = (String, Option<String>, String, Vec<Verdict>);

#[derive(Clone, Copy)]
enum Stage {
    Recognize,
    Refine,
}

struct State {
    intake: Intake,
    expression: Expression,
    revision: u64,

    app_bundle_id: Option<String>,
    app_name: Option<String>,
    app_category: Option<String>,

    /// The field's text before the caret, and whether the take continues
    /// its sentence.
    field_before: String,
    continues: bool,
    last_take: Option<String>,
    last_take_capture_id: Option<String>,
    last_take_own_chars: usize,
    selection: Option<String>,

    style: Style,
    /// The style asked for by name, and whether a phrase said so (not only
    /// a peek).
    spoken: Option<Style>,
    spoken_confirmed: bool,
    style_announced: Option<String>,
    inputs: Arc<CleanupInputs>,
    dictionary: Arc<Dictionary>,
    names: Names,
    cleanup_started: bool,
    peeks: u32,
    peeked_at: i64,

    editing: bool,
    edit_announced: bool,
    edit: Option<Result<Planned, Declined>>,
    command: Option<(String, Option<Transform>)>,

    /// Whisper's own text, the context each phrase continues.
    heard: String,
    /// What was said, saved as the raw transcript.
    raw: String,
    /// The raw words since the settled text, and their latest cleanup.
    tail_raw: String,
    tail_cleaned: String,
    settled: String,
    /// Whitespace after settled text that ends a sentence.
    gap: Option<String>,
    last_settle: Option<Settled>,
    settled_reviews: Vec<Verdict>,
    refined: String,
    shown: String,
    paused: bool,
    overlap: bool,
    tail_dirty: bool,
    needs_final: bool,
    cleanup_closed: bool,
    taken_back: String,
    retractable: String,
    reviews: Vec<Verdict>,
    llm_model: Option<String>,
    refinement_error: Option<String>,

    finished: bool,
    finished_at: Option<Instant>,
    abort: bool,
    after_recognize: f64,
    after_refine: f64,
    release_tail_words: usize,
    persisted: bool,
    stop: Option<Arc<Stop>>,

    /// How it was said so far, once worked out after the latest phrase.
    said: Option<Arc<Said>>,
    baseline: Option<Baseline>,
    taught: Taught,
    /// Each phrase's words, fetched and placed in order.
    alignment: Option<JoinHandle<()>>,
}

pub struct Session {
    pub id: String,
    app: AppState,
    pub settings: Settings,
    pub start: Start,
    /// The speech model recognition runs, as the worker resolved it.
    stt_model: String,
    snapshot: Snapshot,
    data_dir: PathBuf,
    expressive: bool,
    wake: Notify,
    events: mpsc::UnboundedSender<Value>,
    started_at: Instant,
    state: Mutex<State>,
}

impl Session {
    /// A session for `start`, with the user's settings, styles and
    /// dictionary; events go to `events`. Also the worker's foreground
    /// hold, released when the session ends.
    pub async fn open(
        app: &AppState,
        start: &Value,
        events: mpsc::UnboundedSender<Value>,
    ) -> Result<(Arc<Session>, Foreground), String> {
        let settings = app
            .db
            .call(settings::get)
            .await
            .map_err(|e| e.to_string())?;
        let start = Start::parse(start, Some(&settings.language), &settings.stt_model)?;
        let (stt_model, styles_state) = tokio::join!(
            models::speech_model(&app.proxy, &start.stt_model),
            worker::styles_state(&app.proxy),
        );
        let stt_model = stt_model?;
        let data_dir = app
            .db
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let dictionaries = app.dictionaries.clone();
        let self_correction = settings.self_correction;
        let dir = data_dir.clone();
        let (snapshot, inputs, dictionary) = app
            .db
            .call(move |conn| {
                let snapshot = styles::load(conn, styles_state.teaching_style)?;
                let inputs = cleanup_inputs::load(conn, &dir, &snapshot, None, self_correction)?;
                let dictionary = dictionaries.for_app(conn, None, None)?;
                Ok((snapshot, inputs, dictionary))
            })
            .await
            .map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().to_string();
        let captures_dir = data_dir.join("captures");
        std::fs::create_dir_all(&captures_dir)
            .map_err(|e| format!("Could not make the captures folder: {e}"))?;
        let intake = Intake::new(
            &start,
            &captures_dir.join(format!("{id}.wav")),
            app.voice.clone(),
        )?;
        let foreground = models::begin_foreground(&app.proxy).await?;
        let expressive =
            settings.expressive && settings.auto_refine && start.source != Source::Command;
        let state = State {
            expression: Expression::new(start.sample_rate),
            intake,
            revision: 0,
            app_bundle_id: None,
            app_name: None,
            app_category: None,
            field_before: String::new(),
            continues: false,
            last_take: None,
            last_take_capture_id: None,
            last_take_own_chars: 0,
            selection: None,
            style: inputs.style.clone(),
            spoken: None,
            spoken_confirmed: false,
            style_announced: None,
            inputs: Arc::new(inputs),
            dictionary,
            names: Names::new(),
            cleanup_started: false,
            peeks: 0,
            peeked_at: 0,
            editing: false,
            edit_announced: false,
            edit: None,
            command: None,
            heard: String::new(),
            raw: String::new(),
            tail_raw: String::new(),
            tail_cleaned: String::new(),
            settled: String::new(),
            gap: None,
            last_settle: None,
            settled_reviews: Vec::new(),
            refined: String::new(),
            shown: String::new(),
            paused: false,
            overlap: false,
            tail_dirty: false,
            needs_final: false,
            cleanup_closed: true,
            taken_back: String::new(),
            retractable: String::new(),
            reviews: Vec::new(),
            llm_model: None,
            refinement_error: None,
            finished: false,
            finished_at: None,
            abort: false,
            after_recognize: 0.0,
            after_refine: 0.0,
            release_tail_words: 0,
            persisted: false,
            stop: None,
            said: None,
            baseline: None,
            taught: Taught::default(),
            alignment: None,
        };
        let session = Session {
            id,
            app: app.clone(),
            settings,
            start,
            stt_model,
            snapshot,
            data_dir,
            expressive,
            wake: Notify::new(),
            events,
            started_at: Instant::now(),
            state: Mutex::new(state),
        };
        Ok((Arc::new(session), foreground))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, st: &mut State, kind: &str, payload: Value) {
        st.revision += 1;
        let mut event = Map::new();
        event.insert("type".into(), kind.into());
        event.insert("session_id".into(), self.id.clone().into());
        event.insert("revision".into(), st.revision.into());
        event.insert("covered_samples".into(), st.intake.covered().into());
        event.insert("degraded_reason".into(), json!(st.intake.degraded_reason()));
        if let Value::Object(fields) = payload {
            event.extend(fields);
        }
        let _ = self.events.send(Value::Object(event));
    }

    pub fn is_command(&self) -> bool {
        self.start.source == Source::Command
    }

    /// Whether the session cleans up its text as it goes.
    pub fn cleans(&self) -> bool {
        self.settings.auto_refine && !self.is_command()
    }

    fn edits_on(&self) -> bool {
        self.settings.voice_edits && !self.is_command()
    }

    fn edit_possible(&self, st: &State) -> bool {
        self.edits_on() && st.last_take.as_deref().is_some_and(|take| !take.is_empty())
    }

    pub fn samples(&self) -> i64 {
        self.lock().intake.samples()
    }

    pub fn finished(&self) -> bool {
        self.lock().finished
    }

    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    pub fn refinement_error(&self) -> Option<String> {
        self.lock().refinement_error.clone()
    }

    pub fn degraded_reason(&self) -> Option<&'static str> {
        self.lock().intake.degraded_reason()
    }

    pub fn abort(&self) {
        self.lock().abort = true;
    }

    /// The prompt of the session's style, to prefill while the user speaks,
    /// and the style's name.
    pub fn style_prompt(&self) -> (CleanupPrompt, String) {
        let st = self.lock();
        (
            cleanup_prompt(&st.inputs, &self.settings.llm_model),
            st.style.name.clone(),
        )
    }

    pub fn flags(&self) -> Value {
        self.lock().inputs.flags.to_json()
    }

    // -- what the socket sends ------------------------------------------------

    pub fn append(&self, frame: &[u8]) -> Result<(), String> {
        let mut st = self.lock();
        let pcm = st.intake.append(frame)?;
        st.expression.feed(&pcm);
        drop(st);
        self.wake.notify_one();
        Ok(())
    }

    pub fn ignore_cue(&self, start: &Value, end: &Value) -> Result<(), String> {
        self.lock().intake.ignore_cue(start, end)
    }

    /// The field's text before the caret, known shortly after the take
    /// starts.
    pub fn set_context(&self, before: &Value) -> Result<(), String> {
        let Some(before) = before.as_str() else {
            return Err("Context must be text".into());
        };
        let mut st = self.lock();
        st.field_before = py::last(before, FIELD_CONTEXT_CHARS).to_string();
        st.continues = phrase_seams::continues_sentence(&st.field_before);
        Ok(())
    }

    /// The text before the caret, which a voice edit may change; `own_chars`
    /// of its end were written by the capture `capture_id`.
    pub fn set_last_take(
        &self,
        text: &Value,
        capture_id: &Value,
        own_chars: &Value,
    ) -> Result<(), String> {
        let text = match text.as_str() {
            Some(text) if py::len(text) <= LAST_TAKE_CHARS => text,
            _ => {
                return Err(format!(
                    "The last take must be text of at most {LAST_TAKE_CHARS} characters"
                ))
            }
        };
        let capture_id = match capture_id {
            Value::Null => None,
            Value::String(id) => Some(id.clone()),
            _ => return Err("Invalid capture id".into()),
        };
        let length = py::len(text);
        let own_chars = match own_chars {
            Value::Null => {
                if capture_id.as_deref().is_some_and(|id| !id.is_empty()) {
                    length
                } else {
                    0
                }
            }
            Value::Number(number) => match number.as_u64() {
                Some(chars) if chars as usize <= length => chars as usize,
                _ => return Err("Invalid own_chars".into()),
            },
            _ => return Err("Invalid own_chars".into()),
        };
        let mut st = self.lock();
        st.last_take = Some(text.to_string());
        st.last_take_capture_id = capture_id;
        st.last_take_own_chars = own_chars;
        Ok(())
    }

    /// The text a command session rewrites; the model is prefilled with it
    /// at once, while the user still speaks the instruction.
    pub fn set_selection(&self, text: &Value) -> Result<(), String> {
        if !self.is_command() {
            return Err("Only command sessions take a selection".into());
        }
        let Some(text) = text.as_str() else {
            return Err("Selection must be text".into());
        };
        if py::len(text) > MAX_SELECTION_CHARS {
            return Err(
                "Selection is too long for Command Mode (16,000 characters at most)".into(),
            );
        }
        self.lock().selection = Some(text.to_string());
        let proxy = self.app.proxy.clone();
        let selection = text.to_string();
        let model = self.settings.llm_model.clone();
        tokio::spawn(async move {
            if let Err(error) = models::command_prefill(&proxy, &selection, &model).await {
                eprintln!("[server] Command prefill failed: {error}");
            }
        });
        Ok(())
    }

    async fn inputs_for(&self, style_id: &str) -> Result<CleanupInputs, String> {
        let snapshot = self.snapshot.clone();
        let dir = self.data_dir.clone();
        let id = style_id.to_string();
        let self_correction = self.settings.self_correction;
        self.app
            .db
            .call(move |conn| {
                cleanup_inputs::load(conn, &dir, &snapshot, Some(&id), self_correction)
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn dictionary_for(
        &self,
        bundle_id: Option<String>,
        style_id: Option<String>,
    ) -> Result<Arc<Dictionary>, String> {
        let dictionaries = self.app.dictionaries.clone();
        self.app
            .db
            .call(move |conn| dictionaries.for_app(conn, bundle_id.as_deref(), style_id.as_deref()))
            .await
            .map_err(|e| e.to_string())
    }

    /// The dictation's target app, and so its writing style. Returns
    /// whether the style is now that app's: it can't change once a cleanup
    /// started or a style was asked for by name.
    pub async fn set_app(
        &self,
        bundle_id: Option<String>,
        name: Option<String>,
        category: Option<String>,
    ) -> Result<bool, String> {
        let spoken = {
            let mut st = self.lock();
            st.app_bundle_id = bundle_id.clone();
            st.app_name = name;
            if category.is_some() {
                st.app_category = category;
            }
            st.spoken.as_ref().map(|style| style.id.clone())
        };
        let dictionary = self.dictionary_for(bundle_id.clone(), spoken).await?;
        let style = {
            let mut st = self.lock();
            st.names.extend(dictionary.names.iter().cloned());
            st.dictionary = dictionary;
            if st.cleanup_started || st.spoken.is_some() {
                return Ok(false);
            }
            self.snapshot.for_app(bundle_id.as_deref()).clone()
        };
        let inputs = if style.id == self.lock().style.id {
            None
        } else {
            Some(self.inputs_for(&style.id).await?)
        };
        let mut st = self.lock();
        if st.cleanup_started || st.spoken.is_some() {
            return Ok(false);
        }
        if let Some(inputs) = inputs {
            st.inputs = Arc::new(inputs);
        }
        st.style = style;
        Ok(true)
    }

    /// Write the take in `style`, asked for by name; None goes back to the
    /// app's. The client hears of a change in a `style` event.
    async fn use_style(&self, style: Option<Style>) -> Result<(), String> {
        let (bundle_id, previous) = {
            let st = self.lock();
            (st.app_bundle_id.clone(), st.style.clone())
        };
        let target = style
            .clone()
            .unwrap_or_else(|| self.snapshot.for_app(bundle_id.as_deref()).clone());
        let inputs = if target.id == previous.id {
            None
        } else {
            Some(self.inputs_for(&target.id).await?)
        };
        let dictionary = self
            .dictionary_for(bundle_id, style.as_ref().map(|style| style.id.clone()))
            .await?;
        let mut st = self.lock();
        st.spoken = style.clone();
        st.style = target.clone();
        if let Some(inputs) = inputs {
            st.inputs = Arc::new(inputs);
        }
        st.names.extend(dictionary.names.iter().cloned());
        st.dictionary = dictionary;
        if previous.id == target.id
            && (style.is_none() || st.style_announced.as_deref() == Some(target.id.as_str()))
        {
            return Ok(());
        }
        st.style_announced = Some(target.id.clone());
        let from_name = (previous.id != target.id).then_some(previous.name);
        self.emit(
            &mut st,
            "style",
            json!({"style_id": target.id, "name": target.name, "from_name": from_name}),
        );
        Ok(())
    }

    /// `text` without an opening request for a style by name, which picks
    /// the style the take is written in.
    async fn take_spoken_style(&self, text: String) -> Result<String, String> {
        if self.is_command() {
            return Ok(text);
        }
        let (style, rest) = styles::spoken_style(&text, &self.snapshot);
        let Some(style) = style.cloned() else {
            let undo = {
                let st = self.lock();
                st.spoken.is_some() && !st.spoken_confirmed
            };
            if undo {
                self.use_style(None).await?;
            }
            return Ok(text);
        };
        self.use_style(Some(style)).await?;
        self.lock().spoken_confirmed = true;
        Ok(rest)
    }

    fn style_peek_due(&self, st: &State) -> bool {
        if !st.raw.is_empty()
            || st.spoken.is_some()
            || st.edit_announced
            || self.is_command()
            || st.finished
            || st.intake.has_cut()
            || st.peeks >= 2
        {
            return false;
        }
        let Some(start) = st.intake.speech().first_voice() else {
            return false;
        };
        let rate = f64::from(st.intake.rate());
        let said = st.intake.samples() - start;
        if said as f64 >= rate * STYLE_PEEK_BY_S {
            return st.peeks == 0 || (said - st.peeked_at) as f64 >= rate * 0.5;
        }
        st.peeks == 0
            && said as f64 >= rate * STYLE_PEEK_AFTER_S
            && st.intake.speech().quiet() as f64 >= rate * STYLE_PEEK_PAUSE_S
    }

    /// Recognize the audio so far only to find a style request or a voice
    /// edit, so each shows as soon as it is said.
    async fn peek_style(self: &Arc<Self>) -> Result<(), String> {
        let (pcm, offset) = {
            let mut st = self.lock();
            let start = st.intake.speech().first_voice().unwrap_or(0);
            st.peeks += 1;
            st.peeked_at = st.intake.samples() - start;
            if self.snapshot.styles.len() < 2 && !self.edit_possible(&st) {
                st.peeks = 2;
                return Ok(());
            }
            (st.intake.pending().to_vec(), st.intake.offset())
        };
        let text = self.recognize(pcm, offset, false).await?;
        let edit = {
            let st = self.lock();
            self.edit_possible(&st) && voice_edits::starts_edit(&text) && st.raw.is_empty()
        };
        if edit {
            self.announce_edit();
            return Ok(());
        }
        let (style, _) = styles::spoken_style(&text, &self.snapshot);
        let use_it = {
            let st = self.lock();
            st.raw.is_empty() && !st.finished
        };
        if let Some(style) = style.cloned().filter(|_| use_it) {
            self.use_style(Some(style)).await?;
        }
        Ok(())
    }

    fn take_edit(&self, text: &str) {
        if self.edits_on() && voice_edits::starts_edit(text) {
            self.lock().editing = true;
            self.announce_edit();
        }
    }

    /// Tell the client, once and while the user speaks, that the take is an
    /// edit of its last one.
    fn announce_edit(&self) {
        let mut st = self.lock();
        if st.edit_announced || st.finished || !self.edit_possible(&st) {
            return;
        }
        st.edit_announced = true;
        self.emit(&mut st, "edit", json!({}));
    }

    /// Plan the edit the take said; a take that opened like one but says
    /// none is a dictation, cleaned up whole.
    async fn finish_edit(self: &Arc<Self>) {
        {
            let mut st = self.lock();
            let take = if self.edit_possible(&st) {
                st.last_take.clone()
            } else {
                None
            };
            if let Some(planned) = voice_edits::plan(&st.raw, take.as_deref()) {
                st.edit = Some(planned);
                return;
            }
            st.editing = false;
            st.raw = mark_commands(&st.raw);
        }
        if self.settings.auto_refine {
            self.reconcile_refinement().await;
        }
    }

    /// For the final event: the last take before and after the edit, or why
    /// it was declined.
    pub fn edit_result(&self) -> Option<Value> {
        match &self.lock().edit {
            Some(Ok(planned)) => Some(json!({"before": planned.before, "after": planned.after})),
            Some(Err(declined)) => Some(json!({"declined": declined.message})),
            None => None,
        }
    }

    // -- text the session writes -------------------------------------------

    fn vocabulary(&self, st: &State) -> Vec<String> {
        let terms = &st.dictionary.terms;
        if !self.edits_on() {
            return terms.clone();
        }
        voice_edits::COMMAND_TERMS
            .iter()
            .map(|term| term.to_string())
            .chain(
                terms
                    .iter()
                    .filter(|term| !voice_edits::COMMAND_TERMS.contains(&term.as_str()))
                    .cloned(),
            )
            .collect()
    }

    /// `text` with its first word cased as in the raw transcript where the
    /// take continues the field's sentence, else as the style says.
    fn start_like_raw(&self, st: &State, text: &str) -> String {
        if py::lstrip_chars(text, " \t").starts_with('\n') {
            return text.to_string();
        }
        if st.continues {
            return phrase_seams::match_raw_start(text, &st.raw);
        }
        refinement::style_first_word(text, &st.inputs.flags, &st.names)
    }

    /// Learned corrections, then the dictionary, so the user's own entries
    /// win.
    fn corrected(&self, st: &State, text: &str) -> String {
        let corrected = st
            .inputs
            .rules
            .apply_learned_corrections(text, self.start.language.as_deref());
        st.dictionary.apply(&corrected)
    }

    /// Words provisional text holds back: a correction or dictionary entry
    /// may still change them.
    fn holdback(&self, st: &State, minimum: usize) -> usize {
        let span = if st.dictionary.span > 0 {
            st.dictionary.span + 1
        } else {
            0
        };
        let corrections = if st.inputs.rules.is_empty() {
            minimum
        } else {
            CORRECTION_HOLDBACK
        };
        corrections.max(span)
    }

    fn spent(st: &mut State, stage: Stage, started: Instant) {
        let Some(finished_at) = st.finished_at else {
            return;
        };
        let seconds = Instant::now()
            .saturating_duration_since(started.max(finished_at))
            .as_secs_f64();
        match stage {
            Stage::Recognize => st.after_recognize += seconds,
            Stage::Refine => st.after_refine += seconds,
        }
    }

    /// `text` with "!" where it was said with clearly more energy, and words
    /// drawn out written so. Uses only what is worked out already.
    fn express(&self, st: &State, text: &str, final_: bool) -> String {
        if self.expressive && !text.is_empty() {
            if let Some(said) = &st.said {
                return prosody::express(text, said, st.baseline.as_ref(), &st.taught);
            }
            if final_ {
                eprintln!(
                    "[server] How the dictation was said wasn't worked out in time; written as it is"
                );
            }
        }
        text.to_string()
    }

    fn close_dictation(&self, st: &State, text: &str, closed: bool) -> String {
        let mut text = if closed {
            phrase_seams::close_phrase(text)
        } else {
            text.to_string()
        };
        text = phrase_seams::keep_raw_capitals(&text, &st.raw, &st.names);
        let flags = &st.inputs.flags;
        if flags.punctuation == Punctuation::Learned {
            text = st.inputs.profile.apply_learned(&text);
        }
        if flags.smart_cleanup {
            text = refinement::keep_said_punctuation(&st.raw, &text, Some(&st.inputs.punctuation));
        }
        period_after_closers(&text)
    }

    fn join(&self, st: &State, previous: &str, phrase: &str, raw_phrase: &str) -> String {
        if st.overlap {
            return join_overlap(previous, phrase);
        }
        let flags = &st.inputs.flags;
        if flags.punctuation == Punctuation::Learned {
            let standard = phrase_seams::join_phrases(previous, phrase, raw_phrase, "standard");
            return st.inputs.profile.apply_learned(&standard);
        }
        phrase_seams::join_phrases(previous, phrase, raw_phrase, flags.punctuation.as_str())
    }

    /// `text`, cleaned from `raw`, after the settled text.
    fn compose(&self, st: &State, settled: &str, text: &str, raw: &str) -> String {
        if py::strip(settled).is_empty() {
            return self.start_like_raw(st, text);
        }
        if text.is_empty() {
            return settled.to_string();
        }
        if let Some(gap) = &st.gap {
            return if text.starts_with('\n') {
                format!("{settled}{text}")
            } else {
                format!("{settled}{gap}{text}")
            };
        }
        self.join(st, settled, text, raw)
    }

    fn offers_provisional(&self, st: &State) -> bool {
        self.start.provisional
            && st.finished
            && self.settings.auto_refine
            && self.settings.allow_auto_paste
            && !st.overlap
            && st.intake.degraded_reason().is_none()
            && !st.intake.backlogged()
            && !st.needs_final
            && st.refinement_error.is_none()
            && !st.editing
    }

    /// Offer the client the part of `text` that should survive to the final.
    fn show(&self, st: &mut State, text: &str, holdback: usize) {
        if !self.offers_provisional(st) {
            return;
        }
        let stable = stable_prefix(text, holdback);
        if py::len(&stable) > py::len(&st.shown) {
            st.shown = stable.clone();
            self.emit(st, "provisional", json!({"text": stable}));
        }
    }

    fn show_cleaned_so_far(&self) {
        let mut st = self.lock();
        let settled = if st.settled.is_empty() {
            String::new()
        } else {
            self.corrected(&st, &st.settled)
        };
        let holdback = self.holdback(&st, 0);
        self.show(&mut st, &settled, holdback);
    }

    /// What finish would deliver if the cleanup of `prompt` after `prefix`
    /// ended with `partial`, offered as provisional text.
    fn project(&self, prefix: &str, prompt: &str, partial: &str) {
        let mut st = self.lock();
        if !self.offers_provisional(&st) {
            return;
        }
        let mut refined = py::rstrip(py::lstrip_chars(partial, " \t")).to_string();
        if st.inputs.flags.punctuation == Punctuation::Learned {
            refined = st.inputs.profile.apply_learned(&refined);
        }
        let closed = py::rstrip(&refined).ends_with('.');
        let composed = self.compose(&st, prefix, &refined, prompt);
        let text = self.corrected(&st, &composed);
        let text = self.express(&st, &self.close_dictation(&st, &text, closed), false);
        let holdback = self.holdback(&st, 1);
        self.show(&mut st, &text, holdback);
    }

    // -- recognition and cleanup -------------------------------------------

    /// Whisper's text for `pcm`, which starts at sample `start`. `measure`
    /// keeps the phrase's word times for how it was said.
    async fn recognize(
        self: &Arc<Self>,
        pcm: Vec<i16>,
        start: i64,
        measure: bool,
    ) -> Result<String, String> {
        let request = {
            let st = self.lock();
            if pcm.is_empty() || !st.intake.speech().heard(start, start + pcm.len() as i64) {
                return Ok(String::new());
            }
            let earlier = if !st.heard.is_empty() {
                st.heard.as_str()
            } else if st.continues {
                st.field_before.as_str()
            } else {
                ""
            };
            Recognize {
                rate: st.intake.rate(),
                language: self.start.language.clone(),
                stt_model: Some(self.stt_model.clone()),
                previous_text: Some(py::last(earlier, PHRASE_CONTEXT_CHARS).to_string()),
                vocabulary: self.vocabulary(&st),
                alignment: measure && !self.is_command(),
            }
        };
        let started = Instant::now();
        let recognized = models::recognize(&self.app.proxy, &request, &pcm).await;
        Self::spent(&mut self.lock(), Stage::Recognize, started);
        let recognized = recognized?;
        if let Some(alignment) = recognized.alignment {
            self.place_words(alignment, start);
        }
        Ok(py::strip(&recognized.text).to_string())
    }

    /// Fetch a phrase's words and place them in the expression, after the
    /// phrases before it; when the take is expressive, work out how it was
    /// said so far. Never holds up recognition.
    fn place_words(self: &Arc<Self>, alignment: String, start: i64) {
        let session = self.clone();
        let mut st = self.lock();
        let previous = st.alignment.take();
        // Like Python's latest voice task: none until this phrase's is ready.
        st.said = None;
        st.alignment = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let words = match models::alignment_words(&session.app.proxy, &alignment).await {
                Ok(words) => words,
                Err(error) => {
                    eprintln!("[server] couldn't read the phrase's word times: {error}");
                    return;
                }
            };
            let (frames, words, rule) = {
                let mut st = session.lock();
                st.expression.phrase(start.max(0) as u64, words);
                if !session.expressive || st.expression.failed() {
                    return;
                }
                let words = st.expression.words();
                if words.is_empty() {
                    return;
                }
                (
                    st.expression.frames().clone(),
                    words,
                    st.taught.rule.clone(),
                )
            };
            let worked_out = tokio::task::spawn_blocking(move || {
                let voice = Voice::new(&frames, words);
                let stretched = measure_voice(&voice, None, &rule).stretched;
                Said { voice, stretched }
            })
            .await;
            match worked_out {
                Ok(said) => session.lock().said = Some(Arc::new(said)),
                Err(error) => eprintln!(
                    "[server] Couldn't follow how the dictation was said; no \"!\" this time: {error}"
                ),
            }
        }));
    }

    /// The cleanup of `transcript`, line by line, and the model that made
    /// it. `hint` is text the output mostly copies; `stop` ends it early;
    /// with `project`, each partial generation is offered as provisional
    /// text after `project`'s settled prefix.
    async fn refine_transcript(
        &self,
        transcript: &str,
        hint: &str,
        stop: Option<&Arc<Stop>>,
        project: Option<(&str, &str)>,
    ) -> Result<(String, String), String> {
        let inputs = self.lock().inputs.clone();
        let model = self.settings.llm_model.clone();
        if let Some(alone) = commands_alone(transcript) {
            return Ok((alone, model));
        }
        let flags = &inputs.flags;
        let (cleaned, explicit) =
            refinement::prepare_refinement(transcript, flags, Some(&inputs.punctuation));
        if let Some(explicit) = explicit {
            return Ok((explicit, model));
        }
        let prompt = cleanup_prompt(&inputs, &model);
        let mut done = String::new();
        for (line, breaks) in refinement::lines(&cleaned) {
            if stop.is_some_and(|stop| stop.flag.load(Ordering::SeqCst)) {
                break;
            }
            let line_cleaned = if py::strip(line).is_empty() {
                String::new()
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                if let Some(stop) = stop {
                    *stop.generating.lock().unwrap_or_else(|p| p.into_inner()) = Some(id.clone());
                }
                let request = Generate {
                    id,
                    prompt: refinement::without_final_period(line),
                    hint: Some(hint.to_string()),
                    max_tokens: 2048,
                    temperature: 0.2,
                    cleanup: prompt.clone(),
                };
                let before = done.clone();
                let generated = models::generate(&self.app.proxy, &request, |partial| {
                    if let Some((prefix, raw)) = project {
                        self.project(prefix, raw, &format!("{before}{partial}"));
                    }
                })
                .await;
                if let Some(stop) = stop {
                    *stop.generating.lock().unwrap_or_else(|p| p.into_inner()) = None;
                }
                refinement::finish_line(&generated?.text)
            };
            done.push_str(&line_cleaned);
            done.push_str(breaks);
        }
        let text = refinement::finish(
            transcript,
            &done,
            flags,
            &inputs.profile,
            Some(&inputs.punctuation),
        );
        Ok((text, model))
    }

    /// Add a recognized phrase; `paused` when its audio was cut at a pause.
    async fn accept(self: &Arc<Self>, text: String, paused: bool) {
        let mut text = text;
        if !text.is_empty() && self.lock().raw.is_empty() {
            match self.take_spoken_style(text.clone()).await {
                Ok(rest) => text = rest,
                Err(error) => eprintln!("[server] couldn't switch to the style asked for: {error}"),
            }
            self.take_edit(&text);
        }
        let said_before;
        {
            let mut st = self.lock();
            let earlier = st.heard.clone();
            said_before = st.raw.clone();
            st.heard = self.join(&st, &earlier, &text, &text);
            if st.editing {
                if !text.is_empty() {
                    st.raw = if st.overlap {
                        join_overlap(&st.raw, &text)
                    } else {
                        joined(&st.raw, &text)
                    };
                    st.paused = paused;
                }
                let raw = st.raw.clone();
                self.emit(
                    &mut st,
                    "transcript",
                    json!({"accepted_text": raw, "provisional_text": "", "text": raw, "final": false}),
                );
                return;
            }
            if !text.is_empty() {
                if !st.overlap {
                    let mut phrase = text.clone();
                    if !earlier.is_empty() {
                        let before = if st.paused {
                            st.tail_raw.clone()
                        } else {
                            String::new()
                        };
                        let (tail, continued) =
                            phrase_seams::continue_after_seam(&before, &text, &earlier, &st.names);
                        phrase = continued;
                        if tail != before {
                            let cut = py::len(&tail) as isize - py::len(&before) as isize;
                            st.raw = python_head(&st.raw, cut);
                            st.tail_raw = tail;
                        }
                    } else if st.continues {
                        phrase = phrase_seams::continue_phrase(&text, &st.field_before, &st.names);
                    }
                    if paused {
                        phrase = phrase_seams::strip_pause_mark(&phrase);
                    }
                    st.raw = joined(&st.raw, &phrase);
                    st.tail_raw = joined(&st.tail_raw, &phrase);
                } else {
                    st.raw = join_overlap(&st.raw, &text);
                    st.tail_raw = join_overlap(&st.tail_raw, &text);
                }
                st.raw = mark_commands(&st.raw);
                st.tail_raw = mark_commands(&st.tail_raw);
                st.tail_dirty = true;
                st.paused = paused;
            }
            let raw = st.raw.clone();
            self.emit(
                &mut st,
                "transcript",
                json!({"accepted_text": raw, "provisional_text": "", "text": raw, "final": false}),
            );
            if !self.settings.auto_refine || self.is_command() {
                return;
            }
            st.cleanup_started = st.cleanup_started || !st.raw.is_empty();
        }
        if let Err(error) = self.clean_phrase(&text, &said_before).await {
            eprintln!("[server] Streaming refinement failed: {error}");
            self.lock().refinement_error = Some(error);
        }
    }

    async fn clean_phrase(self: &Arc<Self>, text: &str, said_before: &str) -> Result<(), String> {
        let clean = {
            let mut st = self.lock();
            let (_, correction) = refinement::prepare_refinement(
                &st.raw,
                &st.inputs.flags,
                Some(&st.inputs.punctuation),
            );
            if let Some(correction) = correction {
                st.refined = self.start_like_raw(&st, &correction);
                st.cleanup_closed = true;
                st.llm_model = Some(self.settings.llm_model.clone());
                st.settled = correction;
                st.gap = None;
                st.last_settle = None;
                st.tail_raw.clear();
                st.tail_cleaned.clear();
                st.tail_dirty = false;
                false
            } else if !text.is_empty() {
                let retracted = if st.inputs.flags.self_correction && !st.settled.is_empty() {
                    retracted_by(text, said_before)
                } else {
                    None
                };
                if let Some(retracted) = retracted {
                    st.taken_back = retracted.clone();
                    if st.last_settle.is_some() {
                        Self::reopen(&mut st);
                    }
                    if !st.settled.is_empty() {
                        st.retractable = retracted;
                    }
                }
                true
            } else {
                false
            }
        };
        if clean {
            self.clean_tail().await?;
        }
        let mut st = self.lock();
        st.refined = self.corrected(&st, &st.refined);
        let refined = st.refined.clone();
        self.emit(&mut st, "refined", json!({"text": refined}));
        Ok(())
    }

    /// Put the latest settled text back in the open tail, for a correction
    /// of it.
    fn reopen(st: &mut State) {
        let Some((settled, gap, raw, reviews)) = st.last_settle.take() else {
            return;
        };
        st.settled = settled;
        st.gap = gap;
        st.settled_reviews = reviews;
        st.tail_raw = joined(&raw, &st.tail_raw);
        st.tail_cleaned.clear();
    }

    fn settle(&self, st: &mut State, text: &str, raw: &str, gap: Option<String>) {
        st.last_settle = Some((
            st.settled.clone(),
            st.gap.clone(),
            raw.to_string(),
            st.settled_reviews.clone(),
        ));
        let settled = st.settled.clone();
        let composed = self.compose(st, &settled, text, raw);
        st.settled = self.express(st, &composed, false);
        st.gap = gap;
        st.settled_reviews = st.reviews.clone();
    }

    fn speech_follows(st: &State) -> bool {
        let (offset, samples) = (st.intake.offset(), st.intake.samples());
        samples > offset && st.intake.speech().heard(offset, samples)
    }

    /// Clean the open tail and settle every finished sentence of the result.
    async fn clean_tail(self: &Arc<Self>) -> Result<(), String> {
        let (prompt, hint, stop, prefix) = {
            let mut st = self.lock();
            let prompt = st.tail_raw.clone();
            let stop = (!st.finished).then(|| Arc::new(Stop::default()));
            st.stop = stop.clone();
            if st.finished {
                st.release_tail_words = py::split(&prompt).len();
            }
            (prompt, st.tail_cleaned.clone(), stop, st.settled.clone())
        };
        let started = Instant::now();
        let refined = self
            .refine_transcript(&prompt, &hint, stop.as_ref(), Some((&prefix, &prompt)))
            .await;
        let mut st = self.lock();
        st.stop = None;
        Self::spent(&mut st, Stage::Refine, started);
        let (refined, model) = refined?;
        st.llm_model = Some(model);
        if stop.is_some_and(|stop| stop.flag.load(Ordering::SeqCst)) {
            return Ok(());
        }
        let (mut refined, verdict) = content_check::check_refinement(
            &prompt,
            &refined,
            &st.inputs.flags,
            &refinement::shown_examples(&st.inputs.personal),
            &st.taken_back,
            Some(&st.inputs.punctuation),
        );
        st.tail_dirty = false;
        let rejected = verdict.outcome == "reject";
        let mut reviews = st.settled_reviews.clone();
        if verdict.outcome != "ok" {
            reviews.push(verdict);
        }
        st.reviews = reviews;
        st.cleanup_closed = rejected || py::rstrip(&refined).ends_with('.');
        if rejected && st.inputs.flags.self_correction && correction_cue(&prompt) {
            st.needs_final = true;
        }
        let finished = if !rejected && !st.overlap {
            sentence_tail::settle(&prompt, &refined)
        } else {
            None
        };
        if let Some(finished) = finished {
            self.settle(
                &mut st,
                &finished.committed,
                &finished.committed_raw,
                Some(finished.gap),
            );
            st.tail_raw = finished.open_raw;
            refined = finished.open_cleaned;
        } else if py::split(&prompt).len() > sentence_tail::MAX_OPEN_WORDS && !st.finished {
            let open = phrase_seams::open_phrase(&refined, &prompt);
            self.settle(&mut st, &open, &prompt, None);
            st.tail_raw.clear();
            refined.clear();
        }
        st.tail_cleaned = refined.clone();
        let shown = if refined.is_empty() {
            String::new()
        } else {
            phrase_seams::open_phrase(&refined, &st.tail_raw)
        };
        let (settled, tail_raw) = (st.settled.clone(), st.tail_raw.clone());
        st.refined = self.compose(&st, &settled, &shown, &tail_raw);
        Ok(())
    }

    /// Deliver settled text and the cleaned tail, closed like a finished
    /// dictation.
    async fn finish_cleanup(self: &Arc<Self>) {
        let ready = async {
            let dirty = {
                let st = self.lock();
                st.tail_dirty && !st.tail_raw.is_empty()
            };
            if dirty {
                self.clean_tail().await?;
            }
            Ok::<_, String>(())
        }
        .await;
        if let Err(error) = ready {
            eprintln!("[server] Streaming refinement failed: {error}");
            self.lock().refinement_error = Some(error);
            return;
        }
        if self.lock().needs_final {
            self.reconcile_refinement().await;
            return;
        }
        let (mut closed, retractable) = {
            let st = self.lock();
            let composed = self.compose(&st, &st.settled, &st.tail_cleaned, &st.tail_raw);
            let text = self.corrected(&st, &composed);
            let closed = self.close_dictation(&st, &text, st.cleanup_closed);
            (self.express(&st, &closed, true), st.retractable.clone())
        };
        if !retractable.is_empty() {
            closed = self.clean_whole_take(closed, &retractable).await;
        }
        let mut st = self.lock();
        if closed != st.refined {
            st.refined = closed.clone();
            self.emit(&mut st, "refined", json!({"text": closed}));
        }
    }

    /// The whole take cleaned again, for a correction of settled text; the
    /// streamed text when that fails or the content check rejects it.
    async fn clean_whole_take(self: &Arc<Self>, streamed: String, retractable: &str) -> String {
        let raw = self.lock().raw.clone();
        let started = Instant::now();
        let refined = self.refine_transcript(&raw, "", None, None).await;
        Self::spent(&mut self.lock(), Stage::Refine, started);
        let (refined, model) = match refined {
            Ok(refined) => refined,
            Err(error) => {
                eprintln!(
                    "[server] Cleaning the whole take for a correction failed; keeping the streamed text: {error}"
                );
                return streamed;
            }
        };
        let mut st = self.lock();
        let (refined, verdict) = content_check::check_refinement(
            &raw,
            &refined,
            &st.inputs.flags,
            &refinement::shown_examples(&st.inputs.personal),
            retractable,
            Some(&st.inputs.punctuation),
        );
        if verdict.outcome == "reject" {
            return streamed;
        }
        st.llm_model = Some(model);
        st.reviews = vec![verdict];
        let text = self.corrected(&st, &self.start_like_raw(&st, &refined));
        let closed = self.close_dictation(&st, &text, true);
        self.express(&st, &closed, true)
    }

    /// Clean the whole take at once: ambiguous spoken corrections, the
    /// full-audio fallback and an edit that wasn't one.
    async fn reconcile_refinement(self: &Arc<Self>) {
        if self.is_command() {
            self.finish_command().await;
            return;
        }
        if !self.settings.auto_refine {
            return;
        }
        let raw = {
            let mut st = self.lock();
            st.cleanup_started = true;
            st.raw.clone()
        };
        let started = Instant::now();
        let refined = self.refine_transcript(&raw, "", None, None).await;
        let mut st = self.lock();
        let (refined, model) = match refined {
            Ok(refined) => refined,
            Err(error) => {
                st.refinement_error = Some(error);
                return;
            }
        };
        Self::spent(&mut st, Stage::Refine, started);
        st.llm_model = Some(model);
        let (refined, verdict) = content_check::check_refinement(
            &raw,
            &refined,
            &st.inputs.flags,
            &refinement::shown_examples(&st.inputs.personal),
            "",
            Some(&st.inputs.punctuation),
        );
        let refined = self.start_like_raw(&st, &refined);
        st.reviews = vec![verdict];
        let corrected = self.corrected(&st, &refined);
        st.refined = self.express(&st, &corrected, true);
        st.refinement_error = None;
        let text = st.refined.clone();
        self.emit(&mut st, "refined", json!({"text": text}));
    }

    /// Rewrite the selection by what was said, or by the transform it names.
    async fn finish_command(self: &Arc<Self>) {
        let rewritten = async {
            let (selection, raw) = {
                let st = self.lock();
                (st.selection.clone(), st.raw.clone())
            };
            let Some(selection) = selection else {
                return Err("Select text to rewrite first".to_string());
            };
            let transforms =
                settings::normalize_transforms(self.settings.command_transforms.clone())
                    .unwrap_or_default();
            let (instruction, transform) = resolve_instruction(&raw, &transforms);
            let started = Instant::now();
            let rewritten = models::command_rewrite(
                &self.app.proxy,
                &selection,
                &instruction,
                &self.settings.llm_model,
            )
            .await;
            Self::spent(&mut self.lock(), Stage::Refine, started);
            let (text, model) = rewritten?;
            Ok((text, model, instruction, transform))
        }
        .await;
        let mut st = self.lock();
        match rewritten {
            Ok((text, model, instruction, transform)) => {
                st.refined = text.clone();
                st.llm_model = Some(model);
                st.command = Some((instruction, transform));
                st.refinement_error = None;
                self.emit(&mut st, "refined", json!({"text": text}));
            }
            Err(error) => {
                eprintln!("[server] Command failed: {error}");
                st.refinement_error = Some(error);
            }
        }
    }

    /// Recognize the whole recording instead: phrases can't be joined
    /// safely.
    async fn reconcile_full_audio(self: &Arc<Self>) -> Result<(), String> {
        let request = {
            let mut st = self.lock();
            st.intake.close_recording()?;
            let samples = st.intake.samples();
            if st.intake.speech().heard(0, samples) {
                Some(RecognizeFile {
                    path: st.intake.path().to_string_lossy().into_owned(),
                    language: self.start.language.clone(),
                    stt_model: Some(self.stt_model.clone()),
                    previous_text: st.continues.then(|| st.field_before.clone()),
                    vocabulary: self.vocabulary(&st),
                    check_speech: false,
                })
            } else {
                None
            }
        };
        let raw = match request {
            Some(request) => {
                let started = Instant::now();
                let recognized = models::recognize_file(&self.app.proxy, &request).await;
                Self::spent(&mut self.lock(), Stage::Recognize, started);
                let raw = py::strip(&recognized?.text).to_string();
                let raw = self.take_spoken_style(raw).await?;
                self.lock().editing = false;
                self.take_edit(&raw);
                let mut st = self.lock();
                if st.editing {
                    raw
                } else {
                    let raw = if st.continues {
                        phrase_seams::continue_phrase(&raw, &st.field_before, &st.names)
                    } else {
                        raw
                    };
                    let _ = &mut st;
                    mark_commands(&raw)
                }
            }
            None => String::new(),
        };
        let editing = {
            let mut st = self.lock();
            if st.abort {
                return Ok(());
            }
            st.raw = raw.clone();
            st.intake.cover_all();
            self.emit(
                &mut st,
                "transcript",
                json!({"accepted_text": raw, "provisional_text": "", "text": raw, "final": false}),
            );
            st.editing
        };
        if editing {
            self.finish_edit().await;
        } else {
            self.reconcile_refinement().await;
        }
        Ok(())
    }

    /// Recognize and clean phrases as they arrive, then finish the take
    /// once released.
    pub async fn run(self: Arc<Self>) -> Result<(), String> {
        self.load_names().await;
        if self.expressive {
            let (baseline, taught) = prosody::load(&self.app.db).await;
            let mut st = self.lock();
            st.baseline = baseline;
            st.taught = taught;
        }
        loop {
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (next, peek) = {
                let mut st = self.lock();
                if st.abort {
                    return Ok(());
                }
                if !st.intake.backlogged() && self.style_peek_due(&st) {
                    (Next::Wait, true)
                } else {
                    let finished = st.finished;
                    (st.intake.next(finished), false)
                }
            };
            if peek {
                self.peek_style().await?;
                continue;
            }
            match next {
                Next::FullAudio => return self.reconcile_full_audio().await,
                Next::Phrase(window) => {
                    if window.last {
                        self.show_cleaned_so_far();
                    }
                    let pcm = self.lock().intake.pcm(&window).to_vec();
                    let text = self.recognize(pcm, window.start, true).await?;
                    {
                        let mut st = self.lock();
                        if st.abort {
                            return Ok(());
                        }
                        st.intake.advance(&window);
                    }
                    self.accept(text, window.paused).await;
                    self.lock().overlap = window.forced;
                }
                Next::Done => {
                    let (editing, needs_final) = {
                        let st = self.lock();
                        (st.editing, st.needs_final)
                    };
                    if self.is_command() {
                        self.finish_command().await;
                    } else if editing {
                        self.finish_edit().await;
                    } else if needs_final {
                        self.reconcile_refinement().await;
                    } else if self.settings.auto_refine {
                        self.finish_cleanup().await;
                    }
                    return Ok(());
                }
                Next::Wait => notified.await,
            }
        }
    }

    /// The user's names, which keep their capitals mid-sentence.
    async fn load_names(&self) {
        let known = self.app.known_names.clone();
        match self.app.db.call(move |conn| known.get(conn)).await {
            Ok(names) => {
                let mut st = self.lock();
                let dictionary = st.dictionary.clone();
                st.names.extend(names.iter().cloned());
                st.names.extend(dictionary.names.iter().cloned());
            }
            Err(error) => eprintln!(
                "[server] Could not read the user's names; names in the word lists still count: {error}"
            ),
        }
    }

    /// The user released: recognize what's left and finish. A cleanup
    /// running for speech that continued after it started is stopped.
    pub fn finish(&self) {
        let mut st = self.lock();
        st.finished = true;
        st.finished_at = Some(Instant::now());
        if let Some(stop) = st.stop.clone() {
            if Self::speech_follows(&st) {
                stop.flag.store(true, Ordering::SeqCst);
                let generating = stop
                    .generating
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if let Some(id) = generating {
                    let proxy = self.app.proxy.clone();
                    tokio::spawn(async move {
                        models::stop(&proxy, &id).await;
                    });
                }
            }
        }
        drop(st);
        self.wake.notify_one();
    }

    /// One log line per dictation: where the time went, no text.
    pub fn timing_summary(&self) -> String {
        let st = self.lock();
        let now = Instant::now();
        let rate = st.intake.rate();
        let release = match st.finished_at {
            Some(at) => format!("{:.2}s", now.saturating_duration_since(at).as_secs_f64()),
            None => "not released".to_string(),
        };
        format!(
            "session={} audio={:.2}s rate={rate} wall={:.2}s release_to_final={release} \
             after_release_recognize={:.2}s after_release_refine={:.2}s \
             after_release_tail_words={} peak_backlog={:.2}s degraded={} refinement_error={}",
            self.id,
            st.intake.samples() as f64 / f64::from(rate),
            now.saturating_duration_since(self.started_at).as_secs_f64(),
            st.after_recognize,
            st.after_refine,
            st.release_tail_words,
            st.intake.peak_backlog(),
            st.intake.degraded_reason().unwrap_or("no"),
            if st.refinement_error.is_some() {
                "yes"
            } else {
                "no"
            },
        )
    }

    // -- saving ---------------------------------------------------------------

    /// The recording's storage path, or "" once it's deleted because the
    /// user keeps none.
    fn stored_audio(&self) -> String {
        if self.settings.discard_audio {
            let path = self.lock().intake.path().to_path_buf();
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "[server] Could not delete the recording {}: {error}",
                        path.display()
                    );
                }
            }
            return String::new();
        }
        format!("captures/{}.wav", self.id)
    }

    /// Save the take as a capture; the response the final event carries.
    pub async fn persist(&self) -> Result<Value, String> {
        self.lock().intake.close_recording()?;
        let audio_path = self.stored_audio();
        let (new, auto_refine, allow_auto_paste) = {
            let st = self.lock();
            let rate = f64::from(st.intake.rate());
            let mut new = NewCapture {
                id: self.id.clone(),
                audio_path,
                source: self.start.source.as_str().to_string(),
                language: self.start.language.clone(),
                duration_ms: Some(
                    (st.intake.samples() as f64 / rate * 1000.0).round_ties_even() as i64,
                ),
                transcript_raw: st.raw.clone(),
                stt_model: Some(self.stt_model.clone()),
                app_bundle_id: st.app_bundle_id.clone(),
                app_name: st.app_name.clone(),
                app_category: st.app_category.clone(),
                ..NewCapture::default()
            };
            if self.is_command() {
                new.command_selection = st.selection.clone();
                if let (Some((instruction, transform)), None) = (&st.command, &st.refinement_error)
                {
                    new.command_instruction = Some(instruction.clone());
                    new.command_transform = transform.as_ref().map(|t| t.name.clone());
                    new.transcript_refined = Some(st.refined.clone());
                    new.llm_model = st.llm_model.clone();
                }
                (new, true, true)
            } else if let Some(edit) = &st.edit {
                new.source = "command".to_string();
                new.command_selection = st.last_take.clone();
                new.command_transform = Some(voice_edits::TRANSFORM_NAME.to_string());
                match edit {
                    Ok(planned) => {
                        new.command_selection = Some(planned.before.clone());
                        new.command_instruction = Some(planned.instruction.clone());
                        new.transcript_refined = Some(planned.after.clone());
                    }
                    Err(declined) => new.command_instruction = Some(declined.message.clone()),
                }
                (new, true, true)
            } else {
                let auto_refine = self.settings.auto_refine;
                if auto_refine && st.refinement_error.is_none() {
                    new.transcript_refined = Some(st.refined.clone());
                }
                new.llm_model = st.llm_model.clone();
                if auto_refine {
                    new.refinement_flags = Some(flags_json(&st.inputs.flags));
                    new.refinement_review = content_check::summarize_reviews(&st.reviews)
                        .map(|summary| review_json(&summary));
                    new.style_id = Some(st.style.id.clone());
                }
                new.teaches_style_id = st.spoken.as_ref().and_then(|spoken| {
                    (spoken.id != self.snapshot.for_app(st.app_bundle_id.as_deref()).id)
                        .then(|| spoken.id.clone())
                });
                (new, auto_refine, self.settings.allow_auto_paste)
            }
        };
        let sync = self.app.sync;
        let capture = self
            .app
            .db
            .call(move |conn| captures::insert_capture(conn, &new, sync))
            .await
            .map_err(|e| e.to_string())?;
        self.lock().persisted = true;
        let mut response = serde_json::to_value(&capture).map_err(|e| e.to_string())?;
        if let Value::Object(fields) = &mut response {
            fields.insert("auto_refine".into(), auto_refine.into());
            fields.insert("allow_auto_paste".into(), allow_auto_paste.into());
        }
        Ok(response)
    }

    /// Delete the recording of a take that wasn't saved.
    pub fn close(&self) {
        let mut st = self.lock();
        let _ = st.intake.close_recording();
        if !st.persisted {
            let _ = std::fs::remove_file(st.intake.path());
        }
    }

    // -- after the final event ----------------------------------------------

    /// Measure how the take was said and save it with its capture, once
    /// every phrase's words are placed.
    pub async fn measure(&self, text: String) {
        let alignment = self.lock().alignment.take();
        if let Some(alignment) = alignment {
            let _ = alignment.await;
        }
        let rate = self.start.sample_rate;
        let expression = std::mem::replace(&mut self.lock().expression, Expression::new(rate));
        prosody::measure_and_save(&self.app.db, &self.id, &expression, &text).await;
    }

    /// What a voice edit teaches: a report on the take it fixed, and a
    /// word spelled for it in the Dictionary.
    pub async fn learn_from_edit(&self) {
        let (planned, capture_id, bundle_id) = {
            let st = self.lock();
            let Some(Ok(planned)) = &st.edit else {
                return;
            };
            let fixed_own = voice_edits::changes_end(planned, st.last_take_own_chars);
            let capture_id = st
                .last_take_capture_id
                .clone()
                .filter(|id| fixed_own && !id.is_empty());
            (planned.clone(), capture_id, st.app_bundle_id.clone())
        };
        if let Some(spelled) = planned.spelled.clone() {
            let (sync, edit_id, replaced) =
                (self.app.sync, self.id.clone(), planned.replaced.clone());
            let added = self
                .app
                .db
                .call(move |conn| {
                    crate::dictionary::store::add_spelled_word(
                        conn,
                        &spelled,
                        bundle_id.as_deref(),
                        Some(&replaced),
                        Some(&edit_id),
                        sync,
                    )
                })
                .await;
            match added {
                Ok(_) => worker::dictionary_changed(&self.app.proxy).await,
                Err(error) => {
                    eprintln!("[server] Couldn't add the spelled word to the Dictionary: {error}")
                }
            }
        }
        if let Some(capture_id) = capture_id {
            if let Err(error) = self.report_fix(&planned, capture_id).await {
                eprintln!("[server] Couldn't file the voice fix as a correction: {error}");
            }
        }
    }

    /// File the edit as a correction on the capture that wrote the text.
    async fn report_fix(&self, planned: &Planned, capture_id: String) -> Result<(), String> {
        let id = capture_id.clone();
        let found = self
            .app
            .db
            .call(move |conn| {
                let Some(capture) = captures::get(conn, &id)? else {
                    return Ok(None);
                };
                let target = if capture.transcript_refined.is_some() {
                    "refined"
                } else {
                    "raw"
                };
                let latest = captures::latest_report(conn, &id, target)?;
                Ok(Some((
                    capture,
                    target,
                    latest.map(|report| report.expected_text),
                )))
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some((capture, target, latest)) = found else {
            return Ok(());
        };
        let output = latest
            .or_else(|| capture.output(target).map(str::to_string))
            .unwrap_or_default();
        let expected = match voice_edits::corrected(&output, &planned.before, &planned.after) {
            Some(expected) if expected != output => expected,
            _ => {
                eprintln!(
                    "[server] Voice fix not found in capture {capture_id}; no correction filed"
                );
                return Ok(());
            }
        };
        let expected = worker::join_spelling(&self.app.proxy, &expected).await?;
        let original = capture.output(target).unwrap_or_default().to_string();
        if expected == original {
            return Ok(());
        }
        let new = NewReport {
            capture_id,
            target: target.to_string(),
            expected_text: expected,
            notes: py::strip(&planned.instruction).to_string(),
            source: "voice_fix".to_string(),
            replaces: None,
            seen: capture,
            filed_by: Some(self.id.clone()),
        };
        let sync = self.app.sync;
        let saved = self
            .app
            .db
            .call(move |conn| {
                let saved = match captures::save_report(conn, new, sync)? {
                    Ok(saved) => saved,
                    Err(refused) => return Ok(Err(refused.message())),
                };
                if let Some(replaced) = &saved.replaced {
                    crate::dictionary::store::delete_added_by(conn, &replaced.id, sync)?;
                }
                Ok(Ok(saved))
            })
            .await
            .map_err(|e| e.to_string())??;
        self.app.events.publish(Kind::Learned);
        let row = &saved.report;
        let learn = [worker::Learn {
            report_id: &row.id,
            source: &row.source,
            original: &original,
            expected: &row.expected_text,
        }];
        let changed = [(row.target.clone(), row.source.clone())];
        worker::reports_changed(
            &self.app.proxy,
            &learn,
            &changed,
            saved.replaced.is_some(),
            None,
        )
        .await;
        learned::reports_changed(&self.app, &changed).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_keeps_matching_words_once() {
        assert_eq!(
            join_overlap("one two three", "Three, four"),
            "one two three four"
        );
        assert_eq!(join_overlap("one two", "five six"), "one two five six");
        assert_eq!(join_overlap("", "five"), "five");
        assert_eq!(join_overlap("one", ""), "one");
    }

    #[test]
    fn stable_prefix_holds_back_words_and_their_punctuation() {
        assert_eq!(stable_prefix("Hello there, my friend.", 2), "Hello there");
        assert_eq!(stable_prefix("Hello there.", 0), "Hello there");
        assert_eq!(stable_prefix("Hello there", 2), "");
        assert_eq!(stable_prefix("Hello", 2), "");
    }

    #[test]
    fn python_head_takes_negative_bounds() {
        assert_eq!(python_head("abcdef", -2), "abcd");
        assert_eq!(python_head("abcdef", 0), "");
        assert_eq!(python_head("abcdef", 3), "abc");
    }

    fn transform(name: &str, instruction: &str) -> Transform {
        Transform {
            id: None,
            name: name.to_string(),
            instruction: instruction.to_string(),
        }
    }

    #[test]
    fn instructions_name_transforms() {
        let transforms = [
            transform("Polish", "Polish the text."),
            transform("Make it formal", "Formal."),
        ];
        let (instruction, named) = resolve_instruction("ok polish this please", &transforms);
        assert_eq!(instruction, "Polish the text.");
        assert_eq!(named.unwrap().name, "Polish");
        let (instruction, named) = resolve_instruction("make it formal", &transforms);
        assert_eq!(instruction, "Formal.");
        assert!(named.is_some());
        let (instruction, named) = resolve_instruction("  translate to French ", &transforms);
        assert_eq!(instruction, "translate to French");
        assert!(named.is_none());
    }

    #[test]
    fn saved_flags_and_reviews_read_as_python_wrote_them() {
        let flags = Flags {
            punctuation: Punctuation::Casual,
            style: Some("casual".into()),
            ..Flags::default()
        };
        assert_eq!(
            flags_json(&flags),
            "{\"smart_cleanup\": true, \"self_correction\": true, \"preserve_technical\": true, \
             \"punctuation_style\": \"casual\", \"style\": \"casual\"}"
        );
        let summary = Summary {
            outcome: "flag".into(),
            added: vec!["caf\u{e9}".into()],
            missing: vec![],
            reasons: vec!["added words".into()],
        };
        assert_eq!(
            review_json(&summary),
            "{\"outcome\": \"flag\", \"added\": [\"caf\\u00e9\"], \"missing\": [], \"reasons\": [\"added words\"]}"
        );
    }
}
