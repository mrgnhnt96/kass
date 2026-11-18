//! The writing styles API against a real kass.db, in front of a stand-in
//! worker that records what it is told changed and answers the styles state.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WHATSAPP: &str = "net.whatsapp.WhatsApp";

#[derive(Clone, Default)]
struct Worker {
    /// Each `/worker/styles/changed` body, then a "dictionary" for each
    /// dictionary reload, in order.
    told: Arc<Mutex<Vec<Value>>>,
}

async fn styles_changed(State(worker): State<Worker>, Json(body): Json<Value>) -> Json<Value> {
    worker.told.lock().unwrap().push(body);
    Json(json!({"ok": true}))
}

async fn dictionary_changed(State(worker): State<Worker>) -> Json<Value> {
    worker.told.lock().unwrap().push(json!("dictionary"));
    Json(json!({"ok": true}))
}

async fn styles_state() -> Json<Value> {
    Json(json!({"cache_mb_per_style": 189}))
}

async fn start_worker(worker: Worker) -> SocketAddr {
    let app = Router::new()
        .route("/worker/styles/changed", post(styles_changed))
        .route("/worker/styles/state", post(styles_state))
        .route("/worker/dictionary/changed", post(dictionary_changed))
        // What each style learned is regrouped in kass (tests/learned.rs).
        .route(
            "/worker/learned/changed",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .with_state(worker);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A kass.db with the default style and one WhatsApp capture, corrected.
fn database() -> PathBuf {
    // Its own folder: the learned files are written next to it.
    let dir = std::env::temp_dir().join(format!("kass-styles-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kass.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
    conn.execute_batch(
        "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
             capitalize_first, smart_cleanup, preserve_technical)
         VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1);
         INSERT INTO captures (id, audio_path, source, transcript_raw, app_bundle_id, app_name,
             app_category, audio_deleted, created_at)
         VALUES ('w', 'a.wav', 'dictation', 'hi', 'net.whatsapp.WhatsApp', 'WhatsApp',
             'public.app-category.social-networking', 0, '2026-11-01 10:00:00.000000');
         INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot)
         VALUES ('f', 'w', 'refined', 'Hi.', '', '{}');",
    )
    .unwrap();
    path
}

struct Kass {
    server: kass_server::Running,
    told: Arc<Mutex<Vec<Value>>>,
    database: PathBuf,
}

impl Kass {
    async fn start(shared_server: Option<&str>) -> Self {
        let worker = Worker::default();
        let told = worker.told.clone();
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
            told,
            database,
        }
    }

    /// What the worker was told since the last call.
    fn told(&self) -> Vec<Value> {
        std::mem::take(&mut *self.told.lock().unwrap())
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

    fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        let conn = rusqlite::Connection::open(&self.database).unwrap();
        let mut statement = conn.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|at| match row.get_ref(at).unwrap() {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        rusqlite::types::ValueRef::Text(text) => {
                            json!(String::from_utf8_lossy(text))
                        }
                        other => panic!("unexpected {other:?}"),
                    })
                    .collect())
            })
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

fn changed(examples: bool) -> Value {
    json!({"examples": examples})
}

