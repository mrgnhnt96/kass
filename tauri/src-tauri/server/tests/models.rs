//! Model downloads, status, storage and the task list, in front of a
//! stand-in worker that says what's loaded and a stand-in Hugging Face.
//! Python: backend/routes/models.py and tasks.py, which these replaced.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::net::TcpListener;

const WEIGHTS: &[u8] = b"pretend these are weights";

// -- a stand-in Hugging Face, shared by every test ---------------------------

async fn listing() -> Json<Value> {
    Json(json!({
        "sha": "c0ffee",
        "siblings": [
            { "rfilename": "config.json", "blobId": "cf01", "size": 2 },
            { "rfilename": "model.safetensors", "blobId": "b33b", "size": 134,
              "lfs": { "sha256": "5eed", "size": WEIGHTS.len(), "pointerSize": 134 } },
        ]
    }))
}

async fn resolve(UrlPath((_, _, file)): UrlPath<(String, String, String)>) -> Response {
    match file.as_str() {
        "config.json" => b"{}".to_vec().into_response(),
        "model.safetensors" => WEIGHTS.to_vec().into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Started once on a thread of its own, so it outlives each test's runtime,
/// and named in `HF_ENDPOINT` before any server starts.
fn hub() {
    static HUB: OnceLock<SocketAddr> = OnceLock::new();
    HUB.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let app = Router::new()
                    .route("/api/models/{org}/{name}/revision/main", get(listing))
                    .route("/{org}/{name}/resolve/c0ffee/{*file}", get(resolve));
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                sender.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        let address = receiver.recv().unwrap();
        std::env::set_var("HF_ENDPOINT", format!("http://{address}"));
        address
    });
}

// -- a stand-in worker ---------------------------------------------------------

/// What the worker was asked, and what's loaded in it.
#[derive(Clone, Default)]
struct Worker {
    calls: Arc<Mutex<Vec<String>>>,
    loaded: Arc<Mutex<Vec<String>>>,
}

async fn loaded(State(worker): State<Worker>) -> Json<Value> {
    Json(json!({ "loaded": *worker.loaded.lock().unwrap() }))
}

async fn load(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    let name = body["model_name"].as_str().unwrap().to_string();
    worker.calls.lock().unwrap().push(format!("load {name}"));
    if name == "qwen3-4b" {
        return Json(json!({ "error": "Not enough memory" }));
    }
    worker.loaded.lock().unwrap().push(name);
    Json(json!({}))
}

async fn unload(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    let name = body["model_name"].as_str().unwrap().to_string();
    worker.calls.lock().unwrap().push(format!("unload {name}"));
    let mut loaded = worker.loaded.lock().unwrap();
    let was_loaded = loaded.contains(&name);
    loaded.retain(|model| *model != name);
    Json(json!({ "was_loaded": was_loaded }))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/models/loaded", post(loaded))
        .route("/worker/models/load", post(load))
        .route("/worker/models/unload", post(unload))
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
    /// The models folder the app chose.
    cache: PathBuf,
    client: reqwest::Client,
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kass-models-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// A downloaded model as huggingface_hub leaves it.
fn downloaded(cache: &Path, repo: &str) {
    let folder = cache.join(format!("models--{}", repo.replace('/', "--")));
    std::fs::create_dir_all(folder.join("blobs")).unwrap();
    std::fs::create_dir_all(folder.join("snapshots/abc")).unwrap();
    std::fs::write(folder.join("blobs/w"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    std::os::unix::fs::symlink(
        "../../blobs/w",
        folder.join("snapshots/abc/model.safetensors"),
    )
    .unwrap();
}

impl Kass {
    async fn start() -> Self {
        hub();
        let worker = Worker::default();
        let cache = temp("cache");
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database: std::env::temp_dir().join("kass-models-test-unused.db"),
            shared_server: None,
            voice_model: None,
            models_dir: Some(cache.clone()),
        })
        .await
        .unwrap();
        Kass {
            server,
            worker,
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

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }

    async fn delete(&self, path: &str) -> (u16, Value) {
        let response = self.client.delete(self.url(path)).send().await.unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }

    /// The events of a progress stream until it ends, heartbeats left out.
    async fn events(&self, path: &str) -> Vec<Value> {
        let response = self.client.get(self.url(path)).send().await.unwrap();
        assert_eq!(
            response.headers()["content-type"],
            "text/event-stream; charset=utf-8"
        );
        let mut body = response.bytes_stream();
        let mut text = String::new();
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(chunk) = body.next().await {
                text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            }
        })
        .await
        .expect("the stream ends once the download does");
        text.split("\n\n")
            .filter_map(|event| event.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap())
            .collect()
    }

    fn calls(&self) -> Vec<String> {
        self.worker.calls.lock().unwrap().clone()
    }
}

