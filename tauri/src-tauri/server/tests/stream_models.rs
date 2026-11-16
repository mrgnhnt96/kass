//! The dictation session's model calls against a stand-in worker that
//! answers as `backend/routes/stream_models.py` does.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::stream;
use kass_server::proxy::Proxy;
use kass_server::stream::models::{self, CleanupPrompt, Generate, Recognize, RecognizeFile};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct Worker {
    running: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    foreground: Arc<AtomicI32>,
}

async fn recognize(body: Bytes) -> Json<Value> {
    let size = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
    let head: Value = serde_json::from_slice(&body[4..4 + size]).unwrap();
    let samples: Vec<i16> = body[4 + size..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect();
    let text = format!(
        "{samples:?} at {} after {:?} knowing {}",
        head["rate"],
        head["previous_text"].as_str().unwrap_or(""),
        head["vocabulary"]
    );
    let alignment = head["alignment"].as_bool().unwrap().then_some("7");
    Json(json!({"text": text, "alignment": alignment, "seconds": 0.25}))
}

async fn recognize_file(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"text": format!("all of {}", body["path"].as_str().unwrap()), "seconds": 3.0}))
}

async fn words(Path(key): Path<String>) -> Response {
    if key != "7" {
        return (StatusCode::NOT_FOUND, Json(json!({"detail": "gone"}))).into_response();
    }
    Json(json!({"words": [{"text": "Hi.", "start": 0.1, "end": 0.3}]})).into_response()
}

/// Partials split across chunks, as a socket may deliver them, until the
/// cleanup is stopped or written out.
async fn generate(State(worker): State<Worker>, Json(body): Json<Value>) -> Response {
    assert_eq!(body["system"], "Clean this up.");
    assert_eq!(body["examples"], json!([["um hi", "Hi."]]));
    assert_eq!(body["cache_key"], "cleanup:Chat");
    assert_eq!(body["flags"], json!({"style": "Chat"}));
    assert_eq!(body["hint"], "");
    if body["prompt"] == "break" {
        return Body::from("{\"error\": \"out of memory\"}\n").into_response();
    }
    let (sender, receiver) = tokio::sync::mpsc::channel::<String>(8);
    worker.stopped.store(false, Ordering::SeqCst);
    worker.running.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        let mut text = String::new();
        let mut stopped = false;
        for word in ["Hello", " there", "."] {
            tokio::time::sleep(Duration::from_millis(30)).await;
            if worker.stopped.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            text.push_str(word);
            let line = format!("{}\n", json!({"partial": text}));
            let (head, tail) = line.split_at(line.len() / 2);
            for chunk in [head, tail] {
                let _ = sender.send(chunk.to_owned()).await;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        worker.running.store(false, Ordering::SeqCst);
        let end = json!({"text": text, "model": "1.7B", "stopped": stopped, "seconds": 0.1});
        let _ = sender.send(format!("{end}\n")).await;
    });
    let chunks = stream::unfold(receiver, |mut receiver| async move {
        let chunk = receiver.recv().await?;
        Some((Ok::<_, Infallible>(chunk), receiver))
    });
    Body::from_stream(chunks).into_response()
}

async fn stop(State(worker): State<Worker>, Path(id): Path<String>) -> Json<Value> {
    let running = id == "phrase-1"
        && worker.running.load(Ordering::SeqCst)
        && !worker.stopped.swap(true, Ordering::SeqCst);
    Json(json!({"stopped": running}))
}

async fn rewrite(Json(body): Json<Value>) -> Response {
    if body["model_size"] == "4B" {
        let detail = json!({"detail": "Download Qwen3 4B in Models to use Command Mode"});
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(detail)).into_response();
    }
    Json(json!({"text": "Dear team,", "model": body["model_size"]})).into_response()
}

async fn ok() -> Json<Value> {
    Json(json!({"ok": true}))
}

async fn begin(State(worker): State<Worker>) -> Json<Value> {
    worker.foreground.fetch_add(1, Ordering::SeqCst);
    Json(json!({"ok": true}))
}

async fn end(State(worker): State<Worker>) -> Json<Value> {
    worker.foreground.fetch_sub(1, Ordering::SeqCst);
    Json(json!({"ok": true}))
}

async fn start_worker(worker: Worker) -> Proxy {
    let app = Router::new()
        .route("/worker/recognize", post(recognize))
        .route("/worker/recognize/file", post(recognize_file))
        .route("/worker/alignments/{key}/words", post(words))
        .route("/worker/cleanup/generate", post(generate))
        .route("/worker/cleanup/{id}/stop", post(stop))
        .route("/worker/cleanup/prefill", post(ok))
        .route("/worker/cleanup/load", post(ok))
        .route("/worker/command/prefill", post(ok))
        .route("/worker/command/rewrite", post(rewrite))
        .route("/worker/keep-resident", post(ok))
        .route("/worker/foreground/begin", post(begin))
        .route("/worker/foreground/end", post(end))
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Proxy::new(address)
}