#[tokio::test]
async fn styles_and_apps_round_trip() {
    let kass = Kass::start(None).await;

    let (status, listing) = kass.call("GET", "/writing-styles", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        listing,
        json!({
            "styles": [{
                "id": "personal", "name": "Personal", "position": 0, "is_default": true,
                "punctuation_style": "standard", "preserve_technical": true, "description": "",
            }],
            "apps": [{
                "bundle_id": WHATSAPP, "name": "WhatsApp", "style_id": "personal",
                "confirmed": false, "count": 1, "corrections": 1, "suggested_style_id": "personal",
            }],
            "max_styles": 6,
            "cache_mb_per_style": 189,
        })
    );

    let apps = json!({"apps": [{"bundle_id": WHATSAPP, "app_name": "WhatsApp"}]});
    let (status, confirmed) = kass
        .call("POST", "/writing-styles/apps/confirm", Some(apps.clone()))
        .await;
    assert_eq!(status, 200);
    assert_eq!(confirmed["apps"][0]["confirmed"], true);
    assert_eq!(confirmed["apps"][0]["style_id"], "personal");
    assert_eq!(confirmed["apps"][0]["suggested_style_id"], Value::Null);
    assert_eq!(kass.told(), vec![changed(false)]);
    // Already confirmed: nothing new to tell.
    kass.call("POST", "/writing-styles/apps/confirm", Some(apps))
        .await;
    assert!(kass.told().is_empty());

    let (status, chat) = kass
        .call("POST", "/writing-styles", Some(json!({"name": "Chat"})))
        .await;
    assert_eq!(status, 200);
    assert_eq!(chat["position"], 1);
    let chat = chat["id"].as_str().unwrap().to_string();
    assert_eq!(kass.told(), vec![changed(false)]);

    let path = format!("/writing-styles/apps/{WHATSAPP}");
    let (status, listing) = kass
        .call(
            "PUT",
            &path,
            Some(json!({"style_id": chat, "corrections": "leave"})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(listing["apps"][0]["style_id"], json!(chat));
    assert_eq!(listing["apps"][0]["confirmed"], true);
    assert_eq!(listing["apps"][0]["corrections"], 0);
    assert_eq!(kass.told(), vec![changed(true)]);
    assert_eq!(
        kass.query("SELECT teaches_style_id FROM captures"),
        vec![vec![json!("personal")]]
    );

    let (status, missing) = kass
        .call(
            "PUT",
            "/writing-styles/apps/x",
            Some(json!({"style_id": "missing"})),
        )
        .await;
    assert_eq!(
        (status, missing),
        (404, json!({"detail": "Style not found"}))
    );
    let (status, _) = kass
        .call(
            "PUT",
            "/writing-styles/apps/x",
            Some(json!({"style_id": chat, "corrections": "drop"})),
        )
        .await;
    assert_eq!(status, 422);
    let (status, blank) = kass
        .call(
            "PUT",
            "/writing-styles/apps/%20",
            Some(json!({"style_id": chat})),
        )
        .await;
    assert_eq!((status, blank), (400, json!({"detail": "Name an app"})));

    let (status, described) = kass
        .call(
            "PATCH",
            &format!("/writing-styles/{chat}"),
            Some(json!({"description": "  Informal.\nNo greetings.  ", "name": null})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(described["description"], "Informal.\nNo greetings.");
    assert_eq!(described["name"], "Chat");
    assert_eq!(kass.told(), vec![changed(false)]);
    let (status, long) = kass
        .call(
            "PATCH",
            &format!("/writing-styles/{chat}"),
            Some(json!({"description": "x".repeat(601)})),
        )
        .await;
    assert_eq!(
        (status, long),
        (
            400,
            json!({"detail": "Keep the description under 600 characters"})
        )
    );
    let (status, _) = kass
        .call(
            "PATCH",
            "/writing-styles/missing",
            Some(json!({"name": "Gone"})),
        )
        .await;
    assert_eq!(status, 404);

    let (status, refused) = kass.call("DELETE", "/writing-styles/personal", None).await;
    assert_eq!(
        (status, refused),
        (
            400,
            json!({"detail": "Make another style the default before deleting this one"})
        )
    );
    kass.told();
    let (status, _) = kass
        .call("DELETE", &format!("/writing-styles/{chat}"), None)
        .await;
    assert_eq!(status, 204);
    assert_eq!(kass.told(), vec![json!("dictionary"), changed(true)]);
    let (_, listing) = kass.call("GET", "/writing-styles", None).await;
    assert_eq!(listing["styles"].as_array().unwrap().len(), 1);
    // Its app is unassigned again, so new.
    assert_eq!(listing["apps"][0]["confirmed"], false);
    assert_eq!(listing["apps"][0]["style_id"], "personal");
    let (status, _) = kass
        .call("DELETE", &format!("/writing-styles/{chat}"), None)
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn making_a_style_the_default_regroups_examples() {
    let kass = Kass::start(None).await;
    let (_, chat) = kass
        .call("POST", "/writing-styles", Some(json!({"name": "Chat"})))
        .await;
    let chat = chat["id"].as_str().unwrap().to_string();
    kass.told();
    let (status, made) = kass
        .call(
            "PATCH",
            &format!("/writing-styles/{chat}"),
            Some(json!({"is_default": true})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(made["is_default"], true);
    assert_eq!(kass.told(), vec![changed(true)]);
    let (_, listing) = kass.call("GET", "/writing-styles", None).await;
    let defaults: Vec<&Value> = listing["styles"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|style| style["is_default"] == true)
        .map(|style| &style["id"])
        .collect();
    assert_eq!(defaults, vec![&json!(chat)]);
}

#[tokio::test]
async fn requests_are_checked_as_pydantic_checks_them() {
    let kass = Kass::start(None).await;

    let (status, missing) = kass.call("POST", "/writing-styles", Some(json!({}))).await;
    assert_eq!(status, 422);
    assert_eq!(missing["detail"][0]["type"], "missing");
    assert_eq!(missing["detail"][0]["loc"], json!(["body", "name"]));

    let (status, long) = kass
        .call(
            "POST",
            "/writing-styles",
            Some(json!({"name": "x".repeat(81)})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(long["detail"][0]["type"], "string_too_long");

    let (status, taken) = kass
        .call(
            "POST",
            "/writing-styles",
            Some(json!({"name": " personal "})),
        )
        .await;
    assert_eq!(
        (status, taken),
        (
            400,
            json!({"detail": "There's already a style called personal"})
        )
    );

    let (status, pattern) = kass
        .call(
            "PATCH",
            "/writing-styles/personal",
            Some(json!({"punctuation_style": "loud"})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(pattern["detail"][0]["type"], "string_pattern_mismatch");
    assert_eq!(
        pattern["detail"][0]["msg"],
        "String should match pattern '^(standard|casual|learned)$'"
    );

    let (status, literal) = kass
        .call(
            "PUT",
            "/writing-styles/apps/x",
            Some(json!({"style_id": "personal", "corrections": "drop"})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(
        literal["detail"][0]["msg"],
        "Input should be 'bring' or 'leave'"
    );
    assert_eq!(literal["detail"][0]["loc"], json!(["body", "corrections"]));

    let (status, empty) = kass
        .call(
            "POST",
            "/writing-styles/apps/confirm",
            Some(json!({"apps": [{"bundle_id": ""}, 3]})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(empty["detail"][0]["type"], "string_too_short");
    assert_eq!(
        empty["detail"][0]["loc"],
        json!(["body", "apps", 0, "bundle_id"])
    );
    assert_eq!(empty["detail"][1]["type"], "model_type");
    assert_eq!(empty["detail"][1]["loc"], json!(["body", "apps", 1]));

    let many: Vec<Value> = (0..501)
        .map(|n| json!({"bundle_id": format!("app.{n}")}))
        .collect();
    let (status, too_many) = kass
        .call(
            "POST",
            "/writing-styles/apps/confirm",
            Some(json!({"apps": many})),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(
        too_many["detail"][0]["msg"],
        "List should have at most 500 items after validation, not 501"
    );

    // Blank bundle ids are dropped, as the worker dropped them.
    let (status, _) = kass
        .call(
            "POST",
            "/writing-styles/apps/confirm",
            Some(json!({"apps": [{"bundle_id": "  "}]})),
        )
        .await;
    assert_eq!(status, 200);
    assert!(kass.query("SELECT * FROM app_styles").is_empty());
}

#[tokio::test]
async fn a_stand_in_records_every_row_it_changes() {
    let kass = Kass::start(Some("http://127.0.0.1:1")).await;
    let (_, chat) = kass
        .call("POST", "/writing-styles", Some(json!({"name": "Chat"})))
        .await;
    let chat = chat["id"].as_str().unwrap().to_string();
    kass.call(
        "PUT",
        &format!("/writing-styles/apps/{WHATSAPP}"),
        Some(json!({"style_id": chat, "corrections": "leave"})),
    )
    .await;
    let mut recorded =
        kass.query("SELECT kind, ref_id, deleted FROM sync_changes ORDER BY kind, ref_id");
    recorded.sort_by_key(|row| row[0].as_str().unwrap().to_string());
    assert_eq!(
        recorded,
        vec![
            vec![json!("app_style"), json!(WHATSAPP), json!(0)],
            vec![json!("capture"), json!("w"), json!(0)],
            vec![json!("style"), json!(chat), json!(0)],
        ]
    );
}
