//! `/`, `/health`, `/shutdown` and `/capture/readiness`, against a real
//! kass.db and model folder, in front of a stand-in worker. Python:
//! `backend/routes/health.py` and `captures.py`'s readiness, which these
//! replaced.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

// -- a stand-in worker ---------------------------------------------------------

/// What the worker was asked, in order, and what its `/health` says.
#[derive(Clone)]
struct Worker {
    calls: Arc<Mutex<Vec<String>>>,
    health: Arc<Mutex<Value>>,
    /// The Kass server in front, to see whether it still listens when the
    /// worker is asked to stop.
    server: Arc<Mutex<Option<SocketAddr>>>,
}

impl Default for Worker {
    fn default() -> Self {
        Worker {
            calls: Arc::default(),
            health: Arc::new(Mutex::new(json!({
                "status": "healthy",
                "model_loaded": true,
                "model_downloaded": true,
                "model_size": "turbo",
                "gpu_available": true,
                "gpu_type": "Metal (Apple Silicon via MLX)",
                "backend_type": "mlx",
                "version": "worker's own",
                "started_at": 1_763_000_000.25,
                "pid": 4242,
                "peak_memory_mb": 1800,
            }))),
            server: Arc::default(),
        }
    }
}

async fn health(State(worker): State<Worker>) -> Json<Value> {
    worker.calls.lock().unwrap().push("health".into());
    Json(worker.health.lock().unwrap().clone())
}

async fn shutdown(State(worker): State<Worker>) -> Json<Value> {
    let server = worker.server.lock().unwrap().unwrap();
    let listening = TcpStream::connect(server).await.is_ok();
    worker
        .calls
        .lock()
        .unwrap()
        .push(format!("shutdown (server listening: {listening})"));
    Json(json!({ "message": "Shutting down..." }))
}

/// A slow model call, proxied as every path no module answers is.
async fn slow(State(worker): State<Worker>) -> Json<Value> {
    tokio::time::sleep(Duration::from_millis(600)).await;
    worker.calls.lock().unwrap().push("slow done".into());
    Json(json!({ "done": true }))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/health", get(health))
        .route("/shutdown", post(shutdown))
        .route("/slow", post(slow))
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

// -- the server ----------------------------------------------------------------

struct Kass {
    server: kass_server::Running,
    worker: Worker,
    data: PathBuf,
    cache: PathBuf,
    client: reqwest::Client,
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kass-health-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A model as huggingface_hub leaves it, with one weights file named `file`.
fn downloaded(cache: &Path, repo: &str, file: &str) {
    let folder = cache.join(format!("models--{}", repo.replace('/', "--")));
    std::fs::create_dir_all(folder.join("blobs")).unwrap();
    std::fs::create_dir_all(folder.join("snapshots/abc")).unwrap();
    std::fs::write(folder.join("blobs/w"), b"weights").unwrap();
    std::os::unix::fs::symlink("../../blobs/w", folder.join("snapshots/abc").join(file)).unwrap();
}

impl Kass {
    async fn start(worker: Worker) -> Self {
        let data = temp("data");
        let cache = temp("cache");
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database: data.join("kass.db"),
            shared_server: None,
            voice_model: None,
            models_dir: Some(cache.clone()),
            app_version: "1.2.3-beta.4".into(),
        })
        .await
        .unwrap();
        *worker.server.lock().unwrap() = Some(server.address());
        Kass {
            server,
            worker,
            data,
            cache,
            client: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.address())
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let response = self.client.get(self.url(path)).send().await.unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }

    async fn post(&self, path: &str) -> (u16, Value) {
        let response = self.client.post(self.url(path)).send().await.unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }

    /// Choose models as an older Kass might have saved them, past the
    /// checks `PUT /settings/captures` makes.
    async fn choose(&self, stt: &str, llm: &str) {
        // Reading the settings saves the defaults first.
        assert_eq!(self.get("/settings/captures").await.0, 200);
        rusqlite::Connection::open(self.data.join("kass.db"))
            .unwrap()
            .execute(
                "UPDATE capture_settings SET stt_model = ?1, llm_model = ?2",
                [stt, llm],
            )
            .unwrap();
    }

    fn calls(&self) -> Vec<String> {
        self.worker.calls.lock().unwrap().clone()
    }
}

// -- / ---------------------------------------------------------------------------

#[tokio::test]
async fn the_root_names_kass_and_its_version() {
    let kass = Kass::start(Worker::default()).await;
    let (status, body) = kass.get("/").await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({ "message": "kass API", "version": "1.2.3-beta.4" })
    );
    assert!(kass.calls().is_empty(), "{:?}", kass.calls());
}

// -- /capture/readiness ------------------------------------------------------------

