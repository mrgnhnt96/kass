//! Settings, history retention, health, links and take reports against a
//! real kass.db, in front of a stand-in worker that records what it is
//! asked to do. Python: test_history_retention, test_commands,
//! test_dictation_settings_migration, test_usage_report and test_links.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the stand-in worker was asked to do, in order.
#[derive(Clone, Default)]
struct Worker {
    calls: Arc<Mutex<Vec<String>>>,
}

async fn switched(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker
        .calls
        .lock()
        .unwrap()
        .push(format!("switched {}", body["enabled"]));
    Json(json!({"ok": true}))
}

async fn start_worker(worker: Worker) -> std::net::SocketAddr {
    let app = Router::new()
        .route("/worker/shared-adapters/switched", post(switched))
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

struct Kass {
    server: kass_server::Running,
    worker: Worker,
    /// The data folder, holding kass.db, as the app's is.
    data: PathBuf,
}

struct Reply {
    status: u16,
    head: String,
    body: Value,
}

impl Kass {
    async fn start(standing_in: bool) -> Self {
        let data = std::env::temp_dir().join(format!("kass-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("kass.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch(include_str!("settings_schema.sql"))
            .unwrap();
        let worker = Worker::default();
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database,
            shared_server: standing_in.then(|| "http://shared.local:17493".to_string()),
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

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.data.join("kass.db")).unwrap()
    }

    /// What the worker was asked since the last look.
    fn worker_calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.worker.calls.lock().unwrap())
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
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
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        Reply {
            status: head[9..12].parse().unwrap(),
            head: head.to_string(),
            body: serde_json::from_str(body).unwrap_or(Value::Null),
        }
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let reply = self.send("GET", path, None).await;
        (reply.status, reply.body)
    }

    async fn put(&self, body: Value) -> (u16, Value) {
        let reply = self.send("PUT", "/settings/captures", Some(body)).await;
        (reply.status, reply.body)
    }

    /// A capture `days_ago` days old.
    fn capture(&self, id: &str, days_ago: i64) {
        let created =
            (Utc::now().naive_utc() - Duration::days(days_ago)).format("%Y-%m-%d %H:%M:%S%.6f");
        self.db()
            .execute(
                "INSERT INTO captures (id, audio_path, source, transcript_raw, audio_deleted, created_at) \
                 VALUES (?1, '', 'dictation', 'hi', 0, ?2)",
                rusqlite::params![id, created.to_string()],
            )
            .unwrap();
    }

    fn ids(&self) -> Vec<String> {
        let conn = self.db();
        let mut statement = conn.prepare("SELECT id FROM captures ORDER BY id").unwrap();
        let ids = statement.query_map([], |row| row.get(0)).unwrap();
        ids.map(Result::unwrap).collect()
    }

    fn sync_changes(&self) -> Vec<(String, String)> {
        let conn = self.db();
        let mut statement = conn
            .prepare("SELECT kind, ref_id FROM sync_changes ORDER BY kind, ref_id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

fn problem(kind: &str, loc: Value, msg: &str, input: Value, ctx: Option<Value>) -> Value {
    let mut problem = json!({"type": kind, "loc": loc, "msg": msg, "input": input});
    if let Some(ctx) = ctx {
        problem["ctx"] = ctx;
    }
    problem
}

// -- settings -----------------------------------------------------------------

#[tokio::test]
async fn the_first_read_saves_the_python_defaults() {
    let kass = Kass::start(false).await;
    let defaults: Value = serde_json::from_str(include_str!("settings_defaults.json")).unwrap();
    assert_eq!(
        kass.get("/settings/captures").await,
        (200, defaults.clone())
    );
    let saved: i64 = kass
        .db()
        .query_row(
            "SELECT count(*) FROM capture_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, 1);
    assert_eq!(kass.get("/settings/captures").await, (200, defaults));
}

#[tokio::test]
async fn a_change_keeps_the_settings_it_does_not_name() {
    let kass = Kass::start(false).await;
    let (status, saved) = kass
        .put(json!({"llm_model": "4B", "input_device_id": "mic-2"}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(saved["llm_model"], "4B");
    assert_eq!(saved["input_device_id"], "mic-2");
    assert_eq!(saved["stt_model"], "turbo");
    // Null resets the microphone; a null for a setting that can't be empty is ignored.
    let (_, saved) = kass
        .put(json!({"input_device_id": null, "stt_model": null}))
        .await;
    assert_eq!(saved["input_device_id"], Value::Null);
    assert_eq!(saved["stt_model"], "turbo");
    assert_eq!(saved["llm_model"], "4B");
    let (_, read) = kass.get("/settings/captures").await;
    assert_eq!(read, saved);
    assert!(kass.worker_calls().is_empty());
}

#[tokio::test]
async fn a_change_is_checked_the_way_pydantic_checks_it() {
    let kass = Kass::start(false).await;
    let cases = [
        (
            json!({"sound_cue_volume": 1.5}),
            problem(
                "less_than_equal",
                json!(["body", "sound_cue_volume"]),
                "Input should be less than or equal to 1",
                json!(1.5),
                Some(json!({"le": 1.0})),
            ),
        ),
        (
            json!({"chord_push_to_talk_keys": []}),
            problem(
                "too_short",
                json!(["body", "chord_push_to_talk_keys"]),
                "List should have at least 1 item after validation, not 0",
                json!([]),
                Some(json!({"field_type": "List", "min_length": 1, "actual_length": 0})),
            ),
        ),
        (
            json!({"history_retention_days": 12}),
            problem(
                "literal_error",
                json!(["body", "history_retention_days"]),
                "Input should be 0, 7, 30, 90 or 365",
                json!(12),
                Some(json!({"expected": "0, 7, 30, 90 or 365"})),
            ),
        ),
        (
            json!({"stt_model": "huge"}),
            problem(
                "string_pattern_mismatch",
                json!(["body", "stt_model"]),
                "String should match pattern '^(base|small|medium|large|turbo)$'",
                json!("huge"),
                Some(json!({"pattern": "^(base|small|medium|large|turbo)$"})),
            ),
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(kass.put(body).await, (422, json!({"detail": [expected]})));
    }
    let (status, body) = kass.put(json!({"command_transforms": [{"name": 1}]})).await;
    assert_eq!(status, 422);
    assert_eq!(
        body,
        json!({"detail": [
            problem(
                "string_type",
                json!(["body", "command_transforms", 0, "name"]),
                "Input should be a valid string",
                json!(1),
                None,
            ),
            problem(
                "missing",
                json!(["body", "command_transforms", 0, "instruction"]),
                "Field required",
                json!({"name": 1}),
                None,
            ),
        ]})
    );
    // Lax values pydantic takes: a boolean volume and a whole-number float.
    let (status, saved) = kass
        .put(json!({"sound_cue_volume": true, "history_retention_days": 90.0, "chord_command_keys": []}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(saved["sound_cue_volume"], 1.0);
    assert_eq!(saved["history_retention_days"], 90);
    assert_eq!(saved["chord_command_keys"], json!([]));
}

#[tokio::test]
async fn transforms_that_cannot_be_said_apart_are_refused() {
    let kass = Kass::start(false).await;
    let (status, body) = kass
        .put(json!({"command_transforms": [
            {"name": "A", "instruction": "x"},
            {"name": "a", "instruction": "y"},
        ]}))
        .await;
    assert_eq!(
        (status, body),
        (400, json!({"detail": "Two transforms are called 'a'"}))
    );
    let (status, saved) = kass
        .put(json!({"command_transforms": [{"name": "  Shorten ", "instruction": " Make it short "}]}))
        .await;
    assert_eq!(status, 200);
    let transform = &saved["command_transforms"][0];
    assert_eq!(transform["name"], "Shorten");
    assert_eq!(transform["instruction"], "Make it short");
    assert!(!transform["id"].as_str().unwrap().is_empty());
    // No transforms and no Command Mode chord is a valid choice.
    let (status, saved) = kass
        .put(json!({"command_transforms": [], "chord_command_keys": []}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(saved["command_transforms"], json!([]));
    assert_eq!(saved["chord_command_keys"], json!([]));
}

#[tokio::test]
async fn switching_shared_adapters_tells_the_worker() {
    let kass = Kass::start(false).await;
    let (_, saved) = kass.put(json!({"shared_adapters": false})).await;
    assert_eq!(saved["shared_adapters"], false);
    assert_eq!(kass.worker_calls(), ["switched false"]);
    kass.put(json!({"shared_adapters": true})).await;
    assert_eq!(kass.worker_calls(), ["switched true"]);
}

#[tokio::test]
async fn sharing_usage_again_skips_the_days_it_was_off() {
    let kass = Kass::start(false).await;
    let sent_through = || -> Option<String> {
        kass.db()
            .query_row(
                "SELECT usage_sent_through FROM capture_settings",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    kass.put(json!({"share_usage": false})).await;
    assert_eq!(sent_through(), None);
    let (_, saved) = kass.put(json!({"share_usage": true})).await;
    assert_eq!(saved["share_usage"], true);
    let yesterday = (chrono::Local::now().date_naive() - Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    assert_eq!(sent_through(), Some(yesterday.clone()));
    // Already on: nothing to skip.
    kass.db()
        .execute(
            "UPDATE capture_settings SET usage_sent_through = '2026-01-01'",
            [],
        )
        .unwrap();
    kass.put(json!({"share_usage": true})).await;
    assert_eq!(sent_through().as_deref(), Some("2026-01-01"));
}

#[tokio::test]
async fn standing_in_records_shared_changes_but_not_this_macs_own() {
    let kass = Kass::start(true).await;
    kass.get("/settings/captures").await;
    assert!(kass.sync_changes().is_empty());
    // The microphone is this Mac's own.
    kass.put(json!({"input_device_id": "mic-2"})).await;
    assert!(kass.sync_changes().is_empty());
    // Saving what's already saved changes nothing.
    kass.put(json!({"llm_model": "0.6B"})).await;
    assert!(kass.sync_changes().is_empty());
    kass.put(json!({"llm_model": "4B"})).await;
    assert_eq!(
        kass.sync_changes(),
        [("settings".to_string(), "1".to_string())]
    );
}

#[tokio::test]
async fn not_standing_in_records_nothing() {
    let kass = Kass::start(false).await;
    kass.put(json!({"llm_model": "4B"})).await;
    kass.send(
        "POST",
        "/usage/takes",
        Some(json!({"mode": "dictation", "outcome": "delivered"})),
    )
    .await;
    assert!(kass.sync_changes().is_empty());
}

// -- history retention ----------------------------------------------------------

#[tokio::test]
async fn the_preview_counts_what_a_shorter_window_would_delete() {
    let kass = Kass::start(false).await;
    for days_ago in [100, 60, 20, 5, 0] {
        kass.capture(&format!("c{days_ago}"), days_ago);
    }
    for (days, expiring) in [(7, 3), (30, 2), (90, 1), (365, 0), (0, 0)] {
        let path = format!("/settings/captures/retention-preview?days={days}");
        assert_eq!(
            kass.get(&path).await,
            (200, json!({"days": days, "expiring": expiring}))
        );
    }
    assert_eq!(
        kass.get("/settings/captures/retention-preview?days=12")
            .await,
        (
            422,
            json!({"detail": "days must be one of the retention choices"})
        )
    );
    let (status, body) = kass
        .get("/settings/captures/retention-preview?days=soon")
        .await;
    assert_eq!(status, 422);
    assert_eq!(body["detail"][0]["type"], "int_parsing");
    assert_eq!(body["detail"][0]["loc"], json!(["query", "days"]));
    let (status, body) = kass.get("/settings/captures/retention-preview").await;
    assert_eq!(status, 422);
    assert_eq!(body["detail"][0]["type"], "missing");
}

#[tokio::test]
async fn the_newest_capture_is_never_counted() {
    let kass = Kass::start(false).await;
    kass.capture("only", 100);
    let path = "/settings/captures/retention-preview?days=7";
    assert_eq!(
        kass.get(path).await,
        (200, json!({"days": 7, "expiring": 0}))
    );
}

#[tokio::test]
async fn choosing_a_window_confirms_it_and_sweeps_now() {
    let kass = Kass::start(false).await;
    kass.capture("old", 40);
    kass.capture("new", 1);
    let status = "/settings/captures/retention-status";
    assert_eq!(
        kass.get(status).await,
        (200, json!({"days": 30, "confirmed": false, "expiring": 1}))
    );
    assert_eq!(kass.put(json!({"history_retention_days": 12})).await.0, 422);
    assert_eq!(kass.ids(), ["new", "old"]);
    // Keeping the default, as the launch dialog's first choice does.
    let (_, saved) = kass.put(json!({"history_retention_days": 30})).await;
    assert_eq!(saved["history_retention_confirmed"], true);
    // The window applies before the app reloads its captures list.
    assert_eq!(kass.ids(), ["new"]);
    assert_eq!(
        kass.db()
            .query_row("SELECT count(*) FROM retired_captures", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(kass.worker_calls().is_empty());
    assert_eq!(
        kass.get(status).await,
        (200, json!({"days": 30, "confirmed": true, "expiring": 0}))
    );
}

#[tokio::test]
async fn nothing_to_delete_confirms_without_asking() {
    let kass = Kass::start(false).await;
    kass.capture("first", 3);
    let status = "/settings/captures/retention-status";
    assert_eq!(
        kass.get(status).await,
        (200, json!({"days": 30, "confirmed": true, "expiring": 0}))
    );
    let (_, saved) = kass.get("/settings/captures").await;
    assert_eq!(saved["history_retention_confirmed"], true);
}

// -- take reports ---------------------------------------------------------------

#[tokio::test]
async fn a_take_report_is_saved_for_the_daily_report() {
    let kass = Kass::start(true).await;
    let body = json!({"mode": "command", "outcome": "delivered", "latency_ms": 420});
    let reply = kass.send("POST", "/usage/takes", Some(body)).await;
    assert_eq!(reply.status, 204);
    let failed = json!({"mode": "dictation", "outcome": "failed"});
    assert_eq!(
        kass.send("POST", "/usage/takes", Some(failed)).await.status,
        204
    );
    let conn = kass.db();
    let mut statement = conn
        .prepare("SELECT id, mode, outcome, latency_ms FROM take_reports ORDER BY id")
        .unwrap();
    let saved: Vec<(i64, String, String, Option<i64>)> = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        saved,
        [
            (1, "command".into(), "delivered".into(), Some(420)),
            (2, "dictation".into(), "failed".into(), None),
        ]
    );
    assert_eq!(
        kass.sync_changes(),
        [
            ("take".to_string(), "1".to_string()),
            ("take".to_string(), "2".to_string())
        ]
    );
}

#[tokio::test]
async fn a_take_report_is_checked_the_way_pydantic_checks_it() {
    let kass = Kass::start(false).await;
    let cases = [
        (
            json!({"mode": "dictation", "outcome": "delivered", "latency_ms": -1}),
            problem(
                "greater_than_equal",
                json!(["body", "latency_ms"]),
                "Input should be greater than or equal to 0",
                json!(-1),
                Some(json!({"ge": 0})),
            ),
        ),
        (
            json!({"mode": "x", "outcome": "failed"}),
            problem(
                "literal_error",
                json!(["body", "mode"]),
                "Input should be 'dictation' or 'command'",
                json!("x"),
                Some(json!({"expected": "'dictation' or 'command'"})),
            ),
        ),
    ];
    for (body, expected) in cases {
        let reply = kass.send("POST", "/usage/takes", Some(body)).await;
        assert_eq!(
            (reply.status, reply.body),
            (422, json!({"detail": [expected]}))
        );
    }
    let saved: i64 = kass
        .db()
        .query_row("SELECT count(*) FROM take_reports", [], |row| row.get(0))
        .unwrap();
    assert_eq!(saved, 0);
}

// -- links and health -------------------------------------------------------------

fn location(reply: &Reply) -> &str {
    reply
        .head
        .lines()
        .find_map(|line| line.strip_prefix("location: "))
        .unwrap()
}

#[tokio::test]
async fn a_web_link_redirects_to_the_app() {
    let kass = Kass::start(false).await;
    let reply = kass
        .send("GET", "/open/captures?capture=abc-123", None)
        .await;
    assert_eq!(reply.status, 302);
    assert_eq!(location(&reply), "kass://captures?capture=abc-123");
    let reply = kass.send("GET", "/open/settings/dictation", None).await;
    assert_eq!(location(&reply), "kass://settings/dictation");
    let reply = kass.send("GET", "/open/styles/my%20style", None).await;
    assert_eq!(location(&reply), "kass://styles/my%20style");
}

#[tokio::test]
async fn an_internal_build_is_marked_by_a_file() {
    let kass = Kass::start(false).await;
    assert_eq!(
        kass.get("/internal").await,
        (200, json!({"internal": false}))
    );
    std::fs::write(kass.data.join("internal"), "").unwrap();
    assert_eq!(
        kass.get("/internal").await,
        (200, json!({"internal": true}))
    );
}

#[tokio::test]
async fn the_filesystem_check_writes_to_the_data_folders() {
    let kass = Kass::start(false).await;
    let (status, health) = kass.get("/health/filesystem").await;
    assert_eq!(status, 200);
    let data = std::fs::canonicalize(&kass.data).unwrap();
    assert_eq!(
        health["directories"],
        json!([
            {"path": data.join("captures").display().to_string(), "exists": true, "writable": true, "error": null},
            {"path": data.display().to_string(), "exists": true, "writable": true, "error": null},
        ])
    );
    assert!(kass.data.join("captures").is_dir());
    assert!(!kass.data.join(".kass_probe").exists());
    let free = health["disk_free_mb"].as_f64().unwrap();
    assert!(free > 0.0 && free <= health["disk_total_mb"].as_f64().unwrap());
    assert_eq!(health["healthy"], free >= 500.0);
}

#[tokio::test]
async fn the_app_may_ask_from_its_own_origin() {
    let kass = Kass::start(false).await;
    let mut stream = TcpStream::connect(kass.server.address()).await.unwrap();
    stream
        .write_all(
            b"OPTIONS /settings/captures HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\n\
              Access-Control-Request-Method: PUT\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response
        .to_lowercase()
        .contains("access-control-allow-origin: tauri://localhost"));
}
