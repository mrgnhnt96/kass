//! Uploading, refining and retranscribing a capture end to end, through a
//! stand-in worker that recognizes and generates.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the stand-in worker was asked, with each body.
#[derive(Clone, Default)]
struct Worker {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    /// Lets a cleanup of "hold on" finish.
    go_on: Arc<tokio::sync::Notify>,
}

impl Worker {
    fn bodies(&self, path: &str) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(called, _)| called == path)
            .map(|(_, body)| body.clone())
            .collect()
    }

    fn record(&self, path: &str, body: Value) {
        self.calls.lock().unwrap().push((path.to_string(), body));
    }
}

async fn ok(State(worker): State<Worker>, request: axum::extract::Request) -> Json<Value> {
    let path = request.uri().path().to_string();
    let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap();
    worker.record(&path, serde_json::from_slice(&bytes).unwrap_or(Value::Null));
    Json(json!({"ok": true}))
}

async fn speech_model() -> Json<Value> {
    Json(json!({"model": "turbo"}))
}

async fn recognize_file(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker.record("/worker/recognize/file", body);
    Json(json!({"text": " hello there ", "seconds": 0.1}))
}

async fn generate(State(worker): State<Worker>, Json(body): Json<Value>) -> Response {
    let prompt = body["prompt"].as_str().unwrap_or("").to_string();
    worker.record("/worker/cleanup/generate", body);
    if prompt == "hold on" {
        worker.go_on.notified().await;
    }
    let lines = [
        json!({"partial": "Hello"}).to_string(),
        json!({"text": cleaned(&prompt), "model": "qwen", "stopped": false, "seconds": 0.1})
            .to_string(),
    ];
    Body::from(format!("{}\n{}\n", lines[0], lines[1])).into_response()
}

