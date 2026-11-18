//! `POST /commands/run` against a real kass.db, in front of a stand-in
//! worker that "rewrites" by echoing what it was asked and turns down
//! what Python's worker turns down. Python: `backend/tests/test_commands.py`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    /// The bodies of the rewrite calls, in order.
    rewrites: Arc<Mutex<Vec<Value>>>,
}

/// "Hola." for anything Spanish; the worker's 422 when the model isn't
/// downloaded (4B here) and its 500 when the model breaks ("explode").
async fn rewrite(State(worker): State<Worker>, Json(body): Json<Value>) -> Response {
    worker.rewrites.lock().unwrap().push(body.clone());
    let model = body["model_size"].as_str().unwrap().to_string();
    if model == "4B" {
        let detail = "Download Qwen3 4B in Models to use Command Mode";
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": detail })),
        )
            .into_response();
    }
    let instruction = body["instruction"].as_str().unwrap();
    if instruction == "explode" {
        let detail = "Metal ran out of memory";
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "detail": detail })),
        )
            .into_response();
    }
    let text = if instruction.contains("Spanish") {
        "Hola.".to_string()
    } else {
        format!("{} ({instruction})", body["selection"].as_str().unwrap())
    };
    Json(json!({ "text": text, "model": format!("qwen3-{}", model.to_lowercase()) }))
        .into_response()
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/command/rewrite", post(rewrite))
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

