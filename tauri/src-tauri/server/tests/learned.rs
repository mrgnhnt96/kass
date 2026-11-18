//! What each writing style learned (writing-style.json and
//! correction-notes.json) against a real kass.db, in front of a stand-in
//! worker that records when it's told to reread them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// `hashlib.sha256(b"Write Kass as one word.").hexdigest()[:12]`
const KASS_RULE: &str = "Write Kass as one word.";
const KASS_RULE_ID: &str = "dd4982fda515";
const BASICALLY_RULE: &str = "Drop 'basically'.";

/// Said, shown after cleanup, and what the user meant: four word fixes and
/// two dropped final periods. Counted, the habit is the dropped period.
const CORRECTIONS: [(&str, &str, &str); 6] = [
    ("command and push", "Command and push.", "Commit and push."),
    (
        "command and push now",
        "Command and push.",
        "Commit and push.",
    ),
    (
        "command and push it",
        "Command and push.",
        "Commit and push.",
    ),
    (
        "command and push them",
        "Command and push.",
        "Commit and push.",
    ),
    ("commit", "Commit.", "Commit"),
    ("status", "Status.", "Status"),
];

#[derive(Clone, Default)]
struct Worker {
    /// Each `/worker/learned/changed` body, in order.
    learned: Arc<Mutex<Vec<Value>>>,
}

async fn learned_changed(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker.learned.lock().unwrap().push(body);
    Json(json!({"ok": true}))
}

async fn styles_state() -> Json<Value> {
    Json(json!({"cache_mb_per_style": 189, "teaching_style": null}))
}

async fn finish(UrlPath(session): UrlPath<String>) -> Json<Value> {
    Json(json!({"finished": session}))
}

async fn ok() -> Json<Value> {
    Json(json!({"ok": true}))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/learned/changed", post(learned_changed))
        .route("/worker/styles/state", post(styles_state))
        .route("/writing-style/teach/{session}/finish", post(finish))
        .fallback(ok)
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A kass.db with the default style and a cleaned-up capture for each of
/// [`CORRECTIONS`], corrected by hand when `corrected`.
fn database(corrected: bool) -> PathBuf {
    // Its own folder: the learned files are written next to it.
    let dir = std::env::temp_dir().join(format!("kass-learned-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kass.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
    conn.execute_batch(
        "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
             capitalize_first, smart_cleanup, preserve_technical)
         VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1);",
    )
    .unwrap();
    for (at, (said, shown, meant)) in CORRECTIONS.iter().enumerate() {
        let created_at = format!("2026-11-01 10:0{at}:00.000000");
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, transcript_refined,
                 audio_deleted, created_at)
             VALUES (?1, 'a.wav', 'dictation', ?2, ?3, 0, ?4)",
            (format!("c{at}"), said, shown, &created_at),
        )
        .unwrap();
        if corrected {
            let snapshot = json!({"transcript_raw": said, "transcript_refined": shown});
            conn.execute(
                "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes,
                     snapshot, source, created_at)
                 VALUES (?1, ?2, 'refined', ?3, '', ?4, 'manual', ?5)",
                (
                    format!("f{at}"),
                    format!("c{at}"),
                    meant,
                    snapshot.to_string(),
                    &created_at,
                ),
            )
            .unwrap();
        }
    }
    path
}

struct Kass {
    server: kass_server::Running,
    learned: Arc<Mutex<Vec<Value>>>,
    dir: PathBuf,
}

