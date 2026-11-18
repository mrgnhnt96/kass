//! `POST /llm/generate` and `POST /transcribe` in front of a stand-in
//! worker. Python: `backend/routes/llm.py`, `backend/routes/transcription.py`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Default)]
struct Worker {
    /// The bodies the worker was sent, in order.
    calls: Arc<Mutex<Vec<Value>>>,
    /// What each uploaded file held when the worker read it.
    uploads: Arc<Mutex<Vec<Vec<u8>>>>,
}

/// Streams a partial, then the text: the prompt in capitals. "break"
/// answers the worker's error line.
async fn generate(State(worker): State<Worker>, Json(body): Json<Value>) -> Response {
    worker.calls.lock().unwrap().push(body.clone());
    let prompt = body["prompt"].as_str().unwrap();
    if prompt == "break" {
        return Body::from("{\"error\": \"/Users/x/model.safetensors: out of memory\"}\n")
            .into_response();
    }
    let text = prompt.to_uppercase();
    let model = format!(
        "qwen3-{}",
        body["model_size"].as_str().unwrap().to_lowercase()
    );
    let lines = format!(
        "{}\n{}\n",
        json!({ "partial": &text[..1] }),
        json!({ "text": text, "model": model, "stopped": false, "seconds": 0.2 })
    );
    Body::from(lines).into_response()
}

/// "the file's bytes" as text, and its length in seconds as its byte count.
async fn recognize_file(State(worker): State<Worker>, Json(body): Json<Value>) -> Response {
    worker.calls.lock().unwrap().push(body.clone());
    let path = body["path"].as_str().unwrap();
    let bytes = std::fs::read(path).unwrap();
    worker.uploads.lock().unwrap().push(bytes.clone());
    if bytes == b"garbage" {
        let detail = "unsupported file format";
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "detail": detail })),
        )
            .into_response();
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Json(json!({ "text": text, "seconds": 0.1, "duration": bytes.len() as f64 })).into_response()
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/cleanup/generate", post(generate))
        .route("/worker/recognize/file", post(recognize_file))
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

struct Kass {
    server: kass_server::Running,
    worker: Worker,
    data: PathBuf,
}

const BOUNDARY: &str = "kass-boundary";

/// A multipart part: name, file name, contents.
type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

