//! Learning corrections from the reports in a real kass.db, in front of a
//! stand-in worker that says whether it's idle, reloads the rules and trains
//! the models model improvement asks for.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Model improvement has a run ready to train, which makes `trained`.
    ready: Arc<AtomicBool>,
    /// The trained models a run makes.
    trained: Arc<Mutex<Value>>,
    /// The trained models the worker was told to use, in order.
    used: Arc<Mutex<Vec<Value>>>,
    stops: Arc<AtomicUsize>,
    /// Stops asked for because the user started recording.
    recordings: Arc<AtomicUsize>,
    /// Each prepare's body, with the reports and dictations the worker read
    /// back from the server while preparing.
    prepared: Arc<Mutex<Vec<Value>>>,
}

async fn idle(State(worker): State<Worker>) -> Json<Value> {
    Json(json!({ "idle": worker.idle.load(Ordering::SeqCst), "dictating": false }))
}

async fn reload(State(worker): State<Worker>) -> Json<Value> {
    worker.reloads.fetch_add(1, Ordering::SeqCst);
    Json(json!({}))
}

/// What the worker reads from kass.db through the server at `path`.
async fn ask_server(body: &Value, path: &str, request: Value) -> Value {
    let server = body["cleanup_server"].as_str().unwrap();
    reqwest::Client::new()
        .post(format!("{server}{path}"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn prepare(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    let reports = ask_server(
        &body,
        "/capture/learning/training/reports",
        json!({ "limit": 1000 }),
    )
    .await;
    let dictations = ask_server(
        &body,
        "/capture/learning/training/dictations",
        json!({ "after": null, "limit": 400 }),
    )
    .await;
    let corrections = ask_server(
        &body,
        "/capture/learning/training/corrections",
        json!({ "capture_ids": ["home"] }),
    )
    .await;
    worker.prepared.lock().unwrap().push(json!({
        "body": body, "reports": reports, "dictations": dictations, "corrections": corrections,
    }));
    let ready = worker.ready.load(Ordering::SeqCst);
    Json(json!({
        "groups": [], "counts": { "reports": 3 }, "evaluated_report_ids": ["report-1"],
        "ready": ready, "waiting": if ready { Value::Null } else { json!("waiting_data") },
        "fingerprint": if ready { json!("f1") } else { Value::Null },
        "run": if ready { json!("run-1") } else { Value::Null },
        "trains": true, "generation": 4, "interrupted": false,
    }))
}

async fn train(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(
        (body["run"].clone(), body["generation"].clone()),
        (json!("run-1"), json!(4))
    );
    let active = worker.trained.lock().unwrap().clone();
    Json(
        json!({ "active": active, "metrics": { "adapter": { "passed": true } }, "voice_resume": null }),
    )
}

/// The trained models all pass their checks.
async fn check(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({ "active": body["active"] }))
}

async fn use_models(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker.used.lock().unwrap().push(body["active"].clone());
    Json(json!({ "active": body["active"] }))
}

async fn stop(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker.stops.fetch_add(1, Ordering::SeqCst);
    if body["recording"] == true {
        worker.recordings.fetch_add(1, Ordering::SeqCst);
    }
    Json(json!({}))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/learning/idle", post(idle))
        .route("/worker/learning/reload", post(reload))
        .route("/worker/learning/model/prepare", post(prepare))
        .route("/worker/learning/model/train", post(train))
        .route("/worker/learning/model/check", post(check))
        .route("/worker/learning/model/use", post(use_models))
        .route("/worker/learning/model/stop", post(stop))
        .route(
            "/worker/learning/model/reports",
            post(|| async { Json(json!({ "reports": [] })) }),
        )
        .route(
            "/worker/learning/model/voice",
            post(|| async { Json(json!({ "bank": { "train": 0 } })) }),
        )
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

    /// The learning status once model improvement's run has ended.
    async fn settled(&self) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let (status, body) = self.call("GET", "/capture/learning").await;
            assert_eq!(status, 200, "{body}");
            if body["model"]["running"] == false {
                return body;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the run never ended"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn model_state(&self) -> Value {
        let path = self.data.join("model-improvement/state.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn write_model_state(&self, state: Value) {
        let dir = self.data.join("model-improvement");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), state.to_string()).unwrap();
    }

    fn used(&self) -> Vec<Value> {
        self.worker.used.lock().unwrap().clone()
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
    assert_eq!(body["model"]["phase"], "waiting");

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
    assert_eq!(kass.settled().await["model"]["phase"], "waiting_data");
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
    let mut kass = Kass::new(false);
    kass.write_model_state(json!({
        "version": 1,
        "revision": 3,
        "active": { "llm": { "id": "new", "path": "/runs/new/adapter" } },
        "history": [{ "active": { "llm": { "id": "old", "path": "/runs/old/adapter" } }, "fingerprint": "f" }],
    }));
    kass.serve().await;
    kass.voicebox("refined");
    kass.run().await;
    kass.settled().await;
    let (status, body) = kass.call("POST", "/capture/learning/rollback").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        (body["active_rules"].clone(), body["model"]["phase"].clone()),
        (json!(1), json!("rolled_back"))
    );
    assert_eq!(body["model"]["active_adapter"], "old");
    assert_eq!(body["model"]["can_rollback"], false);
    assert_eq!(
        kass.used(),
        [json!({ "llm": { "id": "old", "path": "/runs/old/adapter" } })]
    );
    let saved = kass.model_state();
    assert_eq!(
        (saved["revision"].clone(), saved["blocked"].clone()),
        (json!(4), json!(["f"]))
    );
    // The run going is stopped first.
    assert_eq!(kass.worker.stops.load(Ordering::SeqCst), 1);

    let (_, body) = kass.call("POST", "/capture/learning/rollback").await;
    assert_eq!(body["active_rules"], 0);
}

#[tokio::test]
async fn a_trained_model_that_does_better_is_used_and_kept() {
    let kass = Kass::start(false).await;
    kass.worker.ready.store(true, Ordering::SeqCst);
    *kass.worker.trained.lock().unwrap() =
        json!({ "llm": { "id": "run-1", "path": "/runs/run-1/adapter" } });
    kass.run().await;
    let body = kass.settled().await["model"].clone();
    assert_eq!(
        (
            body["phase"].clone(),
            body["active_adapter"].clone(),
            body["can_rollback"].clone()
        ),
        (json!("activated"), json!("run-1"), json!(true))
    );
    assert_eq!(body["counts"], json!({ "reports": 3 }));
    assert_eq!(
        kass.used(),
        [json!({ "llm": { "id": "run-1", "path": "/runs/run-1/adapter" } })]
    );
    let saved = kass.model_state();
    assert_eq!(saved["attempted"], json!(["f1"]));
    assert_eq!(saved["evaluated_report_ids"], json!(["report-1"]));
    assert_eq!(
        saved["history"],
        json!([{ "active": {}, "fingerprint": "f1" }])
    );

    // The same reports again: nothing new to train on.
    kass.run().await;
    assert_eq!(kass.settled().await["model"]["phase"], "no_new_data");
    assert_eq!(kass.used().len(), 1);

    // Cancelling answers the status.
    let (status, body) = kass.call("POST", "/capture/learning/cancel").await;
    assert_eq!((status, body["phase"].clone()), (200, json!("no_new_data")));
}

#[tokio::test]
async fn the_worker_plans_from_the_settings_styles_and_rows_the_server_reads() {
    let mut kass = Kass::start(false).await;
    kass.voicebox("refined");
    kass.report("home", "raw", "voice box", "Voicebox", "redictation");
    kass.sql(
        "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
             capitalize_first, smart_cleanup, preserve_technical, description)
         VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1, ''),
                ('chat', 'Chat', 1, 0, 'casual', 1, 1, 0, '');",
    );
    kass.run().await;
    kass.settled().await;
    let prepared = kass.worker.prepared.lock().unwrap()[0].clone();
    let body = &prepared["body"];
    assert_eq!(body["settings"]["llm_model"], json!("0.6B"));
    assert!(body["settings"]["stt_model"].is_string());
    assert!(body["settings"]["language"].is_string());
    assert_eq!(body["settings"]["discard_audio"], json!(false));
    assert_eq!(
        body["style_flags"],
        json!([
            { "smart_cleanup": true, "self_correction": true, "preserve_technical": true, "style": "personal" },
            {
                "smart_cleanup": true, "self_correction": true, "preserve_technical": false,
                "punctuation_style": "casual", "style": "chat",
            },
        ])
    );
    let ids = |rows: &Value| -> Vec<String> {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        ids(&prepared["reports"]["reports"]),
        ["report-4", "report-3", "report-2", "report-1"]
    );
    assert_eq!(
        ids(&prepared["dictations"]["dictations"]),
        ["home", "work", "school"]
    );
    // A re-dictation teaches no voice correction.
    assert_eq!(ids(&prepared["corrections"]["corrections"]), ["report-1"]);
}

#[tokio::test]
async fn recording_tells_the_worker_and_the_server_reports_the_sounds() {
    let kass = Kass::start(false).await;
    let (status, body) = kass.call("POST", "/capture/learning/activity").await;
    assert_eq!((status, body), (200, json!({ "paused": true })));
    assert_eq!(kass.worker.recordings.load(Ordering::SeqCst), 1);

    // No sounds downloaded in this data folder.
    let (_, body) = kass.call("GET", "/capture/learning").await;
    assert_eq!(
        body["model"]["voice"]["sounds"],
        json!({ "state": "missing", "fraction": 0.0, "error": null })
    );
    assert_eq!(body["model"]["voice"]["bank"], json!({ "train": 0 }));
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
