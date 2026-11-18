//! Saving an uploaded recording, cleaning a capture up again and
//! transcribing it again: the capture routes that run the models.
//! Python: `create_capture_endpoint`, `refine_capture_endpoint` and
//! `retranscribe_capture_endpoint` in `backend/routes/captures.py`, and
//! their halves in `backend/services/captures.py`.
//!
//! The text rules and the cleanup prompt are the ones dictation uses; the
//! worker only recognizes and generates. A capture being refined or
//! retranscribed is held (`captures::in_use`) so the history sweep can't
//! delete it halfway.

use std::path::{Path, PathBuf};

use axum::body::Bytes;
use axum::extract::multipart::{Multipart, MultipartRejection};
use axum::extract::{Path as UrlPath, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use kass_text::content_check;
use kass_text::py;
use kass_text::refinement::{self, Flags};
use kass_text::voice_commands::{commands_alone, mark_commands};
use serde_json::{json, Map, Value};

use super::captures::{body, capture_not_found, detail, invalid};
use crate::captures::store::{self, NewCapture, Refined, Retranscribed};
use crate::captures::{self, in_use, optional_string, resolve_storage_path, Capture, Problems};
use crate::cleanup_inputs::{self, CleanupInputs};
use crate::events::{mac_of, Kind};
use crate::settings::{self, Settings};
use crate::stream::models::{self, Generate, RecognizeFile};
use crate::stream::session::{cleanup_prompt, flags_json, review_json};
use crate::styles::{self, Snapshot};
use crate::AppState;

/// `VALID_SOURCES`, sorted as Python lists them in its error.
const SOURCES: [&str; 4] = ["command", "dictation", "file", "recording"];
/// The file types an upload is kept as; anything else is saved as `.wav`.
const SUFFIXES: [&str; 6] = [".wav", ".mp3", ".m4a", ".flac", ".ogg", ".webm"];
/// The types Whisper reads itself when they can't be converted first.
const WHISPER_NATIVE: [&str; 4] = [".wav", ".mp3", ".flac", ".ogg"];
/// The rate a converted upload is saved at (`load_audio`'s default).
const CONVERTED_RATE: u32 = 24_000;
/// `MAX_APP_FIELD_CHARS`.
const MAX_APP_FIELD_CHARS: usize = 255;

const LLM_SIZES: [&str; 3] = ["0.6B", "1.7B", "4B"];
const STT_MODELS: [&str; 5] = ["base", "small", "medium", "large", "turbo"];
const LANGUAGES: [&str; 10] = ["en", "zh", "ja", "ko", "de", "fr", "ru", "pt", "es", "it"];

/// Why a request failed, answered as the worker answered it.
enum Failure {
    BadRequest(String),
    NotFound,
    Gone(String),
    Failed(String),
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Failure::BadRequest(message) => detail(StatusCode::BAD_REQUEST, &message),
            Failure::NotFound => capture_not_found(),
            Failure::Gone(message) => detail(StatusCode::GONE, &message),
            Failure::Failed(message) => {
                eprintln!("[server] captures: {message}");
                detail(StatusCode::INTERNAL_SERVER_ERROR, &message)
            }
        }
    }
}

impl From<crate::db::DbError> for Failure {
    fn from(error: crate::db::DbError) -> Self {
        Failure::Failed(error.to_string())
    }
}

impl From<String> for Failure {
    fn from(error: String) -> Self {
        Failure::Failed(error)
    }
}

fn answer(result: Result<Response, Failure>) -> Response {
    result.unwrap_or_else(IntoResponse::into_response)
}

