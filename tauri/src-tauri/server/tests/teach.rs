//! Teach sessions against a real kass.db, in front of a stand-in worker that
//! writes the other side's turns.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const GOOD_TURN: &str = "MESSAGE: nice. can you post the release notes too?\n\
                         TELL: Post notes? -> Yes, Thursday | Who else? → Tag Jess";

#[derive(Clone, Default)]
struct Worker {
    /// What the next teach turns write, in order; then GOOD_TURN.
    turns: Arc<Mutex<VecDeque<String>>>,
    /// Each `/worker/cleanup/generate` body.
    generated: Arc<Mutex<Vec<Value>>>,
}

async fn generate(State(worker): State<Worker>, Json(body): Json<Value>) -> String {
    worker.generated.lock().unwrap().push(body.clone());
    let text = if body["cache_key"] == "teach" {
        let next = worker.turns.lock().unwrap().pop_front();
        next.unwrap_or_else(|| GOOD_TURN.to_string())
    } else {
        // A preview's cleanup: what was said, marked as cleaned.
        format!("Cleaned: {}", body["prompt"].as_str().unwrap())
    };
    let done = json!({"text": text, "model": "4B", "stopped": false, "seconds": 0.1});
    format!("{done}\n")
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/cleanup/generate", post(generate))
        .fallback(|| async { Json(json!({"ok": true})) })
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A kass.db with the default style and a chat style WhatsApp uses.
fn database() -> PathBuf {
    // Its own folder: the learned files are written next to it.
    let dir = std::env::temp_dir().join(format!("kass-teach-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kass.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
    conn.execute_batch(
        "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
             capitalize_first, smart_cleanup, preserve_technical)
         VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1),
                ('chat', 'Chat', 1, 0, 'standard', 1, 1, 1);
         INSERT INTO app_styles (bundle_id, style_id) VALUES ('net.whatsapp.WhatsApp', 'chat');",
    )
    .unwrap();
    path
}

struct Kass {
    server: kass_server::Running,
    worker: Worker,
    database: PathBuf,
}

impl Kass {
    async fn start() -> Self {
        let worker = Worker::default();
        let database = database();
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker.clone()).await,
            database: database.clone(),
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
            database,
        }
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

    /// A dictation in Kass's own window, saved now.
    fn dictate(&self, id: &str, raw: &str, refined: &str) {
        let now = chrono::Utc::now().naive_utc();
        let conn = rusqlite::Connection::open(&self.database).unwrap();
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, transcript_refined,
                 app_bundle_id, app_name, audio_deleted, created_at)
             VALUES (?1, 'a.wav', 'dictation', ?2, ?3, 'com.mrgnhnt.kass', 'Kass', 0, ?4)",
            rusqlite::params![
                id,
                raw,
                refined,
                now.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
            ],
        )
        .unwrap();
    }

    fn file(&self, name: &str) -> Value {
        let path = self.database.parent().unwrap().join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        if let Some(dir) = self.database.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn path(session: &Value, rest: &str) -> String {
    format!(
        "/writing-style/teach/{}{rest}",
        session["session_id"].as_str().unwrap()
    )
}

fn conversation(session: &Value, at: usize, rest: &str) -> String {
    let id = session["conversations"][at]["id"].as_str().unwrap();
    path(session, &format!("/conversations/{id}{rest}"))
}

#[tokio::test]
async fn a_session_opens_on_its_styles_apps_and_the_worker_hears_it() {
    let kass = Kass::start().await;
    let (status, session) = kass
        .call("POST", "/writing-style/teach?style=chat", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(session["style_id"], "chat");
    assert_eq!(session["target"], 10);
    assert_eq!(session["replies"], 0);
    // WhatsApp is the style's one app: a conversation like it.
    assert_eq!(session["suggested_kinds"], json!(["text_message"]));
    let conversations = session["conversations"].as_array().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0]["kind"], "text_message");
    assert_eq!(conversations[0]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(conversations[0]["messages"][0]["from_you"], false);

    // A style no app uses: the usual three kinds.
    let (_, other) = kass.call("POST", "/writing-style/teach", None).await;
    assert_eq!(other["conversations"].as_array().unwrap().len(), 3);
    let (status, _) = kass.call("DELETE", &path(&other, ""), None).await;
    assert_eq!(status, 204);

    let (status, missing) = kass
        .call("POST", "/writing-style/teach?style=nope", None)
        .await;
    assert_eq!(
        (status, missing),
        (404, json!({"detail": "Style not found"}))
    );

    let (status, _) = kass.call("DELETE", &path(&session, ""), None).await;
    assert_eq!(status, 204);
    let (status, gone) = kass
        .call(
            "POST",
            &path(&session, "/conversations"),
            Some(json!({"kind": "email"})),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(
        gone["detail"],
        "This teaching session expired. Start it again."
    );
    // Discarding twice is fine.
    let (status, _) = kass.call("DELETE", &path(&session, ""), None).await;
    assert_eq!(status, 204);
}

#[tokio::test]
async fn bodies_are_checked_before_the_session() {
    let kass = Kass::start().await;
    let (status, problem) = kass
        .call(
            "POST",
            "/writing-style/teach/none/conversations",
            Some(json!({"kind": "poem"})),
        )
        .await;
    assert_eq!(status, 422);
    let problem = &problem["detail"][0];
    assert_eq!(problem["type"], "literal_error");
    assert_eq!(problem["loc"], json!(["body", "kind"]));
    assert_eq!(
        problem["ctx"]["expected"],
        "'coding_agent', 'design_feedback', 'notes', 'writeup', 'team_chat', \
         'issue_comment', 'email' or 'text_message'"
    );
    let (status, problem) = kass
        .call(
            "POST",
            "/writing-style/teach/none/conversations/c/replies",
            Some(json!({"written": "x".repeat(4001)})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(problem["detail"][0]["type"], "string_too_long");
    let (status, problem) = kass
        .call(
            "POST",
            "/writing-style/teach/none/conversations/c/replies",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(problem["detail"][0]["type"], "missing");
    let (status, _) = kass
        .call(
            "POST",
            "/writing-style/teach/none/conversations/c/replies",
            Some(json!({"written": "hi"})),
        )
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_reply_gets_the_next_turn_from_the_model() {
    let kass = Kass::start().await;
    let (_, session) = kass.call("POST", "/writing-style/teach", None).await;
    let (status, missing) = kass
        .call(
            "POST",
            &path(&session, "/conversations/nope/replies"),
            Some(json!({"written": "hi"})),
        )
        .await;
    assert_eq!(
        (status, missing),
        (404, json!({"detail": "Conversation not found"}))
    );
    let (status, refused) = kass
        .call(
            "POST",
            &conversation(&session, 0, "/replies"),
            Some(json!({"written": "   "})),
        )
        .await;
    assert_eq!(status, 400);
    assert!(refused["detail"].is_string());

    // The first answer repeats the opening message: the model is asked again.
    let opening = session["conversations"][0]["messages"][0]["text"]
        .as_str()
        .unwrap();
    kass.worker.turns.lock().unwrap().push_back(format!(
        "MESSAGE: {}!\nTELL: Ready? -> Yes",
        opening.to_uppercase()
    ));
    let (status, replied) = kass
        .call(
            "POST",
            &conversation(&session, 0, "/replies"),
            Some(json!({"written": "Sure, on it."})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(replied["replies"], 1);
    let messages = &replied["conversations"][0]["messages"];
    assert_eq!(
        messages[1],
        json!({"from_you": true, "text": "Sure, on it."})
    );
    assert_eq!(
        messages[2],
        json!({"from_you": false, "text": "nice. can you post the release notes too?"})
    );
    assert_eq!(
        replied["conversations"][0]["note"]["answers"],
        json!([
            {"ask": "Post notes?", "answer": "Yes, Thursday"},
            {"ask": "Who else?", "answer": "Tag Jess"},
        ])
    );
    let generated = kass.worker.generated.lock().unwrap().clone();
    assert_eq!(generated.len(), 2);
    let turn = &generated[1];
    assert_eq!(turn["cache_key"], "teach");
    assert_eq!(turn["flags"], Value::Null);
    assert_eq!(turn["examples"], json!([]));
    assert_eq!(turn["max_tokens"], 320);
    assert!(turn["prompt"].as_str().unwrap().contains("Sure, on it."));

    // Output it can't read: a written turn instead.
    kass.worker
        .turns
        .lock()
        .unwrap()
        .push_back("I can't help with that.".into());
    let (status, replied) = kass
        .call(
            "POST",
            &conversation(&replied, 0, "/replies"),
            Some(json!({"written": "Done."})),
        )
        .await;
    assert_eq!(status, 200);
    let messages = replied["conversations"][0]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert!(!messages[4]["text"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn dictation_in_kass_becomes_the_reply_and_finishing_saves_it() {
    let kass = Kass::start().await;
    let (_, session) = kass.call("POST", "/writing-style/teach", None).await;
    let reply = conversation(&session, 0, "/replies");
    let (status, _) = kass
        .call("POST", &conversation(&session, 0, "/restart-turn"), None)
        .await;
    assert_eq!(status, 204);
    let (_, none) = kass
        .call("GET", &conversation(&session, 0, "/dictated"), None)
        .await;
    assert_eq!(none, json!({"text": null}));

    kass.dictate(
        "d1",
        "sounds good see you there",
        "Sounds good, see you there.",
    );
    let (_, shown) = kass
        .call("GET", &conversation(&session, 0, "/dictated"), None)
        .await;
    assert_eq!(shown, json!({"text": "Sounds good, see you there."}));
    let (status, _) = kass
        .call(
            "POST",
            &reply,
            Some(json!({"written": "sounds good, see you there"})),
        )
        .await;
    assert_eq!(status, 200);
    // Used in a reply: not offered again.
    let (_, none) = kass
        .call("GET", &conversation(&session, 0, "/dictated"), None)
        .await;
    assert_eq!(none, json!({"text": null}));
    let (status, _) = kass
        .call("POST", &reply, Some(json!({"written": "Typed this one."})))
        .await;
    assert_eq!(status, 200);

    let (status, finished) = kass.call("POST", &path(&session, "/finish"), None).await;
    assert_eq!(status, 200, "{finished}");
    assert_eq!(finished["replies"], 2);
    assert_eq!(finished["dictated"], 1);
    assert_eq!(finished["status"]["runs"], 1);
    assert_eq!(finished["status"]["example_count"], 2);
    // No longer dictation elsewhere to show: the dictated reply, cleaned up
    // without and with the user's examples. The transcript stands in for a
    // rejected cleanup, started as cleanup starts a text.
    assert!(finished["before"]
        .as_str()
        .unwrap()
        .contains("Sounds good see you there"));
    assert!(finished["after"]
        .as_str()
        .unwrap()
        .contains("Sounds good see you there"));

    let writing = kass.file("writing-style.json");
    let examples = writing["styles"]["personal"]["examples"]
        .as_array()
        .unwrap();
    assert_eq!(examples.len(), 2);
    assert_eq!(examples[0]["said"], "sounds good see you there");
    assert_eq!(examples[0]["shown"], "Sounds good, see you there.");
    assert_eq!(examples[0]["written"], "sounds good, see you there");
    assert_eq!(examples[1]["said"], Value::Null);
    assert!(examples[0]["created_at"]
        .as_str()
        .unwrap()
        .ends_with("+00:00"));

    let (status, _) = kass.call("POST", &path(&session, "/finish"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn finishing_without_a_reply_is_refused() {
    let kass = Kass::start().await;
    let (_, session) = kass.call("POST", "/writing-style/teach", None).await;
    let (status, refused) = kass.call("POST", &path(&session, "/finish"), None).await;
    assert_eq!(
        (status, refused),
        (
            400,
            json!({"detail": "Reply at least once before finishing"})
        )
    );
}