/// A faithful cleanup: capitalized with a period. "say more" gets words
/// that weren't said, which the content check refuses.
fn cleaned(prompt: &str) -> String {
    if prompt == "say more" {
        return "Say more about the weather in Paris.".into();
    }
    let mut chars = prompt.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    format!("{first}{}.", chars.as_str())
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/speech-model", post(speech_model))
        .route("/worker/recognize/file", post(recognize_file))
        .route("/worker/cleanup/generate", post(generate))
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
        let data = std::env::temp_dir().join(format!("kass-jobs-{}", uuid::Uuid::new_v4()));
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

    async fn send(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> (u16, Value) {
        let mut stream = TcpStream::connect(self.server.address()).await.unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        let status = response[9..12].parse().unwrap();
        let body = &response[response.find("\r\n\r\n").unwrap() + 4..];
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    async fn post_json(&self, path: &str, body: Value) -> (u16, Value) {
        let headers = "Content-Type: application/json\r\n";
        self.send("POST", path, headers, body.to_string().as_bytes())
            .await
    }

    /// Upload `audio` as `filename` with the form `fields`.
    async fn upload(&self, filename: &str, audio: &[u8], fields: &[(&str, &str)]) -> (u16, Value) {
        let boundary = "kassboundary";
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        body.extend(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend(audio);
        body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
        let headers = format!(
            "Content-Type: multipart/form-data; boundary={boundary}\r\nX-Kass-Mac: mac-1\r\nX-Kass-Mac-Name: Desk\r\n"
        );
        self.send("POST", "/captures", &headers, &body).await
    }

    fn captures_dir(&self) -> Vec<String> {
        match std::fs::read_dir(self.data.join("captures")) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

/// Half a second of 16 kHz mono tone, as a WAV file.
fn wav() -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
    for n in 0..8000 {
        writer.write_sample(((n % 40) - 20) as i16 * 800).unwrap();
    }
    writer.finalize().unwrap();
    cursor.into_inner()
}

#[tokio::test]
async fn uploads_transcribe_and_save_a_capture() {
    let kass = Kass::start().await;
    let fields = [
        ("source", "dictation"),
        ("app_bundle_id", "  com.apple.Notes  "),
        ("app_name", " "),
    ];
    let (status, capture) = kass.upload("take.wav", &wav(), &fields).await;
    assert_eq!(status, 200, "{capture}");
    let id = capture["id"].as_str().unwrap();
    assert_eq!(capture["source"], "dictation");
    assert_eq!(capture["transcript_raw"], "hello there");
    assert_eq!(capture["duration_ms"], 500);
    assert_eq!(capture["stt_model"], "turbo");
    assert_eq!(capture["app_bundle_id"], "com.apple.Notes");
    assert_eq!(capture["app_name"], Value::Null);
    assert_eq!(capture["mac_id"], "mac-1");
    assert_eq!(capture["mac_name"], "Desk");
    assert_eq!(capture["audio_path"], format!("captures/{id}.wav"));
    assert_eq!(capture["audio_deleted"], false);
    assert!(capture["auto_refine"].is_boolean());
    assert_eq!(kass.captures_dir(), [format!("{id}.wav")]);

    let asked = &kass.worker.bodies("/worker/recognize/file")[0];
    assert_eq!(asked["check_speech"], true);
    assert_eq!(asked["stt_model"], "turbo");
    assert!(asked["path"]
        .as_str()
        .unwrap()
        .ends_with(&format!("{id}.wav")));

    let (status, listed) = kass.send("GET", "/captures", "", b"").await;
    assert_eq!(status, 200);
    assert_eq!(listed["total"], 1);
}

#[tokio::test]
async fn a_command_upload_always_refines_and_pastes() {
    let kass = Kass::start().await;
    let settings = json!({"auto_refine": false, "allow_auto_paste": false}).to_string();
    let headers = "Content-Type: application/json\r\n";
    let (status, _) = kass
        .send("PUT", "/settings/captures", headers, settings.as_bytes())
        .await;
    assert_eq!(status, 200);
    let (status, capture) = kass
        .upload("take.wav", &wav(), &[("source", "command")])
        .await;
    assert_eq!(status, 200, "{capture}");
    assert_eq!(capture["auto_refine"], true);
    assert_eq!(capture["allow_auto_paste"], true);
}

#[tokio::test]
async fn bad_uploads_are_refused_and_leave_no_audio() {
    let kass = Kass::start().await;
    let (status, answer) = kass.upload("take.wav", b"", &[]).await;
    assert_eq!(
        (status, answer),
        (400, json!({"detail": "Uploaded file is empty"}))
    );

    let (status, answer) = kass
        .upload("take.wav", &wav(), &[("source", "radio")])
        .await;
    assert_eq!(status, 400);
    assert_eq!(
        answer["detail"],
        "Invalid source 'radio'. Must be one of ['command', 'dictation', 'file', 'recording']"
    );

    let (status, answer) = kass.upload("take.webm", b"not audio at all", &[]).await;
    assert_eq!(status, 400);
    assert_eq!(
        answer["detail"],
        "Could not decode .webm audio — the recording may be empty or corrupt"
    );
    assert!(kass.captures_dir().is_empty(), "{:?}", kass.captures_dir());

    let headers = "Content-Type: multipart/form-data; boundary=x\r\n";
    let (status, answer) = kass.send("POST", "/captures", headers, b"--x--\r\n").await;
    assert_eq!(status, 422);
    assert_eq!(answer["detail"][0]["loc"], json!(["body", "file"]));
    assert!(kass.worker.bodies("/worker/recognize/file").is_empty());
}

#[tokio::test]
async fn refines_a_capture_in_its_style() {
    let kass = Kass::start().await;
    let (_, capture) = kass.upload("take.wav", &wav(), &[]).await;
    let id = capture["id"].as_str().unwrap();

    let (status, refined) = kass
        .post_json(&format!("/captures/{id}/refine"), json!({}))
        .await;
    assert_eq!(status, 200, "{refined}");
    assert_eq!(refined["transcript_refined"], "Hello there.");
    assert_eq!(refined["refinement_review"], Value::Null);
    assert_eq!(refined["style_id"], "personal");
    assert_eq!(refined["refinement_flags"]["style"], "personal");
    assert!(refined["llm_model"].is_string());

    let (status, refined) = kass
        .post_json(
            &format!("/captures/{id}/refine"),
            json!({"model_size": "4B", "flags": {"capitalize_first": false}}),
        )
        .await;
    assert_eq!(status, 200, "{refined}");
    assert_eq!(refined["llm_model"], "4B");
    assert_eq!(refined["refinement_flags"]["capitalize_first"], false);
    assert_eq!(refined["style_id"], Value::Null);
}

#[tokio::test]
async fn a_cleanup_that_adds_words_is_refused_and_flagged() {
    let kass = Kass::start().await;
    let (_, capture) = kass.upload("take.wav", &wav(), &[]).await;
    let id = capture["id"].as_str().unwrap();
    let conn = rusqlite::Connection::open(kass.data.join("kass.db")).unwrap();
    conn.execute(
        "UPDATE captures SET transcript_raw = 'say more' WHERE id = ?1",
        [id],
    )
    .unwrap();

    let (status, refined) = kass
        .post_json(&format!("/captures/{id}/refine"), json!({}))
        .await;
    assert_eq!(status, 200, "{refined}");
    assert_eq!(refined["refinement_review"]["outcome"], "reject");
    // The transcript stands in, written as cleanup would start and end it.
    assert_eq!(refined["transcript_refined"], "Say more.");
}

#[tokio::test]
async fn refine_checks_its_request_and_the_capture() {
    let kass = Kass::start().await;
    let (status, answer) = kass.post_json("/captures/nope/refine", json!({})).await;
    assert_eq!(
        (status, answer),
        (404, json!({"detail": "Capture not found"}))
    );

    let (status, answer) = kass
        .post_json("/captures/nope/refine", json!({"model_size": "8B"}))
        .await;
    assert_eq!(status, 422);
    assert_eq!(answer["detail"][0]["type"], "string_pattern_mismatch");

    let (status, _) = kass.send("POST", "/captures/nope/refine", "", b"").await;
    assert_eq!(status, 422);
}

#[tokio::test]
async fn retranscribing_clears_the_cleanup() {
    let kass = Kass::start().await;
    let (_, capture) = kass.upload("take.wav", &wav(), &[]).await;
    let id = capture["id"].as_str().unwrap();
    kass.post_json(&format!("/captures/{id}/refine"), json!({}))
        .await;

    let (status, again) = kass
        .post_json(
            &format!("/captures/{id}/retranscribe"),
            json!({"model": "small", "language": "de"}),
        )
        .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["transcript_raw"], "hello there");
    assert_eq!(again["transcript_refined"], Value::Null);
    assert_eq!(again["llm_model"], Value::Null);
    assert_eq!(again["refinement_flags"], Value::Null);
    assert_eq!(again["stt_model"], "small");
    assert_eq!(again["language"], "de");
    let asked = kass.worker.bodies("/worker/recognize/file");
    assert_eq!(asked.last().unwrap()["language"], "de");

    let (status, answer) = kass
        .post_json(
            &format!("/captures/{id}/retranscribe"),
            json!({"language": "xx"}),
        )
        .await;
    assert_eq!(status, 422, "{answer}");

    std::fs::remove_file(kass.data.join(format!("captures/{id}.wav"))).unwrap();
    let (status, answer) = kass
        .post_json(&format!("/captures/{id}/retranscribe"), json!({}))
        .await;
    assert_eq!(status, 410);
    assert_eq!(
        answer["detail"],
        format!("Audio for capture {id} is missing")
    );

    let (status, _) = kass
        .post_json("/captures/nope/retranscribe", json!({}))
        .await;
    assert_eq!(status, 404);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn other_formats_are_converted_to_wav() {
    let kass = Kass::start().await;
    let folder = kass.data.join("source");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("take.wav"), wav()).unwrap();
    let converted = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac"])
        .arg(folder.join("take.wav"))
        .arg(folder.join("take.m4a"))
        .status()
        .unwrap();
    assert!(converted.success());
    let m4a = std::fs::read(folder.join("take.m4a")).unwrap();

    let (status, capture) = kass.upload("Take.M4A", &m4a, &[]).await;
    assert_eq!(status, 200, "{capture}");
    let id = capture["id"].as_str().unwrap();
    assert_eq!(capture["audio_path"], format!("captures/{id}.wav"));
    let duration = capture["duration_ms"].as_i64().unwrap();
    assert!((450..=600).contains(&duration), "{duration}");
    assert_eq!(kass.captures_dir(), [format!("{id}.wav")]);
}

#[tokio::test]
async fn history_retention_leaves_a_capture_being_refined() {
    let kass = Kass::start().await;
    let (_, old) = kass.upload("old.wav", &wav(), &[]).await;
    let old = old["id"].as_str().unwrap().to_string();
    kass.upload("current.wav", &wav(), &[]).await;
    let conn = rusqlite::Connection::open(kass.data.join("kass.db")).unwrap();
    let long_ago = (chrono::Utc::now().naive_utc() - chrono::Duration::days(40))
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string();
    conn.execute(
        "UPDATE captures SET transcript_raw = 'hold on', created_at = ?2 WHERE id = ?1",
        rusqlite::params![old, long_ago],
    )
    .unwrap();
    let exists = |id: &str| {
        conn.query_row("SELECT count(*) FROM captures WHERE id = ?1", [id], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
            == 1
    };
    let window = || {
        kass.send(
            "PUT",
            "/settings/captures",
            "Content-Type: application/json\r\n",
            br#"{"history_retention_days": 30}"#,
        )
    };

    let path = format!("/captures/{old}/refine");
    let refine = kass.post_json(&path, json!({}));
    let meanwhile = async {
        while !kass
            .worker
            .bodies("/worker/cleanup/generate")
            .iter()
            .any(|body| body["prompt"] == "hold on")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Saving a window sweeps now; the capture being refined stays.
        assert_eq!(window().await.0, 200);
        assert!(exists(&old));
        kass.worker.go_on.notify_one();
    };
    let ((status, refined), ()) = tokio::join!(refine, meanwhile);
    assert_eq!(status, 200, "{refined}");
    assert_eq!(refined["transcript_refined"], "Hold on.");

    assert_eq!(window().await.0, 200);
    assert!(!exists(&old));
    assert!(!kass
        .captures_dir()
        .iter()
        .any(|name| name.starts_with(&old)));
}
