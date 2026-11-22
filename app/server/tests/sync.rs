//! Sharing Kass between two Macs, end to end: two servers, each with its
//! own data dir and kass.db, in front of stand-in workers. One shares; the
//! other stands in for it. What the stand-in dictated reaches the shared
//! Kass with its recording; the shared Kass's dictionary and trained model
//! reach the stand-in.
//!
//! Then the import rules on their own: the later edit wins, the same word
//! is kept once, each Mac keeps its own settings, and a batch sent twice
//! changes nothing more.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::routing::any;
use axum::{Json, Router};
use kass_server::captures::store::{insert_capture, NewCapture};
use kass_server::dictionary::store::Sync;
use kass_server::sync::import::{apply, Audio, Change};
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What a stand-in worker was asked to do, by path.
#[derive(Clone, Default)]
struct Worker {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Worker {
    fn calls(&self, path: &str) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, body)| body.clone())
            .collect()
    }
}

async fn answer(
    State(worker): State<Worker>,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Json<Value> {
    let path = uri.path().to_string();
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    worker
        .calls
        .lock()
        .unwrap()
        .push((path.clone(), body.clone()));
    Json(match path.as_str() {
        // It uses the copied models: they pass their checks.
        "/worker/sync/mirrored" => json!({"active": body["active"]}),
        "/worker/models/loaded" => json!({"loaded": []}),
        "/worker/learning/idle" => json!({"idle": true}),
        _ => json!({}),
    })
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new().fallback(any(answer)).with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A data dir with a kass.db, as this server makes it, holding the default style.
fn data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kass-sync-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    kass_server::db::prepare(&dir.join("kass.db")).unwrap();
    Connection::open(dir.join("kass.db"))
        .unwrap()
        .execute_batch(DEFAULT_STYLE)
        .unwrap();
    dir
}

const DEFAULT_STYLE: &str = "INSERT INTO writing_styles (id, name, position, is_default,
    punctuation_style, capitalize_first, smart_cleanup, preserve_technical)
    VALUES ('default', 'Default', 0, 1, 'standard', 1, 1, 1);";

struct Kass {
    server: kass_server::Running,
    worker: Worker,
    dir: PathBuf,
}

impl Kass {
    async fn start(dir: PathBuf, worker: Worker, shared_server: Option<String>) -> Self {
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database: dir.join("kass.db"),
            shared_server,
            voice_model: None,
            models_dir: Some(dir.join("models")),
            app_version: "0.0.0-test".into(),
        })
        .await
        .unwrap();
        Kass {
            server,
            worker,
            dir,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.server.address())
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.join("kass.db")).unwrap()
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

    async fn add_word(&self, written: &str) {
        let body = json!({"written": written, "spoken": null, "places": [{"scope": "global"}]});
        let (status, answer) = self.call("POST", "/dictionary", Some(body)).await;
        assert!(status < 300, "{status} {answer}");
    }

    fn words(&self) -> Vec<String> {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT written FROM dictionary_entries ORDER BY written")
            .unwrap();
        let words = statement.query_map([], |row| row.get(0)).unwrap();
        words.collect::<Result<_, _>>().unwrap()
    }

    fn pending(&self) -> i64 {
        self.db()
            .query_row("SELECT count(*) FROM sync_changes", [], |row| row.get(0))
            .unwrap()
    }
}

/// Wait up to `seconds` for `done`, checking twice a second.
async fn eventually(seconds: u64, mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    done()
}