fn cleanup(prompt: &str) -> Generate {
    Generate {
        id: "phrase-1".into(),
        prompt: prompt.into(),
        hint: Some(String::new()),
        max_tokens: 2048,
        temperature: 0.2,
        cleanup: CleanupPrompt {
            model_size: "1.7B".into(),
            flags: Some(json!({"style": "Chat"})),
            system: "Clean this up.".into(),
            examples: vec![("um hi".into(), "Hi.".into())],
            cache_key: Some("cleanup:Chat".into()),
        },
    }
}

#[tokio::test]
async fn a_phrase_goes_to_whisper_with_its_context() {
    let proxy = start_worker(Worker::default()).await;
    let request = Recognize {
        rate: 48_000,
        previous_text: Some("So".into()),
        vocabulary: vec!["Kass".into()],
        alignment: true,
        ..Default::default()
    };

    let answer = models::recognize(&proxy, &request, &[1, -2, 300])
        .await
        .unwrap();

    assert_eq!(
        answer.text,
        r#"[1, -2, 300] at 48000 after "So" knowing ["Kass"]"#
    );
    assert_eq!(answer.seconds, 0.25);
    let alignment = answer.alignment.unwrap();
    let words = models::alignment_words(&proxy, &alignment).await.unwrap();
    assert_eq!(words[0].text, "Hi.");
    assert_eq!((words[0].start, words[0].end), (0.1, 0.3));
    assert_eq!(
        models::alignment_words(&proxy, "8").await.unwrap_err(),
        "gone"
    );
}

#[tokio::test]
async fn the_whole_recording_is_the_fallback() {
    let proxy = start_worker(Worker::default()).await;
    let request = RecognizeFile {
        path: "/tmp/take.wav".into(),
        ..Default::default()
    };

    let answer = models::recognize_file(&proxy, &request).await.unwrap();

    assert_eq!(answer.text, "all of /tmp/take.wav");
    assert_eq!(answer.alignment, None);
}

#[tokio::test]
async fn a_cleanup_streams_its_text_as_it_grows() {
    let proxy = start_worker(Worker::default()).await;
    let mut partials = Vec::new();

    let done = models::generate(&proxy, &cleanup("hello there"), |text| {
        partials.push(text.to_owned())
    })
    .await
    .unwrap();

    assert_eq!(partials, ["Hello", "Hello there", "Hello there."]);
    assert_eq!(done.text, "Hello there.");
    assert_eq!(done.model, "1.7B");
    assert!(!done.stopped);
}

#[tokio::test]
async fn a_cleanup_stops_when_asked() {
    let worker = Worker::default();
    let proxy = start_worker(worker.clone()).await;
    let request = cleanup("hello there");
    let first = Notify::new();
    let mut partials = 0;

    let (done, stopped) = tokio::join!(
        models::generate(&proxy, &request, |_| {
            partials += 1;
            first.notify_one();
        }),
        async {
            first.notified().await;
            models::stop(&proxy, "phrase-1").await
        }
    );

    assert!(stopped);
    let done = done.unwrap();
    assert!(done.stopped);
    assert!(partials < 3);
    assert!(!models::stop(&proxy, "phrase-1").await);
}

#[tokio::test]
async fn a_failed_cleanup_says_why() {
    let proxy = start_worker(Worker::default()).await;

    let error = models::generate(&proxy, &cleanup("break"), |_| {})
        .await
        .unwrap_err();

    assert_eq!(error, "out of memory");
}

#[tokio::test]
async fn command_mode_rewrites_or_says_why_it_cant() {
    let proxy = start_worker(Worker::default()).await;

    models::command_prefill(&proxy, "hey all", "1.7B")
        .await
        .unwrap();
    let rewritten = models::command_rewrite(&proxy, "hey all", "formal", "1.7B")
        .await
        .unwrap();
    assert_eq!(rewritten, ("Dear team,".to_owned(), "1.7B".to_owned()));

    let error = models::command_rewrite(&proxy, "hey all", "formal", "4B")
        .await
        .unwrap_err();
    assert_eq!(error, "Download Qwen3 4B in Models to use Command Mode");
}

#[tokio::test]
async fn warming_calls_answer() {
    let proxy = start_worker(Worker::default()).await;
    let prompt = cleanup("").cleanup;

    models::prefill_cleanup(&proxy, &prompt).await.unwrap();
    models::load_cleanup(&proxy, "1.7B", prompt.flags.as_ref())
        .await
        .unwrap();
    models::keep_resident(&proxy, "dictation started", true, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn training_waits_until_the_dictation_ends_or_is_dropped() {
    let worker = Worker::default();
    let proxy = start_worker(worker.clone()).await;

    let dictation = models::begin_foreground(&proxy).await.unwrap();
    assert_eq!(worker.foreground.load(Ordering::SeqCst), 1);
    dictation.end().await;
    assert_eq!(worker.foreground.load(Ordering::SeqCst), 0);

    let dropped = models::begin_foreground(&proxy).await.unwrap();
    drop(dropped);
    for _ in 0..100 {
        if worker.foreground.load(Ordering::SeqCst) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a dropped dictation still holds training off");
}
