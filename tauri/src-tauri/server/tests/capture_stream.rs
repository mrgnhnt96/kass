//! The `/captures/stream` session end to end: a client streams a take to
//! the server, which recognizes and cleans it through a stand-in worker,
//! saves it in kass.db and keeps the result for recovery.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// What the stand-in worker was asked.
#[derive(Clone, Default)]
struct Worker {
    calls: Arc<Mutex<Vec<String>>>,
}

impl Worker {
    fn called(&self, path: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|call| call == path)
    }
}

async fn ok(State(worker): State<Worker>, request: axum::extract::Request) -> Json<Value> {
    worker
        .calls
        .lock()
        .unwrap()
        .push(request.uri().path().to_string());
    Json(json!({}))
}

async fn speech_model() -> Json<Value> {
    Json(json!({"model": "turbo"}))
}

async fn recognize(State(worker): State<Worker>) -> Json<Value> {
    worker
        .calls
        .lock()
        .unwrap()
        .push("/worker/recognize".into());
    Json(json!({"text": " hello there", "alignment": null, "seconds": 0.1}))
}

async fn generate(State(worker): State<Worker>, Json(body): Json<Value>) -> Response {
    worker
        .calls
        .lock()
        .unwrap()
        .push("/worker/cleanup/generate".into());
    assert_eq!(body["prompt"], "hello there");
    let lines = [
        json!({"partial": "Hello"}).to_string(),
        json!({"text": "Hello there.", "model": "qwen", "stopped": false, "seconds": 0.1})
            .to_string(),
    ];
    Body::from(format!("{}\n{}\n", lines[0], lines[1])).into_response()
}

async fn rewrite(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["selection"], "some text");
    assert_eq!(body["instruction"], "hello there");
    Json(json!({"text": "Some text, rewritten.", "model": "qwen"}))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/speech-model", post(speech_model))
        .route("/worker/recognize", post(recognize))
        .route("/worker/cleanup/generate", post(generate))
        .route("/worker/command/rewrite", post(rewrite))
        .fallback(ok)
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
    async fn start() -> Self {
        let data = std::env::temp_dir().join(format!("kass-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("kass.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
        let settings = include_str!("settings_schema.sql");
        let table = &settings[settings.find("CREATE TABLE capture_settings").unwrap()..];
        conn.execute_batch(&table[..=table.find(");").unwrap() + 1])
            .unwrap();
        conn.execute_batch(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
                 capitalize_first, smart_cleanup, preserve_technical, description)
             VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1, NULL);",
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
            app_version: "0.0.0-test".into(),
        })
        .await
        .unwrap();
        Kass {
            server,
            worker,
            data,
        }
    }

    async fn connect(&self) -> Socket {
        let url = format!("ws://{}/captures/stream", self.server.address());
        tokio_tungstenite::connect_async(url).await.unwrap().0
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(self.server.address()).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let status = response[9..12].parse().unwrap();
        let body = &response[response.find("\r\n\r\n").unwrap() + 4..];
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

async fn send(socket: &mut Socket, message: Value) {
    socket
        .send(Message::text(message.to_string()))
        .await
        .unwrap();
}

/// The next event, skipping none.
async fn event(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("an event in time")
        .expect("the socket open")
        .unwrap();
    match message {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected an event, got {other:?}"),
    }
}

/// Events up to and including the first of `kind`.
async fn until(socket: &mut Socket, kind: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let next = event(socket).await;
        let done = next["type"] == kind || next["type"] == "error";
        seen.push(next);
        if done {
            return seen;
        }
    }
}

fn start(source: &str) -> Value {
    json!({
        "type": "start",
        "protocol_version": 1,
        "sample_rate": 16000,
        "channels": 1,
        "encoding": "pcm_s16le",
        "source": source,
    })
}

/// Half a second of a tone, as the first frame: its sequence number and
/// sample offset, then the PCM.
fn audio() -> Vec<u8> {
    let mut frame = [0u32.to_le_bytes(), 0u32.to_le_bytes()].concat();
    frame.extend((0..8000).flat_map(|n: i32| (((n % 40) - 20) as i16 * 800).to_le_bytes()));
    frame
}

