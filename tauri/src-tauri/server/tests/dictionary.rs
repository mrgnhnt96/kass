//! The dictionary API against a real kass.db, in front of a stand-in worker
//! that counts the reloads it is told to do and counts prompt terms.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const ZED: &str = "dev.zed.Zed";
const SLACK: &str = "com.tinyspeck.slackmacgap";

#[derive(Clone, Default)]
struct Worker {
    reloads: Arc<AtomicUsize>,
    /// Whisper loaded: each term counts as one token per word.
    whisper: bool,
}

async fn changed(State(worker): State<Worker>) -> Json<Value> {
    worker.reloads.fetch_add(1, Ordering::SeqCst);
    Json(json!({"ok": true}))
}

async fn token_counts(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    if !worker.whisper {
        return Json(json!({"counts": null}));
    }
    let counts: Vec<usize> = body["terms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|term| term.as_str().unwrap().split_whitespace().count())
        .collect();
    Json(json!({"counts": counts}))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/dictionary/changed", post(changed))
        .route("/worker/dictionary/token-counts", post(token_counts))
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A kass.db with the default style, a "Code" style, and Zed in it.
fn database() -> PathBuf {
    // Its own folder: the learned files are written next to it.
    let dir = std::env::temp_dir().join(format!("kass-dictionary-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kass.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
    conn.execute_batch(
        "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
             capitalize_first, smart_cleanup, preserve_technical)
         VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1),
                ('code', 'Code', 1, 0, 'standard', 1, 1, 1);
         INSERT INTO app_styles (bundle_id, app_name, style_id) VALUES ('dev.zed.Zed', 'Zed', 'code');",
    )
    .unwrap();
    path
}

struct Kass {
    server: kass_server::Running,
    reloads: Arc<AtomicUsize>,
    database: PathBuf,
}

impl Kass {
    async fn start(whisper: bool, shared_server: Option<&str>) -> Self {
        let worker = Worker {
            whisper,
            ..Worker::default()
        };
        let reloads = worker.reloads.clone();
        let database = database();
        let server = kass_server::start(kass_server::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            worker: start_worker(worker).await,
            database: database.clone(),
            shared_server: shared_server.map(str::to_string),
            voice_model: None,
            models_dir: None,
            app_version: "0.0.0-test".into(),
        })
        .await
        .unwrap();
        Kass {
            server,
            reloads,
            database,
        }
    }

    fn reloads(&self) -> usize {
        self.reloads.load(Ordering::SeqCst)
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

    async fn add(&self, written: &str, spoken: Option<&str>, places: Value) -> (u16, Value) {
        self.call(
            "POST",
            "/dictionary",
            Some(json!({"written": written, "spoken": spoken, "places": places})),
        )
        .await
    }

    async fn entries(&self) -> Vec<Value> {
        let (status, body) = self.call("GET", "/dictionary", None).await;
        assert_eq!(status, 200);
        body["entries"].as_array().unwrap().clone()
    }

    fn sync_changes(&self) -> Vec<(String, bool)> {
        let conn = rusqlite::Connection::open(&self.database).unwrap();
        let mut statement = conn
            .prepare("SELECT ref_id, deleted FROM sync_changes WHERE kind = 'dictionary' ORDER BY ref_id")
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
        if let Some(dir) = self.database.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn global() -> Value {
    json!([{"scope": "global"}])
}

fn zed() -> Value {
    json!({"scope": "app", "scope_id": ZED, "app_name": "Zed"})
}

fn slack() -> Value {
    json!({"scope": "app", "scope_id": SLACK, "app_name": "Slack"})
}

fn written(entries: &[Value]) -> Vec<&str> {
    entries
        .iter()
        .map(|e| e["written"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn adds_lists_and_rejects_the_same_word_twice() {
    let kass = Kass::start(false, None).await;
    let (status, mut added) = kass.add("  Voicebox ", Some("voice  box"), global()).await;
    assert_eq!(status, 200);
    assert!(added["created_at"].as_str().unwrap().starts_with("20"));
    added["id"] = json!("x");
    added["created_at"] = Value::Null;
    assert_eq!(
        added,
        json!({
            "id": "x",
            "written": "Voicebox",
            "spoken": "voice box",
            "places": [{"scope": "global", "scope_id": null, "app_name": null}],
            "created_at": null,
            "match_sound": true,
            "source": "user",
            "phrase": false,
        })
    );
    assert_eq!(kass.reloads(), 1);

    let (status, duplicate) = kass.add("VoiceBox", Some("Voice Box"), global()).await;
    assert_eq!(status, 409);
    assert_eq!(
        duplicate["detail"],
        "That word is already in the dictionary for everywhere"
    );
    assert_eq!(kass.reloads(), 1);

    // The same word said is fine in another place.
    let (status, _) = kass
        .add("voicebox", Some("voice box"), json!([zed()]))
        .await;
    assert_eq!(status, 200);
    assert_eq!(written(&kass.entries().await), ["voicebox", "Voicebox"]);
    // Nothing to send a shared server: this Mac isn't standing in for one.
    assert_eq!(kass.sync_changes(), []);
}

#[tokio::test]
async fn adds_a_phrase_that_keeps_its_lines() {
    let kass = Kass::start(false, None).await;
    let phrase = |written: &str, spoken: Option<&str>| json!({"written": written, "spoken": spoken, "places": global(), "phrase": true});
    let (status, added) = kass
        .call(
            "POST",
            "/dictionary",
            Some(phrase("\n Thanks,  \n\n  Morgan \n", Some(" sign  off "))),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (&added["written"], &added["spoken"], &added["phrase"]),
        (
            &json!("Thanks,\n\nMorgan"),
            &json!("sign off"),
            &json!(true)
        )
    );
    // Longer than a word may be, not than a phrase may.
    let long = "x".repeat(900);
    assert_eq!(
        kass.call("POST", "/dictionary", Some(phrase(&long, Some("long one"))))
            .await
            .0,
        200
    );
    let too_long = "x".repeat(1001);
    assert_eq!(
        kass.call(
            "POST",
            "/dictionary",
            Some(phrase(&too_long, Some("too long")))
        )
        .await
        .0,
        400
    );

    // A phrase is said: it needs what you say, and keeps it.
    assert_eq!(
        kass.call("POST", "/dictionary", Some(phrase("hi", None)))
            .await
            .0,
        400
    );
    let path = format!("/dictionary/{}", added["id"].as_str().unwrap());
    let (status, _) = kass
        .call("PATCH", &path, Some(json!({"spoken": null})))
        .await;
    assert_eq!(status, 400);
    let (status, edited) = kass
        .call("PATCH", &path, Some(json!({"written": "Cheers,\nMorgan"})))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (&edited["written"], &edited["phrase"]),
        (&json!("Cheers,\nMorgan"), &json!(true))
    );
}

#[tokio::test]
async fn rejects_empty_words_unknown_places_and_bad_bodies() {
    let kass = Kass::start(false, None).await;
    assert_eq!(kass.add("   ", None, global()).await.0, 400);
    assert_eq!(
        kass.add("Zed", None, json!([{"scope": "style", "scope_id": "nope"}]))
            .await
            .0,
        400
    );
    assert_eq!(
        kass.add("Zed", None, json!([{"scope": "app"}])).await.0,
        400
    );
    assert_eq!(kass.add(&"x".repeat(201), None, global()).await.0, 400);

    let (status, empty) = kass.add("Zed", None, json!([])).await;
    assert_eq!(status, 422);
    assert_eq!(empty["detail"][0]["loc"], json!(["body", "places"]));
    let (status, scope) = kass.add("Zed", None, json!([{"scope": "planet"}])).await;
    assert_eq!(status, 422);
    assert_eq!(
        scope["detail"][0]["loc"],
        json!(["body", "places", 0, "scope"])
    );
    let (status, missing) = kass
        .call("POST", "/dictionary", Some(json!({"written": "Zed"})))
        .await;
    assert_eq!(status, 422);
    assert_eq!(missing["detail"][0]["type"], "missing");
    assert_eq!(
        kass.call(
            "PATCH",
            "/dictionary/x",
            Some(json!({"match_sound": "maybe"}))
        )
        .await
        .0,
        422
    );

    assert_eq!(kass.reloads(), 0);
    assert_eq!(kass.entries().await, Vec::<Value>::new());
}

#[tokio::test]
async fn one_entry_applies_in_several_places_and_edits_them_together() {
    let kass = Kass::start(false, None).await;
    let (_, added) = kass
        .add(
            "VoiceBox",
            Some("voice box"),
            json!([{"scope": "style", "scope_id": "code"}, slack()]),
        )
        .await;
    let scopes: Vec<_> = added["places"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["scope"].clone())
        .collect();
    assert_eq!(scopes, [json!("style"), json!("app")]);
    assert_eq!(kass.entries().await.len(), 1);

    // Moving it: Slack stays, the style goes, Zed joins; the words change everywhere.
    let path = format!("/dictionary/{}", added["id"].as_str().unwrap());
    let (status, edited) = kass
        .call(
            "PATCH",
            &path,
            Some(json!({"written": "Voicebox", "places": [slack(), zed()]})),
        )
        .await;
    assert_eq!(status, 200);
    let places: Vec<_> = edited["places"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["scope"].clone(), p["scope_id"].clone()))
        .collect();
    assert_eq!(
        places,
        [(json!("app"), json!(SLACK)), (json!("app"), json!(ZED))]
    );
    assert_eq!(edited["written"], "Voicebox");
    assert_eq!(edited["created_at"], added["created_at"]);

    // Everywhere replaces the rest.
    let (_, everywhere) = kass
        .call(
            "PATCH",
            &path,
            Some(json!({"places": [zed(), {"scope": "global"}]})),
        )
        .await;
    assert_eq!(
        everywhere["places"],
        json!([{"scope": "global", "scope_id": null, "app_name": null}])
    );

    assert_eq!(
        kass.call("DELETE", &path, None).await,
        (200, json!({"deleted": true}))
    );
    assert_eq!(kass.entries().await, Vec::<Value>::new());
    assert_eq!(kass.reloads(), 4);
}

#[tokio::test]
async fn moving_an_entry_onto_the_same_word_is_refused() {
    let kass = Kass::start(false, None).await;
    kass.add("VoiceBox", Some("voice box"), json!([zed()]))
        .await;
    let (_, other) = kass.add("Voicebox", Some("voice box"), global()).await;

    let path = format!("/dictionary/{}", other["id"].as_str().unwrap());
    let (status, moved) = kass
        .call("PATCH", &path, Some(json!({"places": [zed()]})))
        .await;

    assert_eq!(status, 409);
    assert_eq!(
        moved["detail"],
        "That word is already in the dictionary for Zed"
    );
    // Refused, nothing changed.
    let entries = kass.entries().await;
    let other = entries.iter().find(|e| e["id"] == other["id"]).unwrap();
    assert_eq!(other["places"][0]["scope"], "global");
}

#[tokio::test]
async fn editing_and_deleting_tell_the_worker_and_a_missing_entry_is_404() {
    let kass = Kass::start(false, None).await;
    let (_, added) = kass.add("Kubernetes", None, global()).await;
    let path = format!("/dictionary/{}", added["id"].as_str().unwrap());

    let (_, edited) = kass
        .call(
            "PATCH",
            &path,
            Some(json!({"written": "k8s", "spoken": "kates"})),
        )
        .await;
    assert_eq!(
        (&edited["written"], &edited["spoken"]),
        (&json!("k8s"), &json!("kates"))
    );
    assert_eq!(
        kass.call("DELETE", &path, None).await,
        (200, json!({"deleted": true}))
    );
    assert_eq!(kass.reloads(), 3);

    let missing = json!({"detail": "Dictionary entry not found"});
    assert_eq!(
        kass.call("DELETE", &path, None).await,
        (404, missing.clone())
    );
    assert_eq!(
        kass.call("PATCH", &path, Some(json!({"match_sound": false})))
            .await,
        (404, missing)
    );
    assert_eq!(kass.reloads(), 3);
}

#[tokio::test]
async fn an_app_gets_its_styles_entries_and_resolved_shows_overrides() {
    let kass = Kass::start(true, None).await;
    kass.add("VoiceBox", Some("voice box"), global()).await;
    kass.add(
        "mrgnhnt96",
        None,
        json!([{"scope": "style", "scope_id": "code"}]),
    )
    .await;
    let (_, mine) = kass
        .add(
            "Voicebox",
            Some("voice box"),
            json!([{"scope": "app", "scope_id": ZED}]),
        )
        .await;

    let (status, resolved) = kass
        .call(
            "GET",
            &format!("/dictionary/resolved?bundle_id={ZED}"),
            None,
        )
        .await;

    assert_eq!(status, 200);
    let entries: Vec<_> = resolved["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["scope"].clone(),
                e["written"].clone(),
                e["overridden"].clone(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            (json!("app"), json!("Voicebox"), json!(false)),
            (json!("style"), json!("mrgnhnt96"), json!(false)),
            (json!("global"), json!("VoiceBox"), json!(true)),
        ]
    );
    assert_eq!(resolved["entries"][0]["id"], mine["id"]);
    assert_eq!(resolved["prompt_terms"], json!(["mrgnhnt96", "Voicebox"]));
    assert_eq!(resolved["dropped_terms"], json!([]));

    // Slack is in the default style: only the global entry.
    let (_, slack) = kass
        .call(
            "GET",
            &format!("/dictionary/resolved?bundle_id={SLACK}"),
            None,
        )
        .await;
    assert_eq!(slack["prompt_terms"], json!(["VoiceBox"]));
}

#[tokio::test]
async fn resolved_estimates_term_sizes_without_whisper_and_drops_what_does_not_fit() {
    let kass = Kass::start(false, None).await;
    // About 200 characters each: 67 tokens estimated, more than the prompt holds.
    let long = format!("Z{}", "z".repeat(199));
    kass.add(&long, Some("long one"), global()).await;
    kass.add("Kubernetes", None, global()).await;

    let (status, resolved) = kass.call("GET", "/dictionary/resolved", None).await;

    assert_eq!(status, 200);
    assert_eq!(resolved["prompt_terms"], json!(["Kubernetes"]));
    assert_eq!(resolved["dropped_terms"], json!([long]));
}

#[tokio::test]
async fn turns_sound_matching_off_and_on() {
    let kass = Kass::start(false, None).await;
    let (_, added) = kass.add("Meghan", None, global()).await;
    assert_eq!(
        (&added["match_sound"], &added["source"]),
        (&json!(true), &json!("user"))
    );

    let path = format!("/dictionary/{}", added["id"].as_str().unwrap());
    let (_, edited) = kass
        .call("PATCH", &path, Some(json!({"match_sound": false})))
        .await;
    assert_eq!(edited["match_sound"], false);
    let (_, resolved) = kass
        .call(
            "GET",
            &format!("/dictionary/resolved?bundle_id={ZED}"),
            None,
        )
        .await;
    let entries: Vec<_> = resolved["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["written"].clone(), e["match_sound"].clone()))
        .collect();
    assert_eq!(entries, [(json!("Meghan"), json!(false))]);
    assert_eq!(resolved["prompt_terms"], json!(["Meghan"]));

    let (_, exact) = kass
        .call(
            "POST",
            "/dictionary",
            Some(json!({"written": "Saggar", "places": global(), "match_sound": false})),
        )
        .await;
    assert_eq!(exact["match_sound"], false);
}

#[tokio::test]
async fn standing_in_for_a_shared_server_records_what_changed() {
    let kass = Kass::start(false, Some("http://studio.local:17493/")).await;
    let (_, added) = kass
        .add("VoiceBox", Some("voice box"), json!([zed(), slack()]))
        .await;
    let id = added["id"].as_str().unwrap();
    let rows = kass.sync_changes();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, deleted)| !deleted));

    // Slack leaves: its row is recorded deleted, Zed's is unchanged.
    let path = format!("/dictionary/{id}");
    kass.call("PATCH", &path, Some(json!({"places": [zed()]})))
        .await;
    let rows = kass.sync_changes();
    assert_eq!(rows.iter().filter(|(_, deleted)| *deleted).count(), 1);

    kass.call("DELETE", &path, None).await;
    assert!(kass.sync_changes().iter().all(|(_, deleted)| *deleted));
}

#[tokio::test]
async fn answers_a_cors_preflight_from_the_app() {
    let kass = Kass::start(false, None).await;
    let response = kass
        .raw(
            "OPTIONS /dictionary HTTP/1.1\r\nHost: x\r\nOrigin: tauri://localhost\r\n\
             Access-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\n\
             Connection: close\r\n\r\n"
                .to_string(),
        )
        .await
        .to_lowercase();
    assert!(response.starts_with("http/1.1 200"), "{response}");
    assert!(
        response.contains("access-control-allow-origin: tauri://localhost"),
        "{response}"
    );
    assert!(
        response.contains("access-control-allow-headers: content-type"),
        "{response}"
    );

    let refused = kass
        .raw(
            "OPTIONS /dictionary HTTP/1.1\r\nHost: x\r\nOrigin: https://evil.example\r\n\
             Access-Control-Request-Method: POST\r\nConnection: close\r\n\r\n"
                .to_string(),
        )
        .await;
    assert!(refused.starts_with("HTTP/1.1 400"), "{refused}");
}

#[tokio::test]
async fn the_worker_s_own_routes_are_not_reachable_from_outside() {
    let kass = Kass::start(false, None).await;
    let not_found = json!({"detail": "Not Found"});
    assert_eq!(
        kass.call("POST", "/worker/dictionary/changed", None).await,
        (404, not_found.clone())
    );
    assert_eq!(kass.call("GET", "/worker", None).await, (404, not_found));
    assert_eq!(kass.reloads(), 0);
}
