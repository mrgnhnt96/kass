//! Learning corrections from the reports in a real kass.db, in front of a
//! stand-in worker that says whether it's idle, reloads the rules and runs
//! model improvement.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Default)]
struct Worker {
    idle: Arc<AtomicBool>,
    reloads: Arc<AtomicUsize>,
    /// Whether model improvement has adapters to go back to.
    adapters: Arc<AtomicBool>,
    rolled_back: Arc<AtomicUsize>,
}

async fn idle(State(worker): State<Worker>) -> Json<Value> {
    Json(json!({ "idle": worker.idle.load(Ordering::SeqCst) }))
}

async fn reload(State(worker): State<Worker>) -> Json<Value> {
    worker.reloads.fetch_add(1, Ordering::SeqCst);
    Json(json!({}))
}

async fn model(State(worker): State<Worker>) -> Json<Value> {
    Json(
        json!({ "phase": "idle", "can_rollback": worker.adapters.load(Ordering::SeqCst), "error": "an old run's" }),
    )
}

async fn model_rollback(State(worker): State<Worker>) -> Json<Value> {
    if !worker.adapters.swap(false, Ordering::SeqCst) {
        return Json(json!({ "refused": "No previous model version is available" }));
    }
    worker.rolled_back.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "phase": "rolled_back", "can_rollback": false }))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/learning/idle", post(idle))
        .route("/worker/learning/reload", post(reload))
        .route("/worker/learning/model", post(model))
        .route("/worker/learning/model/start", post(model))
        .route("/worker/learning/model/rollback", post(model_rollback))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

struct Kass {
    server: Option<kass_server::Running>,
    worker: Worker,
    data: PathBuf,
    reports: usize,
}