#[tokio::test]
async fn streams_a_dictation_saves_it_and_keeps_the_result() {
    let kass = Kass::start().await;
    let mut socket = kass.connect().await;
    send(&mut socket, start("dictation")).await;
    let ready = event(&mut socket).await;
    assert_eq!(ready["type"], "ready");
    let id = ready["session_id"].as_str().unwrap().to_string();
    assert_eq!(ready["auto_refine"], true);

    send(
        &mut socket,
        json!({"type": "app", "bundle_id": " com.apple.mail ", "name": "Mail"}),
    )
    .await;
    socket.send(Message::binary(audio())).await.unwrap();
    send(&mut socket, json!({"type": "finish"})).await;
    let events = until(&mut socket, "final").await;
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.last(), Some(&"final"), "{events:?}");
    assert!(kinds.contains(&"transcript"), "{events:?}");
    assert!(kinds.contains(&"refined"), "{events:?}");
    let revisions: Vec<u64> = events
        .iter()
        .map(|e| e["revision"].as_u64().unwrap())
        .collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "{revisions:?}"
    );

    let last = events.last().unwrap();
    assert_eq!(last["session_id"], id.as_str());
    assert_eq!(last["covered_samples"], 8000);
    assert_eq!(last["refinement_error"], Value::Null);
    let capture = &last["capture"];
    assert_eq!(capture["id"], id.as_str());
    assert_eq!(capture["transcript_raw"], "hello there");
    assert_eq!(capture["transcript_refined"], "Hello there.");
    assert_eq!(capture["app_bundle_id"], "com.apple.mail");
    assert_eq!(capture["style_id"], "personal");
    assert_eq!(capture["auto_refine"], true);
    assert_eq!(capture["duration_ms"], 500);
    assert!(kass.data.join(format!("captures/{id}.wav")).exists());
    assert!(kass.worker.called("/worker/foreground/begin"));

    let (status, saved) = kass.get(&format!("/captures/{id}")).await;
    assert_eq!(status, 200);
    assert_eq!(saved["transcript_refined"], "Hello there.");
    let (status, result) = kass.get(&format!("/captures/stream/{id}/result")).await;
    assert_eq!(status, 200);
    assert_eq!(result["type"], "final");
    assert_eq!(result["capture"]["id"], id.as_str());
    let (status, missing) = kass.get("/captures/stream/nope/result").await;
    assert_eq!(status, 404);
    assert_eq!(
        missing["detail"],
        "Streaming result unavailable; inspect Captures before retrying"
    );
}

#[tokio::test]
async fn rewrites_a_selection_in_a_command_session() {
    let kass = Kass::start().await;
    let mut socket = kass.connect().await;
    send(&mut socket, start("command")).await;
    assert_eq!(event(&mut socket).await["type"], "ready");
    send(
        &mut socket,
        json!({"type": "selection", "text": "some text"}),
    )
    .await;
    socket.send(Message::binary(audio())).await.unwrap();
    send(&mut socket, json!({"type": "finish"})).await;
    let events = until(&mut socket, "final").await;
    let last = events.last().unwrap();
    assert_eq!(last["type"], "final", "{events:?}");
    let capture = &last["capture"];
    assert_eq!(capture["source"], "command");
    assert_eq!(capture["command_selection"], "some text");
    assert_eq!(capture["command_instruction"], "hello there");
    assert_eq!(capture["transcript_refined"], "Some text, rewritten.");
    assert!(!kass.worker.called("/worker/cleanup/generate"));
}

#[tokio::test]
async fn cancel_saves_nothing() {
    let kass = Kass::start().await;
    let mut socket = kass.connect().await;
    send(&mut socket, start("dictation")).await;
    let id = event(&mut socket).await["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    socket.send(Message::binary(audio())).await.unwrap();
    send(&mut socket, json!({"type": "cancel"})).await;
    while let Some(Ok(message)) = socket.next().await {
        if message.is_close() {
            break;
        }
    }
    let (status, _) = kass.get(&format!("/captures/{id}")).await;
    assert_eq!(status, 404);
    assert!(!kass.data.join(format!("captures/{id}.wav")).exists());
}

#[tokio::test]
async fn reports_a_bad_start_or_command() {
    let kass = Kass::start().await;
    let mut socket = kass.connect().await;
    send(&mut socket, json!(["start"])).await;
    let error = event(&mut socket).await;
    assert_eq!(
        error,
        json!({"type": "error", "message": "Expected a start object", "session_id": null})
    );

    let mut socket = kass.connect().await;
    send(&mut socket, start("dictation")).await;
    let id = event(&mut socket).await["session_id"].clone();
    send(&mut socket, json!({"type": "finish"})).await;
    let error = event(&mut socket).await;
    assert_eq!(error["type"], "error");
    assert_eq!(error["message"], "Cannot finish empty audio");
    assert_eq!(error["session_id"], id);

    let mut socket = kass.connect().await;
    send(&mut socket, start("dictation")).await;
    event(&mut socket).await;
    send(&mut socket, json!({"type": "dance"})).await;
    assert_eq!(
        event(&mut socket).await["message"],
        "Expected app, context, last_take, cue, selection, finish or cancel"
    );
}

#[tokio::test]
async fn refuses_other_origins_and_a_third_session() {
    let kass = Kass::start().await;
    let mut request = format!("ws://{}/captures/stream", kass.server.address())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    let _first = kass.connect().await;
    let _second = kass.connect().await;
    let url = format!("ws://{}/captures/stream", kass.server.address());
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}