#[tokio::test]
async fn readiness_says_which_chosen_models_are_downloaded() {
    let kass = Kass::start(Worker::default()).await;
    let (status, body) = kass.get("/capture/readiness").await;
    assert_eq!(status, 200);
    // The defaults: Whisper Turbo and Qwen3 0.6B, neither downloaded.
    assert_eq!(
        body,
        json!({
            "stt": {
                "ready": false,
                "model_name": "whisper-turbo",
                "display_name": "Whisper Turbo",
                "size": "turbo",
                "size_mb": 1620,
            },
            "llm": {
                "ready": false,
                "model_name": "qwen3-0.6b",
                "display_name": "Qwen3 0.6B",
                "size": "0.6B",
                "size_mb": 400,
            },
        })
    );

    downloaded(
        &kass.cache,
        "openai/whisper-large-v3-turbo",
        "model.safetensors",
    );
    let (_, body) = kass.get("/capture/readiness").await;
    assert_eq!(body["stt"]["ready"], true);
    assert_eq!(body["llm"]["ready"], false);

    // Readiness counts .safetensors and .bin weights only, as Python did.
    kass.choose("small", "4B").await;
    downloaded(&kass.cache, "openai/whisper-small", "weights.npz");
    downloaded(&kass.cache, "mlx-community/Qwen3-4B-4bit", "model.bin");
    let (_, body) = kass.get("/capture/readiness").await;
    assert_eq!(body["stt"]["model_name"], "whisper-small");
    assert_eq!(body["stt"]["ready"], false);
    assert_eq!(body["llm"]["model_name"], "qwen3-4b");
    assert_eq!(body["llm"]["ready"], true);
    assert!(kass.calls().is_empty(), "{:?}", kass.calls());
}

#[tokio::test]
async fn readiness_refuses_a_model_it_does_not_know() {
    let kass = Kass::start(Worker::default()).await;
    kass.choose("tiny", "0.6B").await;
    let (status, body) = kass.get("/capture/readiness").await;
    assert_eq!(status, 500);
    assert_eq!(
        body,
        json!({ "detail": "No model config for stt=tiny or llm=0.6B" })
    );
}

// -- /health -----------------------------------------------------------------------

#[tokio::test]
async fn health_reports_the_worker_and_the_chosen_whisper_model() {
    let kass = Kass::start(Worker::default()).await;
    let response = kass.client.get(kass.url("/health")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    // In HealthResponse's order.
    assert_eq!(
        text,
        "{\"status\":\"healthy\",\"model_loaded\":true,\"model_downloaded\":false,\
         \"model_size\":\"turbo\",\"gpu_available\":true,\
         \"gpu_type\":\"Metal (Apple Silicon via MLX)\",\"backend_type\":\"mlx\",\
         \"version\":\"1.2.3-beta.4\",\"started_at\":1763000000.25,\"pid\":4242,\
         \"peak_memory_mb\":1800}"
    );

    // Downloaded counts .npz weights for Whisper too.
    downloaded(&kass.cache, "openai/whisper-large-v3-turbo", "weights.npz");
    let (_, body) = kass.get("/health").await;
    assert_eq!(body["model_downloaded"], true);
}

#[tokio::test]
async fn health_with_no_whisper_loaded_names_no_size() {
    let worker = Worker::default();
    {
        let mut health = worker.health.lock().unwrap();
        health["model_loaded"] = json!(false);
        health["model_size"] = json!("turbo");
    }
    let kass = Kass::start(worker).await;
    kass.choose("tiny", "0.6B").await;
    let (status, body) = kass.get("/health").await;
    assert_eq!(status, 200);
    assert_eq!(body["model_loaded"], false);
    assert_eq!(body["model_size"], Value::Null);
    // No Whisper model by that size, so nothing to say about its files.
    assert_eq!(body["model_downloaded"], Value::Null);
}

#[tokio::test]
async fn health_without_a_worker_is_a_bad_gateway() {
    // Nothing listens on a port just freed.
    let free = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let data = temp("no-worker");
    let server = kass_server::start(kass_server::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        worker: free,
        database: data.join("kass.db"),
        shared_server: None,
        voice_model: None,
        models_dir: Some(temp("no-worker-cache")),
        app_version: "1.2.3".into(),
    })
    .await
    .unwrap();
    let response = reqwest::get(format!("http://{}/health", server.address()))
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({ "detail": "The Kass model worker isn't answering" })
    );
}

// -- /shutdown ---------------------------------------------------------------------

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..100 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what} never happened");
}

#[tokio::test]
async fn shutdown_stops_listening_then_stops_the_worker() {
    let kass = Kass::start(Worker::default()).await;
    assert!(!kass.server.stopped());
    let (status, body) = kass.post("/shutdown").await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "message": "Shutting down..." }));

    wait_until("the worker asked to stop", || {
        kass.calls().iter().any(|call| call.starts_with("shutdown"))
    })
    .await;
    assert_eq!(kass.calls(), ["shutdown (server listening: false)"]);
    assert!(kass.server.stopped());
    assert!(TcpStream::connect(kass.server.address()).await.is_err());
}

#[tokio::test]
async fn shutdown_lets_a_running_request_finish_before_the_worker_stops() {
    let kass = Kass::start(Worker::default()).await;
    let slow = {
        let request = kass.client.post(kass.url("/slow")).send();
        tokio::spawn(request)
    };
    // The slow request is with the worker before shutdown comes.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, _) = kass.post("/shutdown").await;
    assert_eq!(status, 200);

    let answer = slow.await.unwrap().unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(
        answer.json::<Value>().await.unwrap(),
        json!({ "done": true })
    );
    wait_until("the worker asked to stop", || kass.calls().len() == 2).await;
    assert_eq!(
        kass.calls(),
        ["slow done", "shutdown (server listening: false)"]
    );
}