impl Kass {
    /// Kass, once the habits counted at start are saved.
    async fn start(corrected: bool) -> Self {
        let worker = Worker::default();
        let learned = worker.learned.clone();
        let database = database(corrected);
        let dir = database.parent().unwrap().to_path_buf();
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker).await,
            database,
            shared_server: None,
            voice_model: None,
            models_dir: None,
        })
        .await
        .unwrap();
        let kass = Kass {
            server,
            learned,
            dir,
        };
        kass.until(|kass| {
            kass.file("writing-style.json")["counting"] == 2
                && !kass.learned.lock().unwrap().is_empty()
        })
        .await;
        assert_eq!(kass.told(), vec![json!({"forget_teaching": null})]);
        kass
    }

    async fn until(&self, done: impl Fn(&Kass) -> bool) {
        for _ in 0..100 {
            if done(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out");
    }

    /// What the worker was told since the last call.
    fn told(&self) -> Vec<Value> {
        std::mem::take(&mut *self.learned.lock().unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn file(&self, name: &str) -> Value {
        read(&self.path(name))
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
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
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

fn not_found(detail: &str) -> (u16, Value) {
    (404, json!({ "detail": detail }))
}

#[tokio::test]
async fn habits_are_counted_from_corrections_at_start() {
    let kass = Kass::start(true).await;
    let (status, learned) = kass.call("GET", "/writing-style", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        learned,
        json!({
            "ready": true, "runs": 0, "last_run_at": null, "example_count": 0,
            "habits": ["drop_final_period"],
        })
    );
    // The empty style id is the default style; an unknown one is missing.
    assert_eq!(
        kass.call("GET", "/writing-style?style=", None).await,
        (200, learned)
    );
    assert_eq!(
        kass.call("GET", "/writing-style?style=nope", None).await,
        not_found("Style not found")
    );
}

#[tokio::test]
async fn habits_are_counted_again_after_a_correction() {
    let kass = Kass::start(false).await;
    let (_, learned) = kass.call("GET", "/writing-style", None).await;
    assert_eq!(learned["habits"], json!([]));

    for (at, (_, _, meant)) in CORRECTIONS.iter().enumerate() {
        let (_, seen) = kass.call("GET", &format!("/captures/c{at}"), None).await;
        let body = json!({"snapshot": seen, "target": "refined", "expected_text": meant});
        let (status, report) = kass
            .call("POST", &format!("/captures/c{at}/feedback"), Some(body))
            .await;
        assert_eq!(status, 200, "{report}");
    }
    let (_, learned) = kass.call("GET", "/writing-style", None).await;
    assert_eq!(learned["habits"], json!(["drop_final_period"]));
    assert!(!kass.told().is_empty());
}

#[tokio::test]
async fn examples_are_listed_and_hidden() {
    let kass = Kass::start(true).await;
    let (status, examples) = kass.call("GET", "/writing-style/examples", None).await;
    assert_eq!(status, 200);
    assert_eq!(examples.as_array().unwrap().len(), 6);
    assert_eq!(
        examples[0],
        json!({
            "id": "correction:f5", "source": "correction", "said": "status", "meant": "Status",
            "created_at": "2026-11-01T10:05:00", "app_bundle_id": null, "app_name": null,
        })
    );
    assert_eq!(
        kass.call("GET", "/writing-style/examples?style=nope", None)
            .await,
        not_found("Style not found")
    );

    assert_eq!(
        kass.call("DELETE", "/writing-style/examples/correction:f5", None)
            .await
            .0,
        204
    );
    assert_eq!(kass.told(), vec![json!({"forget_teaching": null})]);
    assert_eq!(
        kass.file("writing-style.json")["hidden_examples"],
        json!(["correction:f5"])
    );
    let (_, examples) = kass.call("GET", "/writing-style/examples", None).await;
    assert_eq!(examples.as_array().unwrap().len(), 5);
    assert_eq!(examples[0]["id"], "correction:f4");
    assert_eq!(
        kass.call("DELETE", "/writing-style/examples/correction:missing", None)
            .await,
        not_found("Example not found")
    );
    assert!(kass.told().is_empty());
}

#[tokio::test]
async fn notes_are_listed_removed_and_reset() {
    let kass = Kass::start(true).await;
    let notes = json!({
        "version": 2,
        "styles": {"personal": {
            "notes": [KASS_RULE, BASICALLY_RULE], "considered_ids": [], "history": [],
            "last_run": "2026-11-02T09:00:00", "outcome": "updated", "metrics": null,
        }},
    });
    std::fs::write(kass.path("correction-notes.json"), notes.to_string()).unwrap();

    let (status, listed) = kass.call("GET", "/writing-style/notes", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        listed,
        json!({
            "notes": [
                {"id": KASS_RULE_ID, "text": KASS_RULE},
                {"id": listed["notes"][1]["id"], "text": BASICALLY_RULE},
            ],
            // Every example fits the prompt: none waits for a summary.
            "pending": 0,
            "last_run": "2026-11-02T09:00:00",
            "outcome": "updated",
        })
    );
    assert_eq!(
        kass.call("GET", "/writing-style/notes?style=nope", None)
            .await,
        not_found("Style not found")
    );

    let remove = format!("/writing-style/notes/{KASS_RULE_ID}");
    assert_eq!(kass.call("DELETE", &remove, None).await.0, 204);
    assert_eq!(kass.told(), vec![json!({"forget_teaching": null})]);
    let saved = &kass.file("correction-notes.json")["styles"]["personal"];
    assert_eq!(saved["notes"], json!([BASICALLY_RULE]));
    assert_eq!(
        saved["history"],
        json!([{"notes": [KASS_RULE, BASICALLY_RULE]}])
    );
    assert_eq!(saved["outcome"], "updated");
    assert_eq!(
        kass.call("DELETE", &remove, None).await,
        not_found("Rule not found")
    );
    assert_eq!(
        kass.call("DELETE", &format!("{remove}?style=nope"), None)
            .await,
        not_found("Style not found")
    );

    // Reset forgets the style's habits and rules, and its teach sessions.
    let (status, reset) = kass.call("DELETE", "/writing-style", None).await;
    assert_eq!(status, 200);
    assert_eq!(reset["ready"], false);
    assert_eq!(reset["habits"], json!([]));
    assert_eq!(kass.told(), vec![json!({"forget_teaching": "personal"})]);
    let (_, listed) = kass.call("GET", "/writing-style/notes", None).await;
    assert_eq!(listed["notes"], json!([]));
    assert_eq!(listed["outcome"], "waiting");
}

#[tokio::test]
async fn a_deleted_style_forgets_what_it_learned() {
    let kass = Kass::start(true).await;
    let (_, chat) = kass
        .call("POST", "/writing-styles", Some(json!({"name": "Chat"})))
        .await;
    let chat = chat["id"].as_str().unwrap().to_string();

    let mut writing = kass.file("writing-style.json");
    writing["styles"][&chat] = json!({"runs": 2, "examples": []});
    std::fs::write(kass.path("writing-style.json"), writing.to_string()).unwrap();
    let notes = json!({"version": 2, "styles": {&chat: {"notes": [KASS_RULE]}}});
    std::fs::write(kass.path("correction-notes.json"), notes.to_string()).unwrap();
    let (_, learned) = kass
        .call("GET", &format!("/writing-style?style={chat}"), None)
        .await;
    assert_eq!(learned["runs"], 2);

    let deleted = kass
        .call("DELETE", &format!("/writing-styles/{chat}"), None)
        .await;
    assert_eq!(deleted.0, 204);
    let writing = kass.file("writing-style.json");
    assert_eq!(writing["styles"].get(&chat), None);
    assert_eq!(
        kass.file("correction-notes.json")["styles"].get(&chat),
        None
    );
    let (_, learned) = kass.call("GET", "/writing-style", None).await;
    assert_eq!(learned["habits"], json!(["drop_final_period"]));
}

#[tokio::test]
async fn a_teach_session_finishes_in_the_worker() {
    let kass = Kass::start(false).await;
    assert_eq!(
        kass.call("POST", "/writing-style/teach/s1/finish", Some(json!({})))
            .await,
        (200, json!({"finished": "s1"}))
    );
}