fn data_dir(state: &AppState) -> PathBuf {
    state
        .db
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// The saved settings and the styles as they stand.
async fn settings_and_styles(state: &AppState) -> Result<(Settings, Snapshot), Failure> {
    let teaching = state.teach.teaching_style();
    Ok(state
        .db
        .call(move |conn| Ok((settings::get(conn)?, styles::load(conn, teaching)?)))
        .await?)
}

/// The language to recognize in: the one asked for, else the saved one;
/// "auto" lets Whisper tell.
fn language(asked: Option<String>, settings: &Settings) -> Option<String> {
    asked
        .or_else(|| Some(settings.language.clone()))
        .filter(|language| language != "auto")
}

/// `transcript` without an opening request for a style ("use formal
/// mode"), and the style it asked for when that isn't the app's own.
fn spoken_style(
    transcript: &str,
    styles: &Snapshot,
    bundle_id: Option<&str>,
) -> (String, Option<String>) {
    let (style, rest) = styles::spoken_style(transcript, styles);
    let teaches = style
        .filter(|style| style.id != styles.for_app(bundle_id).id)
        .map(|style| style.id.clone());
    (rest, teaches)
}

/// What Whisper heard in `path`, the user's words for this app helping it.
async fn recognize(
    state: &AppState,
    path: &Path,
    language: Option<String>,
    stt_model: &str,
    bundle_id: Option<String>,
) -> Result<String, Failure> {
    let dictionaries = state.dictionaries.clone();
    let dictionary = state
        .db
        .call(move |conn| dictionaries.for_app(conn, bundle_id.as_deref(), None))
        .await?;
    let request = RecognizeFile {
        path: path.to_string_lossy().into_owned(),
        language,
        stt_model: Some(stt_model.to_string()),
        previous_text: None,
        vocabulary: dictionary.terms.clone(),
        check_speech: true,
    };
    let recognized = models::recognize_file(&state.proxy, &request).await?;
    Ok(py::strip(&recognized.text).to_string())
}

/// A capture as the worker returned it.
fn capture_json(capture: &Capture) -> Result<Value, Failure> {
    serde_json::to_value(capture).map_err(|error| Failure::Failed(error.to_string()))
}

// -- uploads ------------------------------------------------------------------

/// An uploaded recording and the form fields sent with it.
#[derive(Default)]
struct Upload {
    audio: Option<(Bytes, Option<String>)>,
    source: Option<String>,
    language: Option<String>,
    stt_model: Option<String>,
    app_bundle_id: Option<String>,
    app_name: Option<String>,
}

/// The upload's fields, or the response refusing it (boxed: a `Response` is large).
async fn read_upload(
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Upload, Box<Response>> {
    let mut upload = Upload::default();
    if let Ok(mut multipart) = multipart {
        loop {
            let field = match multipart.next_field().await {
                Ok(Some(field)) => field,
                Ok(None) => break,
                Err(error) => {
                    return Err(Box::new(detail(
                        StatusCode::BAD_REQUEST,
                        &error.body_text(),
                    )));
                }
            };
            let name = field.name().unwrap_or("").to_string();
            let filename = field.file_name().map(String::from);
            let bytes = field
                .bytes()
                .await
                .map_err(|error| Box::new(detail(StatusCode::BAD_REQUEST, &error.body_text())))?;
            let text = || String::from_utf8_lossy(&bytes).into_owned();
            // The last of a repeated field counts, as in Starlette's form.
            match name.as_str() {
                "file" => upload.audio = Some((bytes.clone(), filename)),
                "source" => upload.source = Some(text()),
                "language" => upload.language = Some(text()),
                "stt_model" => upload.stt_model = Some(text()),
                "app_bundle_id" => upload.app_bundle_id = Some(text()),
                "app_name" => upload.app_name = Some(text()),
                _ => {}
            }
        }
    }
    if upload.audio.is_none() {
        let mut problems = Problems::default();
        problems.add(
            "missing",
            &[json!("body"), json!("file")],
            "Field required",
            &Value::Null,
        );
        return Err(Box::new(invalid(problems)));
    }
    Ok(upload)
}

/// An app field as stored: trimmed, cut short, blank as none.
fn app_field(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed: String = py::strip(&value)
        .chars()
        .take(MAX_APP_FIELD_CHARS)
        .collect();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// The extension an upload is saved with.
fn suffix(filename: &str) -> &'static str {
    let name = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    // Python's `Path.suffix`: a leading dot alone is no extension.
    let suffix = match name.rfind('.') {
        Some(at) if at > 0 && at + 1 < name.len() => name[at..].to_lowercase(),
        _ => return ".wav",
    };
    SUFFIXES
        .iter()
        .find(|known| **known == suffix)
        .copied()
        .unwrap_or(".wav")
}

/// A recording ready for Whisper: where it is and how long it plays.
struct Prepared {
    path: PathBuf,
    duration_ms: Option<i64>,
}

/// Make the saved upload something Whisper reads: a WAV is used as it is,
/// anything else is converted to one. `written` is every file left on disk.
fn prepare(raw: &Path, suffix: &str, written: &mut Vec<PathBuf>) -> Result<Prepared, Failure> {
    if suffix == ".wav" {
        // A WAV is timed from its header; one that can't be read still goes
        // to Whisper, which may manage.
        let duration_ms = hound::WavReader::open(raw).ok().map(|reader| {
            let spec = reader.spec();
            let seconds = f64::from(reader.duration()) / f64::from(spec.sample_rate);
            (seconds * 1000.0).round_ties_even() as i64
        });
        return Ok(Prepared {
            path: raw.to_path_buf(),
            duration_ms,
        });
    }
    let converted = raw.with_extension("wav");
    match convert(raw, &converted) {
        Ok(frames) => {
            written.push(converted.clone());
            if std::fs::remove_file(raw).is_ok() {
                written.retain(|path| path != raw);
            }
            Ok(Prepared {
                path: converted,
                duration_ms: Some((frames * 1000 / u64::from(CONVERTED_RATE)) as i64),
            })
        }
        Err(error) => {
            eprintln!(
                "[server] Could not decode {} ({suffix}): {error}",
                raw.display()
            );
            let _ = std::fs::remove_file(&converted);
            if !WHISPER_NATIVE.contains(&suffix) {
                return Err(Failure::BadRequest(format!(
                    "Could not decode {suffix} audio — the recording may be empty or corrupt"
                )));
            }
            Ok(Prepared {
                path: raw.to_path_buf(),
                duration_ms: None,
            })
        }
    }
}

/// Convert `from` to 16-bit mono WAV at `to` with macOS's own converter;
/// the frames it holds.
fn convert(from: &Path, to: &Path) -> Result<u64, String> {
    let output = std::process::Command::new("/usr/bin/afconvert")
        .args([
            "-f",
            "WAVE",
            "-d",
            &format!("LEI16@{CONVERTED_RATE}"),
            "-c",
            "1",
        ])
        .arg(from)
        .arg(to)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let reader = hound::WavReader::open(to).map_err(|error| error.to_string())?;
    match reader.duration() {
        0 => Err("no audio in it".into()),
        frames => Ok(u64::from(frames)),
    }
}

/// Upload audio, transcribe it and save the capture.
pub(super) async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    multipart: Result<Multipart, MultipartRejection>,
) -> Response {
    let upload = match read_upload(multipart).await {
        Ok(upload) => upload,
        Err(response) => return *response,
    };
    answer(save_upload(&state, &headers, query.as_deref(), upload).await)
}

async fn save_upload(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
    upload: Upload,
) -> Result<Response, Failure> {
    let (audio, filename) = upload.audio.unwrap_or_default();
    if audio.is_empty() {
        return Err(Failure::BadRequest("Uploaded file is empty".into()));
    }
    let (settings, snapshot) = settings_and_styles(state).await?;
    let stt_model = upload
        .stt_model
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| settings.stt_model.clone());
    let language = language(upload.language, &settings);
    let bundle_id = app_field(upload.app_bundle_id);
    let app_name = app_field(upload.app_name);
    let source = upload.source.unwrap_or_else(|| "file".into());
    if !SOURCES.contains(&source.as_str()) {
        let listed: Vec<String> = SOURCES.iter().map(|s| format!("'{s}'")).collect();
        return Err(Failure::BadRequest(format!(
            "Invalid source '{source}'. Must be one of [{}]",
            listed.join(", ")
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let suffix = suffix(filename.as_deref().unwrap_or("capture.wav"));
    let captures_dir = data_dir(state).join("captures");
    let raw = captures_dir.join(format!("{id}{suffix}"));
    let mut written: Vec<PathBuf> = Vec::new();
    let saved = async {
        std::fs::create_dir_all(&captures_dir).map_err(|e| Failure::Failed(e.to_string()))?;
        std::fs::write(&raw, &audio).map_err(|e| Failure::Failed(e.to_string()))?;
        written.push(raw.clone());
        let (raw_path, mut files) = (raw.clone(), written.clone());
        let (prepared, files) = tokio::task::spawn_blocking(move || {
            let prepared = prepare(&raw_path, suffix, &mut files);
            (prepared, files)
        })
        .await
        .map_err(|e| Failure::Failed(e.to_string()))?;
        written = files;
        let prepared = prepared?;

        let stt_model = models::speech_model(&state.proxy, &stt_model).await?;
        let heard = recognize(
            state,
            &prepared.path,
            language.clone(),
            &stt_model,
            bundle_id.clone(),
        )
        .await?;
        let (transcript, teaches) = spoken_style(&heard, &snapshot, bundle_id.as_deref());
        let transcript = mark_commands(&transcript);

        let audio_path = if settings.discard_audio {
            // Only needed to get the transcript.
            if let Err(error) = std::fs::remove_file(&prepared.path) {
                eprintln!(
                    "[server] Could not delete the recording {}: {error}",
                    prepared.path.display()
                );
            }
            written.retain(|path| path != &prepared.path);
            String::new()
        } else {
            let name = prepared
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            format!("captures/{name}")
        };
        let mac = mac_of(headers, query);
        let new = NewCapture {
            id: id.clone(),
            audio_path,
            source: source.clone(),
            language: language.clone(),
            duration_ms: prepared.duration_ms,
            transcript_raw: transcript,
            stt_model: Some(stt_model),
            app_bundle_id: bundle_id.clone(),
            app_name: app_name.clone(),
            teaches_style_id: teaches,
            mac_id: mac.as_ref().map(|mac| mac.id.clone()),
            mac_name: mac.and_then(|mac| mac.name),
            ..NewCapture::default()
        };
        let sync = state.sync;
        let capture = state
            .db
            .call(move |conn| store::insert_capture(conn, &new, sync))
            .await?;
        Ok::<_, Failure>(capture)
    }
    .await;
    let capture = match saved {
        Ok(capture) => capture,
        Err(failure) => {
            // Audio on disk with no capture pointing at it would never go.
            for path in &written {
                let _ = std::fs::remove_file(path);
            }
            return Err(failure);
        }
    };
    state.events.publish(Kind::Changed);

    // A command recording always goes on to its rewrite (POST /commands/run),
    // which replaces the selection whatever the paste setting says.
    let command = source == "command";
    let mut response = capture_json(&capture)?;
    if let Value::Object(fields) = &mut response {
        fields.insert(
            "auto_refine".into(),
            (command || settings.auto_refine).into(),
        );
        fields.insert(
            "allow_auto_paste".into(),
            (command || settings.allow_auto_paste).into(),
        );
    }
    Ok(Json(response).into_response())
}

// -- refining -----------------------------------------------------------------

/// A body field that must be one of `allowed` (a pydantic `pattern`).
fn one_of(
    fields: &Map<String, Value>,
    name: &str,
    allowed: &[&str],
    problems: &mut Problems,
) -> Option<String> {
    let value = fields.get(name).unwrap_or(&Value::Null);
    let loc = [json!("body"), json!(name)];
    let chosen = optional_string(value, &loc, problems).flatten()?;
    if allowed.contains(&chosen.as_str()) {
        return Some(chosen);
    }
    let pattern = format!("^({})$", allowed.join("|").replace('.', "\\."));
    problems.add_ctx(
        "string_pattern_mismatch",
        &loc,
        format!("String should match pattern '{pattern}'"),
        value,
        json!({ "pattern": pattern }),
    );
    None
}

/// `CaptureRefineRequest`: the flags to clean up with, if not the style's,
/// and the model, if not the saved one.
fn refine_request(bytes: &Bytes) -> Result<(Option<Flags>, Option<String>), Problems> {
    let fields = body(bytes)?;
    let mut problems = Problems::default();
    let flags = captures::optional_flags(
        fields.get("flags").unwrap_or(&Value::Null),
        &[json!("body"), json!("flags")],
        &mut problems,
    );
    let model_size = one_of(&fields, "model_size", &LLM_SIZES, &mut problems);
    if !problems.0.is_empty() {
        return Err(problems);
    }
    let flags = flags
        .flatten()
        .map(|flags| Flags::from_json(&serde_json::to_value(&flags).unwrap_or_default()));
    Ok((flags, model_size))
}

/// The cleanup of `transcript`, line by line, as dictation cleans a take.
async fn clean_up(
    state: &AppState,
    inputs: &CleanupInputs,
    model: &str,
    transcript: &str,
) -> Result<String, Failure> {
    if let Some(alone) = commands_alone(transcript) {
        // Carried out by the client, not typed: nothing to clean.
        return Ok(alone);
    }
    let flags = &inputs.flags;
    let (cleaned, explicit) =
        refinement::prepare_refinement(transcript, flags, Some(&inputs.punctuation));
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    let prompt = cleanup_prompt(inputs, model);
    let mut done = String::new();
    for (line, breaks) in refinement::lines(&cleaned) {
        if !py::strip(line).is_empty() {
            let request = Generate {
                id: uuid::Uuid::new_v4().to_string(),
                prompt: refinement::without_final_period(line),
                hint: Some(String::new()),
                max_tokens: 2048,
                temperature: 0.2,
                cleanup: prompt.clone(),
            };
            let generated = models::generate(&state.proxy, &request, |_| {}).await?;
            done.push_str(&refinement::finish_line(&generated.text));
        }
        done.push_str(breaks);
    }
    Ok(refinement::finish(
        transcript,
        &done,
        flags,
        &inputs.profile,
        Some(&inputs.punctuation),
    ))
}

/// Clean a capture's transcript up again, in its style or with the flags
/// given.
pub(super) async fn refine(
    State(state): State<AppState>,
    UrlPath(capture_id): UrlPath<String>,
    bytes: Bytes,
) -> Response {
    let (flags, model_size) = match refine_request(&bytes) {
        Ok(request) => request,
        Err(problems) => return invalid(problems),
    };
    let _held = in_use::hold(&state.in_use, &capture_id);
    answer(refine_capture(&state, &capture_id, flags, model_size).await)
}

async fn refine_capture(
    state: &AppState,
    capture_id: &str,
    flags: Option<Flags>,
    model_size: Option<String>,
) -> Result<Response, Failure> {
    let (settings, snapshot) = settings_and_styles(state).await?;
    let (dir, id, dictionaries) = (
        data_dir(state),
        capture_id.to_string(),
        state.dictionaries.clone(),
    );
    let self_correction = settings.self_correction;
    let loaded = state
        .db
        .call(move |conn| {
            let Some(capture) = store::get(conn, &id)? else {
                return Ok(None);
            };
            let style_id = match &flags {
                Some(flags) => flags.style.clone(),
                // Cleaned up in the style its corrections teach: the one
                // asked for by name ("use formal mode"), else the app's.
                None => {
                    let teaches = store::teaches_style_id(conn, &id)?;
                    let bundle = capture.app_bundle_id.as_deref();
                    Some(
                        snapshot
                            .correction_style(bundle, teaches.as_deref())
                            .to_string(),
                    )
                }
            };
            let mut inputs =
                cleanup_inputs::load(conn, &dir, &snapshot, style_id.as_deref(), self_correction)?;
            if let Some(flags) = flags {
                inputs.flags = flags;
            }
            let dictionary = dictionaries.for_app(
                conn,
                capture.app_bundle_id.as_deref(),
                inputs.flags.style.as_deref(),
            )?;
            Ok(Some((capture, inputs, dictionary)))
        })
        .await?;
    let Some((capture, inputs, dictionary)) = loaded else {
        return Err(Failure::NotFound);
    };
    let model = model_size.unwrap_or_else(|| settings.llm_model.clone());
    let raw = capture.transcript_raw.clone();
    let flags = &inputs.flags;

    let refined = clean_up(state, &inputs, &model, &raw).await?;
    let (refined, verdict) = content_check::check_refinement(
        &raw,
        &refined,
        flags,
        &refinement::shown_examples(&inputs.personal),
        "",
        Some(&inputs.punctuation),
    );
    // The user's own dictionary entries win over learned corrections.
    let corrected = inputs
        .rules
        .apply_learned_corrections(&refined, capture.language.as_deref());
    let refined =
        refinement::style_first_word(&dictionary.apply(&corrected), flags, &dictionary.names);

    let update = Refined {
        transcript_refined: refined,
        style_id: flags.style.clone(),
        refinement_review: content_check::summarize_reviews(&[verdict]).map(|s| review_json(&s)),
        llm_model: model,
        refinement_flags: flags_json(flags),
    };
    let (id, sync) = (capture_id.to_string(), state.sync);
    let saved = state
        .db
        .call(move |conn| store::save_refined(conn, &id, &update, sync))
        .await?;
    let capture = saved.ok_or(Failure::NotFound)?;
    state.events.publish(Kind::Changed);
    Ok(Json(capture_json(&capture)?).into_response())
}

// -- retranscribing -----------------------------------------------------------

/// `CaptureRetranscribeRequest`: the Whisper model and language, if not
/// the saved ones.
fn retranscribe_request(bytes: &Bytes) -> Result<(Option<String>, Option<String>), Problems> {
    let fields = body(bytes)?;
    let mut problems = Problems::default();
    let model = one_of(&fields, "model", &STT_MODELS, &mut problems);
    let language = one_of(&fields, "language", &LANGUAGES, &mut problems);
    if !problems.0.is_empty() {
        return Err(problems);
    }
    Ok((model, language))
}

/// Transcribe a capture's audio again. Its cleanup no longer matches, so
/// it's cleared for the user to refine again.
pub(super) async fn retranscribe(
    State(state): State<AppState>,
    UrlPath(capture_id): UrlPath<String>,
    bytes: Bytes,
) -> Response {
    let (model, language) = match retranscribe_request(&bytes) {
        Ok(request) => request,
        Err(problems) => return invalid(problems),
    };
    let _held = in_use::hold(&state.in_use, &capture_id);
    answer(retranscribe_capture(&state, &capture_id, model, language).await)
}

async fn retranscribe_capture(
    state: &AppState,
    capture_id: &str,
    model: Option<String>,
    language: Option<String>,
) -> Result<Response, Failure> {
    let (settings, snapshot) = settings_and_styles(state).await?;
    let stt_model = model.unwrap_or_else(|| settings.stt_model.clone());
    let language = self::language(language, &settings);
    let id = capture_id.to_string();
    let capture = state
        .db
        .call(move |conn| store::get(conn, &id))
        .await?
        .ok_or(Failure::NotFound)?;
    let audio = resolve_storage_path(&data_dir(state), &capture.audio_path)
        .filter(|path| path.exists())
        .ok_or_else(|| Failure::Gone(format!("Audio for capture {capture_id} is missing")))?;

    let bundle_id = capture.app_bundle_id.clone();
    let heard = recognize(
        state,
        &audio,
        language.clone(),
        &stt_model,
        bundle_id.clone(),
    )
    .await?;
    let (transcript, teaches) = spoken_style(&heard, &snapshot, bundle_id.as_deref());
    let update = Retranscribed {
        transcript_raw: mark_commands(&transcript),
        teaches_style_id: teaches,
        stt_model,
        language: language.filter(|language| !language.is_empty()),
    };
    let (id, sync) = (capture_id.to_string(), state.sync);
    let saved = state
        .db
        .call(move |conn| store::save_retranscribed(conn, &id, &update, sync))
        .await?;
    let capture = saved.ok_or(Failure::NotFound)?;
    state.events.publish(Kind::Changed);
    Ok(Json(capture_json(&capture)?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_keep_a_known_extension_else_become_wav() {
        assert_eq!(suffix("memo.M4A"), ".m4a");
        assert_eq!(suffix("dir.d/take.ogg"), ".ogg");
        assert_eq!(suffix("notes.txt"), ".wav");
        assert_eq!(suffix(".mp3"), ".wav");
        assert_eq!(suffix("capture"), ".wav");
    }

    #[test]
    fn app_fields_are_trimmed_cut_and_blank_is_none() {
        assert_eq!(
            app_field(Some("  com.app  ".into())).as_deref(),
            Some("com.app")
        );
        assert_eq!(app_field(Some("   ".into())), None);
        assert_eq!(app_field(Some("x".repeat(300))).map(|s| s.len()), Some(255));
        assert_eq!(app_field(None), None);
    }

    #[test]
    fn a_pattern_mismatch_names_the_pattern_as_pydantic_does() {
        let mut problems = Problems::default();
        let fields = serde_json::from_value(json!({ "model_size": "8B" })).unwrap();
        assert_eq!(
            one_of(&fields, "model_size", &LLM_SIZES, &mut problems),
            None
        );
        assert_eq!(
            problems.0[0]["msg"],
            json!("String should match pattern '^(0\\.6B|1\\.7B|4B)$'")
        );
    }
}
