//! One-shot transcription of an uploaded recording. Python:
//! `backend/routes/transcription.py`.
//!
//! The server reads the upload into a temporary file and picks the model;
//! the worker decodes the file (any format librosa reads), measures it and
//! runs Whisper (`/worker/recognize/file` with `decode`).

use std::io::Write;
use std::path::PathBuf;

use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

use super::request::{detail, Problems};
use crate::settings;
use crate::stream::models;
use crate::AppState;

pub fn router() -> Router<AppState> {
    // Recordings can be long; Python read uploads of any size.
    Router::new().route(
        "/transcribe",
        post(transcribe).layer(DefaultBodyLimit::disable()),
    )
}

/// Formats kept as uploaded, since the decoder goes by the extension;
/// anything else is read as a WAV.
const AUDIO_EXTENSIONS: [&str; 8] = [
    ".wav", ".mp3", ".m4a", ".ogg", ".flac", ".aac", ".webm", ".opus",
];

/// The Whisper sizes, in the order Python lists them.
const WHISPER_SIZES: [&str; 5] = ["base", "small", "medium", "large", "turbo"];

/// Python's `Path(name).suffix.lower()`.
fn suffix(file_name: &str) -> String {
    let name = file_name.rsplit('/').next().unwrap_or_default();
    match name.rfind('.') {
        Some(at) if at > 0 && at + 1 < name.len() => name[at..].to_lowercase(),
        _ => String::new(),
    }
}

/// The extension the upload is saved with.
fn saved_suffix(file_name: Option<&str>) -> &'static str {
    let uploaded = suffix(file_name.unwrap_or_default());
    AUDIO_EXTENSIONS
        .into_iter()
        .find(|ext| *ext == uploaded)
        .unwrap_or(".wav")
}

/// An uploaded file, removed when dropped.
struct Upload(PathBuf);

impl Upload {
    fn create(suffix: &str) -> std::io::Result<(Self, std::fs::File)> {
        let name = format!("kass-transcribe-{}{suffix}", uuid::Uuid::new_v4());
        let path = std::env::temp_dir().join(name);
        let file = std::fs::File::create(&path)?;
        Ok((Upload(path), file))
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The form: `file`, and the optional `language` and `model`.
struct Form {
    file: Option<Upload>,
    language: Option<String>,
    model: Option<String>,
}

enum FormError {
    Problems(Problems),
    Response(Box<Response>),
}

/// Read the form the way FastAPI does: the last of a repeated field wins,
/// and an empty text field counts as not sent.
async fn read_form(request: Request) -> Result<Form, FormError> {
    let mut form = Form {
        file: None,
        language: None,
        model: None,
    };
    let mut file_sent_as_text = None;
    let multipart = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"));
    if multipart {
        if let Ok(mut multipart) = Multipart::from_request(request, &()).await {
            read_fields(&mut multipart, &mut form, &mut file_sent_as_text).await?;
        }
    } else {
        // A body that isn't a multipart form has none of the fields. It's
        // read anyway: closing with it unread can reset the connection
        // before the client sees the answer.
        let _ = axum::body::to_bytes(request.into_body(), usize::MAX).await;
    }
    if form.file.is_none() {
        let mut problems = Problems::default();
        match file_sent_as_text {
            Some(text) => problems.add_ctx(
                "value_error",
                &[json!("file")],
                "Value error, Expected UploadFile, received: <class 'str'>",
                &json!(text),
                json!({ "error": {} }),
            ),
            None => problems.add("missing", &[json!("file")], "Field required", &json!(null)),
        }
        return Err(FormError::Problems(problems));
    }
    Ok(form)
}

/// Fill `form` from the multipart fields; the upload streams to a file.
async fn read_fields(
    multipart: &mut Multipart,
    form: &mut Form,
    file_sent_as_text: &mut Option<String>,
) -> Result<(), FormError> {
    let unreadable = |error: &dyn std::fmt::Display| {
        eprintln!("[server] transcribe: couldn't read the form: {error}");
        let message = "There was an error parsing the body";
        FormError::Response(Box::new(detail(StatusCode::BAD_REQUEST, message)))
    };
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => return Err(unreadable(&error)),
        };
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(str::to_string);
        if file_name.is_some() {
            if name != "file" {
                continue;
            }
            let (upload, mut file) = Upload::create(saved_suffix(file_name.as_deref()))
                .map_err(|e| FormError::Response(Box::new(failed(e))))?;
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => file
                        .write_all(&chunk)
                        .map_err(|e| FormError::Response(Box::new(failed(e))))?,
                    Ok(None) => break,
                    Err(error) => return Err(unreadable(&error)),
                }
            }
            form.file = Some(upload);
            *file_sent_as_text = None;
            continue;
        }
        let text = match field.text().await {
            Ok(text) => text,
            Err(error) => return Err(unreadable(&error)),
        };
        let value = (!text.is_empty()).then_some(text);
        match name.as_str() {
            "language" => form.language = value,
            "model" => form.model = value,
            "file" => {
                form.file = None;
                *file_sent_as_text = value;
            }
            _ => {}
        }
    }
    Ok(())
}

fn failed(error: impl std::fmt::Display) -> Response {
    detail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
}

/// Transcribe an audio file to text.
async fn transcribe(State(state): State<AppState>, request: Request) -> Response {
    let form = match read_form(request).await {
        Ok(form) => form,
        Err(FormError::Problems(problems)) => return problems.response(),
        Err(FormError::Response(response)) => return *response,
    };
    let upload = form.file.expect("read_form returns the file");
    let model_size = match form.model {
        Some(model) => model,
        None => match state.db.call(settings::get).await {
            Ok(saved) => saved.stt_model,
            Err(error) => return failed(error),
        },
    };
    if !WHISPER_SIZES.contains(&model_size.as_str()) {
        let message = format!(
            "Invalid model size '{model_size}'. Must be one of: {}",
            WHISPER_SIZES.join(", ")
        );
        return detail(StatusCode::BAD_REQUEST, &message);
    }
    let path = upload.0.to_string_lossy().into_owned();
    let language = form.language.as_deref();
    match models::transcribe_upload(&state.proxy, &path, language, &model_size).await {
        Ok((text, duration)) => Json(json!({ "text": text, "duration": duration })).into_response(),
        Err(failure) => failed(failure.detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_like_pathlib() {
        assert_eq!(suffix("take.WEBM"), ".webm");
        assert_eq!(suffix("dir.d/take.tar.ogg"), ".ogg");
        assert_eq!(suffix(".wav"), "");
        assert_eq!(suffix("take."), "");
        assert_eq!(suffix("take"), "");
        assert_eq!(saved_suffix(Some("a.M4A")), ".m4a");
        assert_eq!(saved_suffix(Some("a.mp4")), ".wav");
        assert_eq!(saved_suffix(None), ".wav");
    }
}