/// A dictation saved on the stand-in while the shared Kass was away,
/// recorded to send, with its recording.
fn dictate_on(dir: &Path, id: &str, recording: &[u8]) {
    std::fs::create_dir_all(dir.join("captures")).unwrap();
    std::fs::write(dir.join("captures").join(format!("{id}.wav")), recording).unwrap();
    let mut conn = Connection::open(dir.join("kass.db")).unwrap();
    let capture = NewCapture {
        id: id.into(),
        audio_path: format!("captures/{id}.wav"),
        source: "dictation".into(),
        duration_ms: Some(1200),
        transcript_raw: "hello from the laptop".into(),
        transcript_refined: Some("Hello from the laptop.".into()),
        ..NewCapture::default()
    };
    insert_capture(&mut conn, &capture, Sync { standing_in: true }).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capture_a_word_and_a_trained_model_cross_between_two_macs() {
    // Nothing here downloads a model: Hugging Face is a closed port.
    std::env::set_var("HF_ENDPOINT", "http://127.0.0.1:9");

    // The shared Kass, with a word in its dictionary and a trained cleanup model in use.
    let shared_dir = data_dir("shared");
    let adapter = shared_dir.join("model-improvement/runs/run-1/adapter");
    std::fs::create_dir_all(&adapter).unwrap();
    std::fs::write(adapter.join("adapters.safetensors"), b"weights").unwrap();
    std::fs::write(adapter.join("adapter_config.json"), b"{\"rank\": 8}").unwrap();
    std::fs::write(
        shared_dir.join("model-improvement/state.json"),
        json!({"version": 1, "active": {"llm": {"id": "run-1", "sha256": "abc", "path": adapter}}})
            .to_string(),
    )
    .unwrap();
    let shared = Kass::start(shared_dir, Worker::default(), None).await;
    shared.add_word("Studio").await;

    // The stand-in dictated while the shared Kass was away.
    let stand_in_dir = data_dir("stand-in");
    dictate_on(&stand_in_dir, "take-1", b"RIFF-laptop");
    let stand_in = Kass::start(stand_in_dir, Worker::default(), Some(shared.url() + "/")).await;
    stand_in.add_word("Laptop").await;

    let copied = eventually(20, || {
        stand_in.pending() == 0
            && stand_in.words() == ["Laptop", "Studio"]
            && !stand_in.worker.calls("/worker/sync/mirrored").is_empty()
    })
    .await;
    assert!(
        copied,
        "pending {}, words {:?}, status {:?}",
        stand_in.pending(),
        stand_in.words(),
        stand_in.call("GET", "/sync/status", None).await
    );

    // The capture reached the shared Kass, with its recording.
    let (text, stored): (String, String) = shared
        .db()
        .query_row(
            "SELECT transcript_refined, audio_path FROM captures WHERE id = 'take-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(text, "Hello from the laptop.");
    assert_eq!(
        std::fs::read(shared.dir.join(stored)).unwrap(),
        b"RIFF-laptop"
    );
    // So did the word added on the stand-in; nothing was recorded there.
    assert_eq!(shared.words(), ["Laptop", "Studio"]);
    assert_eq!(shared.pending(), 0);

    // The trained model reached the stand-in, which uses its copy.
    let mirrored = stand_in.dir.join("model-improvement/mirrored/run-1/llm");
    assert_eq!(
        std::fs::read(mirrored.join("adapters.safetensors")).unwrap(),
        b"weights"
    );
    assert_eq!(
        std::fs::read(mirrored.join("adapter_config.json")).unwrap(),
        b"{\"rank\": 8}"
    );
    assert_eq!(
        stand_in
            .worker
            .calls("/worker/sync/mirrored")
            .last()
            .unwrap(),
        &json!({"active": {"llm": {"id": "run-1", "sha256": "abc", "path": mirrored}}})
    );
    // It's the stand-in's model now.
    let saved: Value = serde_json::from_slice(
        &std::fs::read(stand_in.dir.join("model-improvement/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["active"]["llm"]["path"], json!(mirrored));

    // Each says how syncing goes.
    let (_, status) = stand_in.call("GET", "/sync/status", None).await;
    assert_eq!(status["standing_in"], true);
    assert_eq!(status["shared_server"], shared.url());
    assert_eq!(status["reachable"], true);
    assert_eq!(status["pending"], 0);
    assert!(status["synced_at"].as_str().unwrap().ends_with("+00:00"));
    assert_eq!(
        shared.call("GET", "/sync/status", None).await.1,
        json!({"standing_in": false})
    );

    // Changes go to the shared Kass, never to a stand-in.
    let (status, answer) = stand_in.call("POST", "/sync/import", None).await;
    assert_eq!(
        (status, answer["detail"].as_str()),
        (
            409,
            Some("This Kass stands in for another; send changes to that one")
        )
    );

    // Falling back loads the stand-in's models, once; the shared Kass has nothing to wake.
    for _ in 0..2 {
        assert_eq!(stand_in.call("POST", "/sync/wake", None).await.0, 204);
    }
    assert_eq!(stand_in.worker.calls("/worker/models/startup").len(), 1);
    assert_eq!(shared.call("POST", "/sync/wake", None).await.0, 204);
    assert!(shared.worker.calls("/worker/models/startup").is_empty());

    // A file that isn't in a trained model in use isn't served.
    let (status, _) = shared
        .call("GET", "/sync/adapters/llm/..%2Fkass.db", None)
        .await;
    assert_eq!(status, 404);
    let (status, _) = shared
        .call("GET", "/sync/adapters/voice/adapters.safetensors", None)
        .await;
    assert_eq!(status, 404);
}

/// A kass.db with the default style, as both Macs have.
fn database() -> Connection {
    let dir = data_dir("import");
    Connection::open(dir.join("kass.db")).unwrap()
}

fn change(kind: &str, id: &str, at: &str, row: Value) -> Change {
    serde_json::from_value(json!({
        "kind": kind, "id": id, "deleted": row.is_null(), "changed_at": at, "row": row,
    }))
    .unwrap()
}

fn entry(id: &str, written: &str, at: &str) -> Value {
    json!({
        "id": id, "scope": "global", "scope_id": "", "written": written,
        "key": written.to_lowercase(), "created_at": at, "updated_at": at,
    })
}

fn add_entry(conn: &Connection, id: &str, written: &str, at: &str) {
    conn.execute(
        "INSERT INTO dictionary_entries (id, scope, scope_id, written, \"key\", created_at, updated_at)
         VALUES (?1, 'global', '', ?2, lower(?2), ?3, ?3)",
        [id, written, at],
    )
    .unwrap();
}

fn written(conn: &Connection) -> Vec<(String, String)> {
    let mut statement = conn
        .prepare("SELECT id, written FROM dictionary_entries ORDER BY id")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

#[test]
fn the_later_edit_wins() {
    let mut conn = database();
    add_entry(&conn, "a", "Kass", "2026-11-18 12:00:00.000000");
    add_entry(&conn, "b", "Midna", "2026-11-18 10:00:00.000000");
    let changes = vec![
        change(
            "dictionary",
            "a",
            "2026-11-18T11:00:00",
            entry("a", "KASS", "2026-11-18T11:00:00"),
        ),
        change(
            "dictionary",
            "b",
            "2026-11-18T11:00:00",
            entry("b", "MIDNA", "2026-11-18T11:00:00"),
        ),
    ];
    apply(&mut conn, Path::new("/nowhere"), changes, &Audio::new()).unwrap();
    assert_eq!(
        written(&conn),
        [("a".into(), "Kass".into()), ("b".into(), "MIDNA".into())]
    );
}

#[test]
fn the_same_word_added_on_both_macs_is_kept_once() {
    let mut conn = database();
    add_entry(&conn, "theirs", "Kass", "2026-11-18 10:00:00.000000");
    let changes = vec![change(
        "dictionary",
        "ours",
        "2026-11-18T11:00:00",
        entry("ours", "Kass", "2026-11-18T11:00:00"),
    )];
    apply(&mut conn, Path::new("/nowhere"), changes, &Audio::new()).unwrap();
    assert_eq!(written(&conn), [("ours".into(), "Kass".into())]);
}

#[test]
fn settings_this_mac_owns_stay_put() {
    let mut conn = database();
    kass_server::settings::get(&mut conn).unwrap();
    conn.execute_batch(
        "UPDATE capture_settings SET input_device_id = 'studio-mic', live_text = 0,
             updated_at = '2026-11-18 10:00:00.000000'",
    )
    .unwrap();
    let row = json!({"id": 1, "input_device_id": "laptop-mic", "live_text": true,
                     "updated_at": "2026-11-18T11:00:00"});
    apply(
        &mut conn,
        Path::new("/nowhere"),
        vec![change("settings", "1", "2026-11-18T11:00:00", row)],
        &Audio::new(),
    )
    .unwrap();
    let (device, live): (String, bool) = conn
        .query_row(
            "SELECT input_device_id, live_text FROM capture_settings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((device.as_str(), live), ("studio-mic", true));
}

#[test]
fn a_batch_sent_twice_changes_nothing_more_and_a_delete_follows() {
    let dir = std::env::temp_dir().join(format!("kass-import-{}", uuid::Uuid::new_v4()));
    let mut conn = database();
    let capture = json!({
        "id": "take", "audio_path": "captures/take.wav", "source": "dictation",
        "transcript_raw": "hi", "duration_ms": 900, "audio_deleted": false,
        "created_at": "2026-11-18T11:00:00.250000",
    });
    let report = json!({
        "id": "report", "capture_id": "take", "target": "refined", "expected_text": "Hi!",
        "snapshot": "{}", "notes": "", "source": "manual", "created_at": "2026-11-18T11:01:00",
    });
    let batch = vec![
        change("capture", "take", "2026-11-18T11:00:00.250000", capture),
        change("feedback", "report", "2026-11-18T11:01:00", report),
    ];
    let audio: Audio = [(
        "take".to_string(),
        ("take.wav".to_string(), b"RIFF".to_vec()),
    )]
    .into();
    for _ in 0..2 {
        let applied = apply(&mut conn, &dir, batch.clone(), &audio).unwrap();
        assert_eq!(
            applied.reports.into_iter().collect::<Vec<_>>(),
            [(("refined".to_string(), "manual".to_string()), false)]
        );
    }
    assert_eq!(
        (count(&conn, "captures"), count(&conn, "capture_feedback")),
        (1, 1)
    );
    assert_eq!(
        std::fs::read(dir.join("captures/take.wav")).unwrap(),
        b"RIFF"
    );
    let created: String = conn
        .query_row("SELECT created_at FROM captures", [], |row| row.get(0))
        .unwrap();
    assert_eq!(created, "2026-11-18 11:00:00.250000");

    let gone = vec![change(
        "capture",
        "take",
        "2026-11-18T12:00:00",
        Value::Null,
    )];
    let applied = apply(&mut conn, &dir, gone, &Audio::new()).unwrap();
    assert_eq!(applied.forgotten, ["take"]);
    assert_eq!(
        (count(&conn, "captures"), count(&conn, "capture_feedback")),
        (0, 0)
    );
    assert!(!dir.join("captures/take.wav").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn a_kind_this_kass_doesnt_know_is_left_out() {
    let mut conn = database();
    let changes = vec![change(
        "hologram",
        "x",
        "2026-11-18T11:00:00",
        json!({"id": "x"}),
    )];
    let applied = apply(&mut conn, Path::new("/nowhere"), changes, &Audio::new()).unwrap();
    assert_eq!(applied, Default::default());
}