impl Kass {
    /// A kass.db with no reports and a worker that's `idle` or not; no
    /// server yet.
    fn new(idle: bool) -> Self {
        let data = std::env::temp_dir().join(format!("kass-learning-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let conn = rusqlite::Connection::open(data.join("kass.db")).unwrap();
        conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
        let worker = Worker::default();
        worker.idle.store(idle, Ordering::SeqCst);
        Kass {
            server: None,
            worker,
            data,
            reports: 0,
        }
    }

    async fn serve(&mut self) {
        self.server = Some(
            kass_server::start(kass_server::Config {
                listen: "127.0.0.1:0".parse().unwrap(),
                worker: start_worker(self.worker.clone()).await,
                database: self.data.join("kass.db"),
                shared_server: None,
                voice_model: None,
                models_dir: None,
                app_version: "0.0.0-test".into(),
            })
            .await
            .unwrap(),
        );
    }

    async fn start(idle: bool) -> Self {
        let mut kass = Kass::new(idle);
        kass.serve().await;
        kass
    }

    /// A report on take `capture` that `original` should have been
    /// `expected`; returns its id.
    fn report(
        &mut self,
        capture: &str,
        target: &str,
        original: &str,
        expected: &str,
        source: &str,
    ) -> String {
        self.reports += 1;
        let id = format!("report-{}", self.reports);
        let field = if target == "raw" {
            "transcript_raw"
        } else {
            "transcript_refined"
        };
        let snapshot = json!({ field: original, "language": null }).to_string();
        let conn = rusqlite::Connection::open(self.data.join("kass.db")).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO captures (id, audio_path, source, transcript_raw, audio_deleted)
             VALUES (?1, 'unused.wav', 'dictation', ?2, 0)",
            [capture, original],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot, source, created_at)
             VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7)",
            rusqlite::params![
                id,
                capture,
                target,
                expected,
                snapshot,
                source,
                format!("2026-03-01 10:00:{:02}.000000", self.reports)
            ],
        )
        .unwrap();
        id
    }

    /// Three takes of "voice box" fixed to "Voicebox": enough to learn it.
    fn voicebox(&mut self, target: &str) {
        for place in ["home", "work", "school"] {
            self.report(
                place,
                target,
                &format!("Please use voice box today at {place}."),
                &format!("Please use Voicebox today at {place}."),
                "manual",
            );
        }
    }

    fn withdraw(&self, report: &str) {
        let conn = rusqlite::Connection::open(self.data.join("kass.db")).unwrap();
        conn.execute("DELETE FROM capture_feedback WHERE id = ?1", [report])
            .unwrap();
    }

    fn sql(&self, sql: &str) {
        let conn = rusqlite::Connection::open(self.data.join("kass.db")).unwrap();
        conn.execute_batch(sql).unwrap();
    }

    fn saved(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.data.join("correction-learning.json")).unwrap(),
        )
        .unwrap()
    }

    fn reloads(&self) -> usize {
        self.worker.reloads.load(Ordering::SeqCst)
    }

    async fn call(&self, method: &str, path: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(self.server.as_ref().unwrap().address())
            .await
            .unwrap();
        stream
            .write_all(
                format!("{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8_lossy(&response);
        let status = response[9..12].parse().unwrap();
        let body = response.split_once("\r\n\r\n").unwrap().1;
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    async fn run(&self) -> Value {
        let (status, body) = self.call("POST", "/capture/learning/run").await;
        assert_eq!(status, 200, "{body}");
        body
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

#[tokio::test]
async fn learns_saves_reloads_and_rolls_back_for_good() {
    let mut kass = Kass::start(false).await;
    let (status, body) = kass.call("GET", "/capture/learning").await;
    assert_eq!(status, 200);
    assert_eq!(
        (body["active_rules"].clone(), body["outcome"].clone()),
        (json!(0), json!("waiting"))
    );
    assert_eq!(body["model"]["phase"], "idle");

    kass.voicebox("refined");
    let body = kass.run().await;
    assert_eq!(
        (body["active_rules"].clone(), body["revision"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(
        body["evaluated_report_ids"],
        json!(["report-1", "report-2", "report-3"])
    );
    assert_eq!(body["model"]["phase"], "idle");
    assert_eq!(kass.reloads(), 1);
    let saved = kass.saved();
    assert_eq!(saved["rules"][0]["replacement"], json!(["Voicebox"]));
    assert_eq!(saved["outcome"], "updated");
    // Nothing new: no change, nothing to reload.
    assert_eq!(kass.run().await["revision"], 1);
    assert_eq!(kass.reloads(), 1);

    let (status, body) = kass.call("POST", "/capture/learning/rollback").await;
    assert_eq!(status, 200);
    assert_eq!(body["active_rules"], 0);
    assert_eq!(kass.reloads(), 2);
    assert_eq!(kass.run().await["active_rules"], 0);
    // Another report on the same words still respects the rollback.
    kass.report(
        "noon",
        "refined",
        "Please use voice box today at noon.",
        "Please use Voicebox today at noon.",
        "manual",
    );
    assert_eq!(kass.run().await["active_rules"], 0);
    let (status, body) = kass.call("POST", "/capture/learning/rollback").await;
    assert_eq!(
        (status, body),
        (
            409,
            json!({ "detail": "No previous correction version is available" })
        )
    );
}

#[tokio::test]
async fn the_adapters_roll_back_before_the_corrections() {
    let mut kass = Kass::start(false).await;
    kass.voicebox("refined");
    kass.run().await;
    kass.worker.adapters.store(true, Ordering::SeqCst);
    let (status, body) = kass.call("POST", "/capture/learning/rollback").await;
    assert_eq!(status, 200);
    assert_eq!(kass.worker.rolled_back.load(Ordering::SeqCst), 1);
    assert_eq!(
        (body["active_rules"].clone(), body["model"]["phase"].clone()),
        (json!(1), json!("rolled_back"))
    );
    let (_, body) = kass.call("POST", "/capture/learning/rollback").await;
    assert_eq!(body["active_rules"], 0);
}

#[tokio::test]
async fn a_contradicting_report_blocks_the_rule_until_withdrawn() {
    let mut kass = Kass::start(false).await;
    kass.voicebox("refined");
    kass.run().await;
    let rule = kass.saved()["rules"][0]["id"].clone();
    let contradiction = kass.report(
        "outside",
        "refined",
        "Please use Voicebox today outside.",
        "Please use voice box today outside.",
        "voice_fix",
    );
    assert_eq!(kass.run().await["active_rules"], 0);
    assert_eq!(
        kass.saved()["blocked_by"],
        json!({ rule.as_str().unwrap(): [contradiction] })
    );

    kass.withdraw(&contradiction);
    assert_eq!(kass.run().await["active_rules"], 1);
    assert_eq!(kass.saved()["blocked"], json!([]));
}

#[tokio::test]
async fn withdrawn_support_drops_the_rule_without_blocking_it() {
    let mut kass = Kass::start(false).await;
    kass.voicebox("refined");
    assert_eq!(kass.run().await["active_rules"], 1);
    kass.withdraw("report-1");
    assert_eq!(kass.run().await["active_rules"], 0);
    assert_eq!(kass.saved()["blocked"], json!([]));
    kass.report(
        "noon",
        "refined",
        "Please use voice box today at noon.",
        "Please use Voicebox today at noon.",
        "manual",
    );
    assert_eq!(kass.run().await["active_rules"], 1);
}

#[tokio::test]
async fn raw_reports_teach_and_the_newest_teach_at_once() {
    let mut kass = Kass::start(false).await;
    for i in 0..6 {
        kass.report(
            &format!("old{i}"),
            "raw",
            &format!("Old text {i}."),
            &format!("Totally new {i}."),
            "manual",
        );
    }
    kass.sql("UPDATE capture_feedback SET created_at = '2020-01-01 00:00:00'");
    kass.voicebox("raw");
    assert_eq!(kass.run().await["active_rules"], 1);
}

#[tokio::test]
async fn the_latest_report_on_a_take_is_the_one_learned_from() {
    let mut kass = Kass::start(false).await;
    kass.voicebox("refined");
    // A newer report on "home" replaces the first, and keeps "voice box".
    kass.report(
        "home",
        "refined",
        "Please use voice box today at home.",
        "Please use voice box today at home!",
        "manual",
    );
    let body = kass.run().await;
    assert_eq!(body["active_rules"], 0);
    assert_eq!(
        body["evaluated_report_ids"],
        json!(["report-2", "report-3", "report-4"])
    );
}

#[tokio::test]
async fn a_state_without_evaluated_reports_waits_for_a_run() {
    let mut kass = Kass::start(false).await;
    kass.voicebox("refined");
    let ids = kass.run().await["evaluated_report_ids"].clone();
    let mut saved = kass.saved();
    saved
        .as_object_mut()
        .unwrap()
        .remove("evaluated_report_ids");
    std::fs::write(
        kass.data.join("correction-learning.json"),
        saved.to_string(),
    )
    .unwrap();
    assert_eq!(
        kass.call("GET", "/capture/learning").await.1["evaluated_report_ids"],
        json!([])
    );
    assert_eq!(kass.run().await["evaluated_report_ids"], ids);
}

#[tokio::test]
async fn learns_by_itself_only_while_the_models_are_idle() {
    let mut busy = Kass::new(false);
    busy.voicebox("refined");
    busy.serve().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!busy.data.join("correction-learning.json").exists());
    assert_eq!(busy.reloads(), 0);

    let mut idle = Kass::new(true);
    idle.voicebox("refined");
    idle.serve().await;
    // It checks as it starts, then every 15 seconds.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while idle.reloads() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "never learned");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(idle.saved()["rules"].as_array().unwrap().len(), 1);
}