impl Kass {
    async fn start() -> Self {
        let data = std::env::temp_dir().join(format!("kass-one-shot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("kass.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
        let settings = include_str!("settings_schema.sql");
        let table = &settings[settings.find("CREATE TABLE capture_settings").unwrap()..];
        conn.execute_batch(&table[..=table.find(");").unwrap() + 1])
            .unwrap();
        let worker = Worker::default();
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database,
            shared_server: None,
            voice_model: None,
        })
        .await
        .unwrap();
        Kass {
            server,
            worker,
            data,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
    ) -> (u16, Value) {
        let mut stream = TcpStream::connect(self.server.address()).await.unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let status = response[9..12].parse().unwrap();
        let body = response.split_once("\r\n\r\n").unwrap().1;
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    async fn generate(&self, body: Value) -> (u16, Value) {
        let body = body.to_string();
        self.call("POST", "/llm/generate", "application/json", body.as_bytes())
            .await
    }

    async fn transcribe(&self, parts: &[Part<'_>]) -> (u16, Value) {
        let mut body = Vec::new();
        for (name, file_name, contents) in parts {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            let disposition = match file_name {
                Some(file) => format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
                ),
                None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"),
            };
            body.extend_from_slice(disposition.as_bytes());
            body.extend_from_slice(contents);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        self.call("POST", "/transcribe", &content_type, &body).await
    }

    fn calls(&self) -> Vec<Value> {
        std::mem::take(&mut *self.worker.calls.lock().unwrap())
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

#[tokio::test]
async fn generates_with_the_base_model_and_no_adapter() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .generate(json!({
            "prompt": "hello",
            "system": "Be brief.",
            "model_size": "1.7B",
            "max_tokens": 64,
            "temperature": 0.3,
            "examples": [["hi", "Hi."]],
        }))
        .await;
    assert_eq!(
        (status, body),
        (200, json!({"text": "HELLO", "model_size": "1.7B"}))
    );
    let sent = &kass.calls()[0];
    assert_eq!(sent["model_size"], "1.7B");
    assert_eq!(sent["system"], "Be brief.");
    assert_eq!(sent["examples"], json!([["hi", "Hi."]]));
    assert_eq!(
        (sent["max_tokens"].clone(), sent["temperature"].clone()),
        (json!(64), json!(0.3))
    );
    // No style flags: the worker runs the plain model, without an adapter.
    assert_eq!(
        (sent["flags"].clone(), sent["cache_key"].clone()),
        (Value::Null, Value::Null)
    );
    assert!(sent["id"].as_str().unwrap().starts_with("llm-"));
}

#[tokio::test]
async fn generate_defaults_like_python() {
    let kass = Kass::start().await;
    let (status, body) = kass.generate(json!({"prompt": "hi"})).await;
    assert_eq!((status, body["model_size"].clone()), (200, json!("0.6B")));
    let sent = &kass.calls()[0];
    assert_eq!(
        (sent["max_tokens"].clone(), sent["temperature"].clone()),
        (json!(512), json!(0.7))
    );
    assert_eq!(
        (sent["system"].clone(), sent["examples"].clone()),
        (json!(""), json!([]))
    );
}

#[tokio::test]
async fn generate_reports_bad_requests_without_running_the_model() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .generate(json!({"prompt": "hi", "examples": [["only one"]]}))
        .await;
    let pair = "Each example must be a [user, assistant] pair";
    assert_eq!((status, body), (400, json!({ "detail": pair })));

    let (status, body) = kass
        .generate(json!({"prompt": "", "model_size": "8B"}))
        .await;
    assert_eq!(status, 422);
    let types: Vec<_> = body["detail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["type"].clone())
        .collect();
    assert_eq!(
        types,
        [json!("string_too_short"), json!("string_pattern_mismatch")]
    );
    assert!(kass.calls().is_empty());
}

#[tokio::test]
async fn generate_hides_what_failed() {
    let kass = Kass::start().await;
    let (status, body) = kass.generate(json!({"prompt": "break"})).await;
    assert_eq!(
        (status, body),
        (500, json!({"detail": "LLM generation failed"}))
    );
}

#[tokio::test]
async fn transcribes_an_upload_and_removes_it() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .transcribe(&[
            ("language", None, b"fr"),
            ("model", None, b"small"),
            ("file", Some("Take.WEBM"), b"bonjour"),
        ])
        .await;
    assert_eq!(
        (status, body),
        (200, json!({"text": "bonjour", "duration": 7.0}))
    );
    let sent = &kass.calls()[0];
    assert_eq!(
        (sent["language"].clone(), sent["stt_model"].clone()),
        (json!("fr"), json!("small"))
    );
    // The worker decodes the upload, and checks there's speech in it.
    assert_eq!(
        (sent["decode"].clone(), sent["check_speech"].clone()),
        (json!(true), json!(true))
    );
    let path = sent["path"].as_str().unwrap();
    assert!(path.ends_with(".webm"), "{path}");
    assert!(!std::path::Path::new(path).exists());
}

#[tokio::test]
async fn transcribes_with_the_users_model_and_reads_unknown_formats_as_wav() {
    let kass = Kass::start().await;
    let (status, _) = kass.transcribe(&[("file", Some("take.mp4"), b"hi")]).await;
    assert_eq!(status, 200);
    let sent = &kass.calls()[0];
    // Settings' Whisper size; no language means the model detects it.
    assert_eq!(
        (sent["stt_model"].clone(), sent["language"].clone()),
        (json!("turbo"), Value::Null)
    );
    assert!(sent["path"].as_str().unwrap().ends_with(".wav"));
}

#[tokio::test]
async fn transcribe_reports_bad_requests_without_running_the_model() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .transcribe(&[("file", Some("a.wav"), b"x"), ("model", None, b"tiny")])
        .await;
    let message = "Invalid model size 'tiny'. Must be one of: base, small, medium, large, turbo";
    assert_eq!((status, body), (400, json!({ "detail": message })));

    let (status, body) = kass.transcribe(&[("language", None, b"en")]).await;
    assert_eq!(status, 422);
    assert_eq!(body["detail"][0]["type"], "missing");
    assert_eq!(body["detail"][0]["loc"], json!(["body", "file"]));

    let (status, body) = kass
        .call("POST", "/transcribe", "application/json", b"{}")
        .await;
    assert_eq!(
        (status, body["detail"][0]["type"].clone()),
        (422, json!("missing"))
    );
    assert!(kass.calls().is_empty());
}

#[tokio::test]
async fn transcribe_passes_on_what_failed() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .transcribe(&[("file", Some("a.ogg"), b"garbage")])
        .await;
    assert_eq!(
        (status, body),
        (500, json!({"detail": "unsupported file format"}))
    );
}