impl Kass {
    /// A recording "rec" whose transcript is the instruction.
    async fn start() -> Self {
        let data = std::env::temp_dir().join(format!("kass-commands-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("kass.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
        let settings = include_str!("settings_schema.sql");
        let table = &settings[settings.find("CREATE TABLE capture_settings").unwrap()..];
        conn.execute_batch(&table[..=table.find(");").unwrap() + 1])
            .unwrap();
        conn.execute_batch(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, refinement_flags,
                 refinement_review, audio_deleted, created_at)
             VALUES ('rec', 'captures/rec.wav', 'command', 'make it Spanish',
                     '{\"punctuation_style\": \"standard\"}', '{\"ok\": true}', 0,
                     '2026-03-03 10:00:00.000000');",
        )
        .unwrap();
        let worker = Worker::default();
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database,
            shared_server: None,
            voice_model: None,
            models_dir: None,
        })
        .await
        .unwrap();
        Kass {
            server,
            worker,
            data,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let body = body.to_string();
        let mut stream = TcpStream::connect(self.server.address()).await.unwrap();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let status = response[9..12].parse().unwrap();
        let body = response.split_once("\r\n\r\n").unwrap().1;
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    async fn run(&self, body: Value) -> (u16, Value) {
        self.call("POST", "/commands/run", body).await
    }

    async fn settings(&self, patch: Value) {
        let (status, _) = self.call("PUT", "/settings/captures", patch).await;
        assert_eq!(status, 200);
    }

    fn rewrites(&self) -> Vec<Value> {
        std::mem::take(&mut *self.worker.rewrites.lock().unwrap())
    }

    fn query<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        let conn = rusqlite::Connection::open(self.data.join("kass.db")).unwrap();
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

#[tokio::test]
async fn rewrites_and_saves_a_command_capture() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .run(json!({
            "selection": "Hello.",
            "instruction": "  translate to Spanish ",
            "app_name": " Notes ",
            "app_bundle_id": "",
        }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["source"], "command");
    assert_eq!(body["transcript_raw"], "translate to Spanish");
    assert_eq!(body["transcript_refined"], "Hola.");
    assert_eq!(body["command_selection"], "Hello.");
    assert_eq!(body["command_instruction"], "translate to Spanish");
    assert_eq!(body["command_transform"], Value::Null);
    assert_eq!(
        (body["app_name"].clone(), body["app_bundle_id"].clone()),
        (json!("Notes"), Value::Null)
    );
    assert_eq!(body["llm_model"], "qwen3-0.6b");
    assert_eq!(
        (body["audio_path"].clone(), body["audio_deleted"].clone()),
        (json!(""), json!(false))
    );
    assert_eq!(body["duration_ms"], Value::Null);
    let id = body["id"].as_str().unwrap();
    assert_eq!(id.len(), 36);
    let sql = format!("SELECT audio_path FROM captures WHERE id = '{id}'");
    assert_eq!(kass.query::<String>(&sql), "");
    // The model the dictation cleanup runs on: one model for both.
    assert_eq!(kass.rewrites()[0]["model_size"], "0.6B");
}

#[tokio::test]
async fn runs_a_transform_by_name() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .run(json!({"selection": "hi", "instruction": "Polish"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["command_transform"], "Polish");
    let sent = &kass.rewrites()[0];
    assert_eq!(body["command_instruction"], sent["instruction"]);
    assert_ne!(sent["instruction"], "Polish");
}

#[tokio::test]
async fn uses_the_users_own_transforms_and_model() {
    let kass = Kass::start().await;
    kass.settings(json!({
        "llm_model": "1.7B",
        "command_transforms": [{"id": "t", "name": "Pirate", "instruction": "Talk like a pirate."}],
    }))
    .await;
    let (status, body) = kass
        .run(json!({"selection": "hi", "instruction": "pirate this"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["command_instruction"], "Talk like a pirate.");
    assert_eq!(body["command_transform"], "Pirate");
    assert_eq!(body["llm_model"], "qwen3-1.7b");
}

#[tokio::test]
async fn turns_an_uploaded_command_recording_into_its_rewrite() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .run(json!({"selection": "Hello.", "capture_id": "rec", "instruction": "ignored"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["id"], "rec");
    assert_eq!(body["transcript_raw"], "make it Spanish");
    assert_eq!(body["transcript_refined"], "Hola.");
    assert_eq!(body["audio_path"], "captures/rec.wav");
    // Dictation's cleanup flags and review don't describe a rewrite.
    assert_eq!(
        (
            body["refinement_flags"].clone(),
            body["refinement_review"].clone()
        ),
        (Value::Null, Value::Null)
    );
    assert_eq!(kass.query::<i64>("SELECT COUNT(*) FROM captures"), 1);

    let (status, body) = kass
        .run(json!({"selection": "x", "capture_id": "missing"}))
        .await;
    assert_eq!(
        (status, body),
        (404, json!({"detail": "Capture not found"}))
    );
}

#[tokio::test]
async fn reports_bad_requests_without_running_the_model() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .run(json!({"selection": "", "instruction": "shorter"}))
        .await;
    assert_eq!(
        (status, body),
        (400, json!({"detail": "Select text to rewrite first"}))
    );
    let (status, body) = kass
        .run(json!({"selection": "hi", "instruction": "   "}))
        .await;
    assert_eq!(
        (status, body),
        (400, json!({"detail": "No instruction was heard"}))
    );
    let (status, body) = kass.run(json!({"selection": "hi"})).await;
    assert_eq!(
        (status, body),
        (400, json!({"detail": "No instruction was heard"}))
    );
    assert!(kass.rewrites().is_empty());

    let (status, body) = kass.run(json!({"instruction": "shorter"})).await;
    assert_eq!(status, 422);
    assert_eq!(body["detail"][0]["loc"], json!(["body", "selection"]));
    assert_eq!(body["detail"][0]["type"], "missing");
    assert_eq!(kass.query::<i64>("SELECT COUNT(*) FROM captures"), 1);
}

#[tokio::test]
async fn passes_on_what_the_worker_turned_down_or_failed() {
    let kass = Kass::start().await;
    let (status, body) = kass
        .run(json!({"selection": "hi", "instruction": "explode"}))
        .await;
    assert_eq!(
        (status, body),
        (500, json!({"detail": "Metal ran out of memory"}))
    );

    kass.settings(json!({"llm_model": "4B"})).await;
    let (status, body) = kass
        .run(json!({"selection": "hi", "instruction": "shorter"}))
        .await;
    let detail = "Download Qwen3 4B in Models to use Command Mode";
    assert_eq!((status, body), (400, json!({ "detail": detail })));
    // Nothing is saved for a command that didn't run.
    assert_eq!(kass.query::<i64>("SELECT COUNT(*) FROM captures"), 1);
}
