//! Captures, their audio, reports and events against a real kass.db, in
//! front of a stand-in worker that hears which reports changed and sends
//! events of its own. Spelling is joined, changes taken back and fixed
//! words put in the dictionary by kass-text and this server, here.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

#[derive(Clone)]
struct Worker {
    /// The bodies of the reports-changed calls, in order.
    learned: Arc<Mutex<Vec<Value>>>,
    events: broadcast::Sender<String>,
}

async fn reports_changed(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker.learned.lock().unwrap().push(body);
    Json(json!({ "ok": true }))
}

async fn events(State(worker): State<Worker>) -> Response {
    let events = worker.events.subscribe();
    let stream = futures_util::stream::unfold(events, |mut events| async move {
        let event = events.recv().await.ok()?;
        Some((Ok::<_, std::io::Error>(event), events))
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn preflight() -> Response {
    (StatusCode::OK, [("x-worker", "yes")], "worker").into_response()
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/captures/reports-changed", post(reports_changed))
        .route("/worker/captures/events", get(events))
        .fallback(preflight)
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
    /// Captures "a" (Mail, newest, with audio), "b" (Mail, refined) and "c"
    /// (no app, no audio file).
    async fn start() -> Self {
        let data = std::env::temp_dir().join(format!("kass-captures-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(data.join("captures")).unwrap();
        std::fs::write(data.join("captures/a.wav"), b"RIFF0123456789").unwrap();
        let database = data.join("kass.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
        conn.execute_batch(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, transcript_refined,
                 refinement_flags, app_bundle_id, app_name, audio_deleted, created_at)
             VALUES ('a', 'captures/a.wav', 'dictation', 'wrong words', NULL,
                     NULL, 'com.apple.mail', 'Mail', 0, '2026-03-03 10:00:00.000000'),
                    ('b', 'data/captures/b.wav', 'dictation', 'raw b', 'Refined b.',
                     '{\"punctuation_style\": \"standard\"}', 'com.apple.mail', 'Mail', 0,
                     '2026-03-02 10:00:00.000000'),
                    ('c', 'captures/c.wav', 'dictation', 'raw c', NULL,
                     NULL, NULL, NULL, 0, '2026-03-01 10:00:00.000000');",
        )
        .unwrap();
        let (events, _) = broadcast::channel(16);
        let worker = Worker {
            learned: Arc::default(),
            events,
        };
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

    async fn raw(&self, request: String) -> String {
        let mut stream = TcpStream::connect(self.server.address()).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let response = self
            .raw(format!(
                "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ))
            .await;
        let status = response[9..12].parse().unwrap();
        let body = response.split_once("\r\n\r\n").unwrap().1;
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    async fn capture(&self, id: &str) -> Value {
        let (status, capture) = self.call("GET", &format!("/captures/{id}"), None).await;
        assert_eq!(status, 200);
        capture
    }

    async fn report(&self, id: &str, body: Value) -> (u16, Value) {
        self.call("POST", &format!("/captures/{id}/feedback"), Some(body))
            .await
    }

    fn learned(&self) -> Vec<Value> {
        std::mem::take(&mut *self.worker.learned.lock().unwrap())
    }

    /// The dictionary's words, with the report that added each.
    fn words(&self) -> Vec<(String, String)> {
        let conn = rusqlite::Connection::open(self.data.join("kass.db")).unwrap();
        let mut query = conn
            .prepare(
                "SELECT written, coalesce(added_by, '') FROM dictionary_entries ORDER BY written",
            )
            .unwrap();
        query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn count(&self, sql: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.data.join("kass.db")).unwrap();
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

fn ids(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn lists_captures_newest_first_by_app_and_page() {
    let kass = Kass::start().await;
    let (status, page) = kass.call("GET", "/captures", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        (ids(&page), page["total"].clone()),
        (vec!["a", "b", "c"], json!(3))
    );
    let (_, page) = kass.call("GET", "/captures?limit=1&offset=1", None).await;
    assert_eq!((ids(&page), page["total"].clone()), (vec!["b"], json!(3)));
    let (_, page) = kass
        .call("GET", "/captures?app_bundle_id=com.apple.mail", None)
        .await;
    assert_eq!(
        (ids(&page), page["total"].clone()),
        (vec!["a", "b"], json!(2))
    );
    let (_, page) = kass
        .call("GET", "/captures?unknown_app=true&app_bundle_id=x", None)
        .await;
    assert_eq!(ids(&page), vec!["c"]);
    let (_, page) = kass.call("GET", "/captures?app_bundle_id=", None).await;
    assert_eq!(page["total"], 3);

    let (_, b) = kass.call("GET", "/captures/b", None).await;
    assert_eq!(b["transcript_refined"], "Refined b.");
    assert_eq!(b["refinement_flags"]["punctuation_style"], "standard");
    assert_eq!(b["created_at"], "2026-03-02T10:00:00");
    assert_eq!(b["app_name"], "Mail");

    let bad = json!({"detail": "limit must be between 1 and 200"});
    assert_eq!(
        kass.call("GET", "/captures?limit=0", None).await,
        (400, bad.clone())
    );
    assert_eq!(
        kass.call("GET", "/captures?limit=201", None).await,
        (400, bad)
    );
    let negative = json!({"detail": "offset must be >= 0"});
    assert_eq!(
        kass.call("GET", "/captures?offset=-1", None).await,
        (400, negative)
    );
    let (status, problems) = kass
        .call("GET", "/captures?limit=x&unknown_app=maybe", None)
        .await;
    assert_eq!(status, 422);
    assert_eq!(problems["detail"][0]["loc"], json!(["query", "limit"]));
    assert_eq!(problems["detail"][0]["type"], "int_parsing");
    assert_eq!(problems["detail"][1]["type"], "bool_parsing");

    let missing = json!({"detail": "Capture not found"});
    assert_eq!(
        kass.call("GET", "/captures/nope", None).await,
        (404, missing)
    );
}

#[tokio::test]
async fn lists_the_apps_captures_came_from_with_their_styles() {
    let kass = Kass::start().await;
    let conn = rusqlite::Connection::open(kass.data.join("kass.db")).unwrap();
    conn.execute_batch(
        "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
             capitalize_first, smart_cleanup, preserve_technical, created_at)
         VALUES ('personal', 'Personal', 0, 1, 'casual', 1, 1, 0, '2026-01-01 00:00:00.000000'),
                ('chat', 'Chat', 1, 0, 'casual', 1, 1, 0, '2026-01-01 00:00:00.000000');
         INSERT INTO app_styles (bundle_id, style_id, app_name, updated_at)
         VALUES ('com.apple.mail', 'chat', 'Mail', '2026-01-01 00:00:00.000000');
         INSERT INTO captures (id, audio_path, source, transcript_raw, app_bundle_id, app_name,
             audio_deleted, created_at)
         VALUES ('d', 'captures/d.wav', 'dictation', 'raw d', 'com.mrgnhnt.kass', 'Kass', 0,
                 '2026-03-04 10:00:00.250000');",
    )
    .unwrap();
    // A teach session is open for the "chat" style.
    let (status, _) = kass
        .call("POST", "/writing-style/teach?style=chat", None)
        .await;
    assert_eq!(status, 200);

    let (status, listed) = kass.call("GET", "/captures/apps", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        listed,
        json!({
            "total": 4,
            "unknown_count": 1,
            "apps": [
                {
                    "app_bundle_id": "com.apple.mail", "app_name": "Mail", "count": 2,
                    "last_captured_at": "2026-03-03T10:00:00", "style_id": "chat",
                    "confirmed": true, "suggested_style_id": null,
                },
                {
                    "app_bundle_id": "com.mrgnhnt.kass", "app_name": "Kass", "count": 1,
                    "last_captured_at": "2026-03-04T10:00:00.250000", "style_id": "chat",
                    "confirmed": false, "suggested_style_id": "personal",
                },
            ],
        })
    );

    let preflight = kass
        .raw(
            "OPTIONS /captures/apps HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\n\
             Access-Control-Request-Method: GET\r\nConnection: close\r\n\r\n"
                .into(),
        )
        .await
        .to_lowercase();
    assert!(!preflight.contains("x-worker"), "{preflight}");
}

#[tokio::test]
async fn serves_audio_as_a_download_with_ranges() {
    let kass = Kass::start().await;
    let whole = kass
        .raw("GET /captures/a/audio HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".into())
        .await;
    assert!(whole.starts_with("HTTP/1.1 200"), "{whole}");
    let lower = whole.to_lowercase();
    assert!(lower.contains("content-type: audio/wav"), "{whole}");
    assert!(
        lower.contains("content-disposition: attachment; filename=\"capture_a.wav\""),
        "{whole}"
    );
    assert!(whole.ends_with("\r\n\r\nRIFF0123456789"), "{whole}");

    let part = kass
        .raw("GET /captures/a/audio HTTP/1.1\r\nHost: x\r\nRange: bytes=0-3\r\nConnection: close\r\n\r\n".into())
        .await;
    assert!(part.starts_with("HTTP/1.1 206"), "{part}");
    assert!(part.ends_with("\r\n\r\nRIFF"), "{part}");

    let gone = json!({"detail": "Audio file not found"});
    assert_eq!(
        kass.call("GET", "/captures/c/audio", None).await,
        (404, gone)
    );
    let missing = json!({"detail": "Capture not found"});
    assert_eq!(
        kass.call("GET", "/captures/nope/audio", None).await,
        (404, missing)
    );
}

#[tokio::test]
async fn keeps_a_correction_and_the_words_it_fixed() {
    let kass = Kass::start().await;
    let seen = kass.capture("a").await;
    let body = json!({"snapshot": seen, "target": "raw", "expected_text": "Rong words", "notes": " Missed "});
    let (status, report) = kass.report("a", body).await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["notes"], "Missed");
    assert_eq!(report["source"], "manual");
    assert_eq!(report["snapshot"], seen);
    let id = report["id"].as_str().unwrap();
    assert_eq!(kass.words(), vec![("Rong".into(), id.to_string())]);
    assert_eq!(
        kass.learned(),
        vec![json!({
            "changed": [{"target": "raw", "source": "manual"}],
            "forgotten": null,
        })]
    );
    assert_eq!(
        kass.call("GET", "/captures/a/feedback", None).await,
        (200, json!([report]))
    );
    assert_eq!(
        kass.call("GET", "/capture/feedback/export", None).await,
        (200, json!([report]))
    );

    let spoken = json!({"snapshot": seen, "target": "raw", "expected_text": "Thanks, M-E-G.", "source": "voice_fix"});
    let (_, spoken) = kass.report("a", spoken).await;
    assert_eq!(spoken["expected_text"], "Thanks, MEG.");
    // A redictation is weaker evidence: it teaches no words.
    let again = json!({"snapshot": seen, "target": "raw", "expected_text": "wrong Wordz", "source": "redictation"});
    assert_eq!(kass.report("a", again).await.0, 200);
    assert!(!kass.words().iter().any(|(written, _)| written == "Wordz"));

    // Amending a report takes back the words it taught.
    let amend =
        json!({"snapshot": seen, "target": "raw", "expected_text": "Right word", "replaces": id});
    let (status, amended) = kass.report("a", amend.clone()).await;
    assert_eq!(status, 200, "{amended}");
    // The amended report is relearned; whether to retrain is the server's.
    assert_eq!(kass.learned()[2]["changed"], json!([{"target": "raw", "source": "manual"}]));
    assert!(!kass.words().iter().any(|(_, by)| by == id));
    let (_, reports) = kass.call("GET", "/captures/a/feedback", None).await;
    assert_eq!(reports.as_array().unwrap().len(), 3);
    let gone = json!({"detail": "The correction being amended is gone. Refresh the capture."});
    assert_eq!(kass.report("a", amend).await, (409, gone));
}

#[tokio::test]
async fn refuses_corrections_that_dont_fit_the_capture() {
    let kass = Kass::start().await;
    let seen = kass.capture("a").await;
    let same = json!({"snapshot": seen, "target": "raw", "expected_text": "wrong words"});
    let unchanged = json!({"detail": "Expected output must differ from the model output."});
    assert_eq!(kass.report("a", same).await, (409, unchanged));
    let refined = json!({"snapshot": seen, "target": "refined", "expected_text": "x"});
    let unrefined = json!({"detail": "This capture has no refined output to report."});
    assert_eq!(kass.report("a", refined).await, (409, unrefined));
    let mut stale = seen.clone();
    stale["transcript_raw"] = json!("older words");
    let stale = json!({"snapshot": stale, "target": "raw", "expected_text": "x"});
    let changed = json!({"detail": "Capture changed. Refresh it before reporting a correction."});
    assert_eq!(kass.report("a", stale).await, (409, changed));
    let elsewhere = json!({"snapshot": seen, "target": "raw", "expected_text": "x"});
    let missing = json!({"detail": "Capture not found"});
    assert_eq!(kass.report("nope", elsewhere).await, (404, missing.clone()));
    assert_eq!(
        kass.call("GET", "/captures/nope/feedback", None).await,
        (404, missing)
    );

    let (status, problems) = kass
        .report("a", json!({"target": "both", "expected_text": 3}))
        .await;
    assert_eq!(status, 422);
    let kinds: Vec<_> = problems["detail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| problem["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["literal_error", "string_type", "missing"]);
    let (status, problems) = kass.call("POST", "/captures/a/feedback", None).await;
    assert_eq!(
        (status, problems["detail"][0]["type"].clone()),
        (422, json!("missing"))
    );
    assert!(kass.learned().is_empty());
}

#[tokio::test]
async fn withdrawing_a_report_takes_its_changes_out_of_newer_ones() {
    let kass = Kass::start().await;
    let seen = kass.capture("a").await;
    let first = json!({"snapshot": seen, "target": "raw", "expected_text": "Rong words"});
    let (_, first) = kass.report("a", first).await;
    let next = json!({"snapshot": seen, "target": "raw", "expected_text": "Rong Wordz"});
    let (_, next) = kass.report("a", next).await;
    let undone = json!({"snapshot": seen, "target": "raw", "expected_text": "Rong words", "source": "redictation"});
    let (_, undone) = kass.report("a", undone).await;
    kass.learned();

    let first_id = first["id"].as_str().unwrap();
    let path = format!("/captures/a/feedback/{first_id}");
    let elsewhere = format!("/captures/b/feedback/{first_id}");
    let missing = json!({"detail": "Report not found"});
    assert_eq!(
        kass.call("DELETE", &elsewhere, None).await,
        (404, missing.clone())
    );
    assert_eq!(kass.call("DELETE", &path, None).await, (204, Value::Null));
    // The newest one changed nothing once the first was taken back, so it went too.
    let (_, reports) = kass.call("GET", "/captures/a/feedback", None).await;
    let left: Vec<_> = reports
        .as_array()
        .unwrap()
        .iter()
        .map(|report| (report["id"].clone(), report["expected_text"].clone()))
        .collect();
    assert_eq!(left, vec![(next["id"].clone(), json!("wrong Wordz"))]);
    assert_ne!(undone["id"], next["id"]);
    // The first one's word went; the newer one still fixes "Wordz", so it
    // keeps that word.
    let next_id = next["id"].as_str().unwrap().to_string();
    assert_eq!(
        kass.words(),
        vec![
            ("Rong Wordz".into(), next_id.clone()),
            ("Wordz".into(), next_id)
        ]
    );
    assert_eq!(
        kass.learned(),
        vec![json!({
            "changed": [
                {"target": "raw", "source": "manual"},
                {"target": "raw", "source": "redictation"},
            ],
            "forgotten": null,
        })]
    );
    assert_eq!(kass.call("DELETE", &path, None).await, (404, missing));
}

#[tokio::test]
async fn deleting_a_capture_removes_its_audio_reports_and_words() {
    let kass = Kass::start().await;
    let seen = kass.capture("a").await;
    let (_, report) = kass
        .report(
            "a",
            json!({"snapshot": seen, "target": "raw", "expected_text": "Right words"}),
        )
        .await;
    kass.learned();
    let conn = rusqlite::Connection::open(kass.data.join("kass.db")).unwrap();
    conn.execute(
        "INSERT INTO dictionary_entries (id, scope, scope_id, written, \"key\", source, added_by)
         VALUES ('w1', 'global', '', 'Right', 'right', 'correction', ?1),
                ('w2', 'global', '', 'Kept', 'kept', 'correction', 'someone-else')",
        [report["id"].as_str().unwrap()],
    )
    .unwrap();

    let deleted = json!({"message": "Capture a deleted"});
    assert_eq!(
        kass.call("DELETE", "/captures/a", None).await,
        (200, deleted)
    );
    assert!(!kass.data.join("captures/a.wav").exists());
    assert_eq!(kass.count("SELECT count(*) FROM capture_feedback"), 0);
    assert_eq!(kass.count("SELECT count(*) FROM dictionary_entries"), 1);
    assert_eq!(
        kass.learned(),
        vec![json!({
            "changed": [{"target": "raw", "source": "manual"}],
            "forgotten": "a",
        })]
    );
    let missing = json!({"detail": "Capture not found"});
    assert_eq!(
        kass.call("GET", "/captures/a", None).await,
        (404, missing.clone())
    );
    assert_eq!(
        kass.call("DELETE", "/captures/a", None).await,
        (404, missing)
    );
    // A capture whose audio is already gone still goes.
    assert_eq!(kass.call("DELETE", "/captures/c", None).await.0, 200);
}

/// Reads an open stream until `done` holds for what came so far.
async fn read_until(stream: &mut TcpStream, seen: &mut String, done: impl Fn(&str) -> bool) {
    let mut buffer = [0u8; 4096];
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done(seen) {
            let n = stream.read(&mut buffer).await.unwrap();
            assert!(n > 0, "the stream ended: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buffer[..n]));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out; so far: {seen}"));
}

#[tokio::test]
async fn streams_capture_events_with_the_macs_listening() {
    let kass = Kass::start().await;
    let mut stream = TcpStream::connect(kass.server.address()).await.unwrap();
    stream
        .write_all(b"GET /captures/events?mac=studio&mac_name=Studio HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut seen = String::new();
    read_until(&mut stream, &mut seen, |seen| seen.contains("event: peers")).await;
    assert!(seen.contains("text/event-stream; charset=utf-8"), "{seen}");
    assert!(
        seen.contains("event: changed\ndata: {}\n\nevent: learned\ndata: {}\n\n"),
        "{seen}"
    );
    assert!(
        seen.contains(
            r#"event: peers\ndata: [{"id": "studio", "name": "Studio"}]"#
                .replace("\\n", "\n")
                .as_str()
        ),
        "{seen}"
    );

    seen.clear();
    assert_eq!(kass.call("DELETE", "/captures/c", None).await.0, 200);
    read_until(&mut stream, &mut seen, |seen| {
        seen.contains("event: changed")
    })
    .await;

    // What the worker saves or learns reaches the apps too.
    tokio::time::timeout(Duration::from_secs(5), async {
        while kass.worker.events.receiver_count() == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the server followed the worker's events");
    seen.clear();
    kass.worker
        .events
        .send("event: learned\ndata: {}\n\n".into())
        .unwrap();
    read_until(&mut stream, &mut seen, |seen| {
        seen.contains("event: learned")
    })
    .await;
}

#[tokio::test]
async fn answers_the_app_s_cors_and_leaves_upload_preflights_to_the_worker() {
    let kass = Kass::start().await;
    let preflight = kass
        .raw(
            "OPTIONS /captures/a/feedback HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\n\
             Access-Control-Request-Method: POST\r\nConnection: close\r\n\r\n"
                .into(),
        )
        .await
        .to_lowercase();
    assert!(preflight.starts_with("http/1.1 200"), "{preflight}");
    assert!(
        preflight.contains("access-control-allow-origin: tauri://localhost"),
        "{preflight}"
    );
    assert!(!preflight.contains("x-worker"), "{preflight}");

    let listed = kass
        .raw("GET /captures HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\nConnection: close\r\n\r\n".into())
        .await
        .to_lowercase();
    assert!(
        listed.contains("access-control-allow-origin: tauri://localhost"),
        "{listed}"
    );

    // Preflights for an upload are still the worker's; it answers them the same.
    let saving = kass
        .raw(
            "OPTIONS /captures HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\n\
             Access-Control-Request-Method: POST\r\nConnection: close\r\n\r\n"
                .into(),
        )
        .await;
    assert!(saving.contains("x-worker: yes"), "{saving}");
    // The upload itself is answered here (`tests/capture_jobs.rs`).
    let saved = kass
        .raw(
            "POST /captures HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
        )
        .await
        .to_lowercase();
    assert!(saved.starts_with("http/1.1 422"), "{saved}");
    assert!(!saved.contains("x-worker"), "{saved}");
    assert!(
        saved.contains("access-control-allow-origin: tauri://localhost"),
        "{saved}"
    );

    let not_found = json!({"detail": "Not Found"});
    assert_eq!(
        kass.call("POST", "/worker/captures/reports-changed", Some(json!({})))
            .await,
        (404, not_found)
    );
}