fn status_of<'a>(models: &'a Value, name: &str) -> &'a Value {
    models["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["model_name"] == name)
        .unwrap()
}

// -- tests ---------------------------------------------------------------------

#[tokio::test]
async fn status_lists_every_model_with_what_is_on_disk_and_loaded() {
    let kass = Kass::start().await;
    downloaded(&kass.cache, "openai/whisper-large-v3-turbo");
    kass.worker
        .loaded
        .lock()
        .unwrap()
        .push("whisper-turbo".into());

    let (code, models) = kass.get("/models/status").await;
    assert_eq!(code, 200);
    let names: Vec<&str> = models["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["model_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "whisper-base",
            "whisper-small",
            "whisper-medium",
            "whisper-large",
            "whisper-turbo",
            "qwen3-0.6b",
            "qwen3-1.7b",
            "qwen3-4b"
        ]
    );
    assert_eq!(
        *status_of(&models, "whisper-turbo"),
        json!({
            "model_name": "whisper-turbo",
            "display_name": "Whisper Turbo",
            "hf_repo_id": "openai/whisper-large-v3-turbo",
            "downloaded": true,
            "downloading": false,
            "size_mb": 2.0,
            "loaded": true,
        })
    );
    assert_eq!(
        *status_of(&models, "qwen3-4b"),
        json!({
            "model_name": "qwen3-4b",
            "display_name": "Qwen3 4B",
            "hf_repo_id": "mlx-community/Qwen3-4B-4bit",
            "downloaded": false,
            "downloading": false,
            "size_mb": null,
            "loaded": false,
        })
    );

    let (_, dir) = kass.get("/models/cache-dir").await;
    assert_eq!(dir, json!({ "path": kass.cache.to_string_lossy() }));
    let (_, space) = kass.get("/models/disk-space").await;
    assert!(space["free_mb"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn a_download_streams_its_progress_then_loads_the_model() {
    let kass = Kass::start().await;
    let (code, answer) = kass
        .post("/models/download", json!({ "model_name": "whisper-base" }))
        .await;
    assert_eq!(code, 200);
    assert_eq!(
        answer,
        json!({ "message": "Model whisper-base download started" })
    );

    let events = kass.events("/models/progress/whisper-base").await;
    let last = events.last().unwrap();
    assert_eq!(last["status"], "complete");
    assert_eq!(last["progress"], 100.0);
    assert_eq!(last["model_name"], "whisper-base");
    assert_eq!(kass.calls(), ["load whisper-base"]);

    let snapshot = kass
        .cache
        .join("models--openai--whisper-base/snapshots/c0ffee");
    assert_eq!(
        std::fs::read(snapshot.join("model.safetensors")).unwrap(),
        WEIGHTS
    );
    let (_, models) = kass.get("/models/status").await;
    let base = status_of(&models, "whisper-base");
    assert_eq!(base["downloaded"], true);
    assert_eq!(base["loaded"], true);
    assert_eq!(
        kass.get("/tasks/active").await.1,
        json!({ "downloads": [] })
    );
}

#[tokio::test]
async fn a_failed_download_stays_listed_until_dismissed() {
    let kass = Kass::start().await;
    kass.post("/models/download", json!({ "model_name": "qwen3-4b" }))
        .await;
    let events = kass.events("/models/progress/qwen3-4b").await;
    let last = events.last().unwrap();
    assert_eq!(last["status"], "error");
    assert_eq!(last["error"], "Not enough memory");

    let (_, active) = kass.get("/tasks/active").await;
    let task = &active["downloads"][0];
    assert_eq!(task["model_name"], "qwen3-4b");
    assert_eq!(task["status"], "error");
    assert_eq!(task["error"], "Not enough memory");
    assert!(task["started_at"].as_str().unwrap().contains('T'));
    for field in ["progress", "current", "total", "filename"] {
        assert!(task.get(field).is_some(), "{field}");
    }
    let (_, models) = kass.get("/models/status").await;
    assert_eq!(status_of(&models, "qwen3-4b")["downloading"], false);

    let cancel = json!({ "model_name": "qwen3-4b" });
    assert_eq!(
        kass.post("/models/download/cancel", cancel.clone()).await.1,
        json!({ "message": "Download task for qwen3-4b cancelled" })
    );
    assert_eq!(
        kass.post("/models/download/cancel", cancel).await.1,
        json!({ "message": "No active task found for qwen3-4b" })
    );
    assert_eq!(
        kass.get("/tasks/active").await.1,
        json!({ "downloads": [] })
    );
}

#[tokio::test]
async fn unknown_models_and_missing_fields_are_refused() {
    let kass = Kass::start().await;
    assert_eq!(
        kass.post("/models/download", json!({ "model_name": "whisper-tiny" }))
            .await,
        (400, json!({ "detail": "Unknown model: whisper-tiny" }))
    );
    let (code, problems) = kass.post("/models/download", json!({})).await;
    assert_eq!(code, 422);
    assert_eq!(
        problems["detail"][0],
        json!({ "type": "missing", "loc": ["body", "model_name"], "msg": "Field required", "input": {} })
    );
    assert_eq!(
        kass.post("/models/nope/unload", json!({})).await,
        (400, json!({ "detail": "Unknown model: nope" }))
    );
    assert_eq!(
        kass.delete("/models/nope").await,
        (400, json!({ "detail": "Unknown model: nope" }))
    );
    assert_eq!(
        kass.post("/tasks/clear", json!({})).await.1,
        json!({ "message": "All task state cleared" })
    );
}

#[tokio::test]
async fn unloading_and_deleting_ask_the_worker_first() {
    let kass = Kass::start().await;
    downloaded(&kass.cache, "mlx-community/Qwen3-0.6B-4bit");
    kass.worker.loaded.lock().unwrap().push("qwen3-0.6b".into());

    assert_eq!(
        kass.post("/models/qwen3-0.6b/unload", json!({})).await.1,
        json!({ "message": "Model qwen3-0.6b unloaded successfully" })
    );
    assert_eq!(
        kass.post("/models/qwen3-0.6b/unload", json!({})).await.1,
        json!({ "message": "Model qwen3-0.6b is not loaded" })
    );
    assert_eq!(
        kass.delete("/models/qwen3-0.6b").await,
        (
            200,
            json!({ "message": "Model qwen3-0.6b deleted successfully" })
        )
    );
    assert!(!kass
        .cache
        .join("models--mlx-community--Qwen3-0.6B-4bit")
        .exists());
    assert_eq!(
        kass.delete("/models/qwen3-0.6b").await,
        (
            404,
            json!({ "detail": "Model qwen3-0.6b not found in cache" })
        )
    );
    assert_eq!(kass.calls().len(), 4);
}

#[tokio::test]
async fn migrating_moves_every_model_and_reports_it_done() {
    let kass = Kass::start().await;
    let cache = kass.cache.to_string_lossy().into_owned();
    assert_eq!(
        kass.post("/models/migrate", json!({ "destination": cache }))
            .await,
        (
            400,
            json!({ "detail": "Source and destination are the same directory" })
        )
    );
    let inside = kass.cache.join("inner").to_string_lossy().into_owned();
    assert_eq!(
        kass.post("/models/migrate", json!({ "destination": inside }))
            .await,
        (
            400,
            json!({ "detail": "Destination cannot be inside the current cache directory" })
        )
    );
    let destination = temp("to").join("models");
    let destination_shown = destination.to_string_lossy().into_owned();
    assert_eq!(
        kass.post(
            "/models/migrate",
            json!({ "destination": destination_shown })
        )
        .await
        .1,
        json!({ "moved": 0, "errors": [], "source": cache, "destination": destination_shown })
    );

    downloaded(&kass.cache, "openai/whisper-small");
    downloaded(&kass.cache, "openai/whisper-medium");
    // Listen first: a move on one volume can finish before a late listener
    // connects, and it hears only what comes after.
    let listening = kass.events("/models/migrate/progress");
    let started = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        kass.post(
            "/models/migrate",
            json!({ "destination": destination_shown }),
        )
        .await
    };
    let (events, (code, answer)) = tokio::join!(listening, started);
    assert_eq!(code, 200);
    assert_eq!(
        answer,
        json!({ "source": cache, "destination": destination_shown })
    );
    assert_eq!(events.last().unwrap()["status"], "complete");
    assert!(destination
        .join("models--openai--whisper-medium/snapshots/abc/model.safetensors")
        .is_file());
    assert!(!kass.cache.join("models--openai--whisper-small").exists());
}
