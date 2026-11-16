mod common;

use common::{CAPTURE, Reply, Server};
use kass_api::types::{
    AppToConfirm, CaptureAppFilter, CaptureCreateOptions, CaptureSettingsUpdate, CaptureSource,
    CaptureUpload, DictionaryEntryUpdate, HistoryRetentionDays, MovedCorrections, TeachKind,
    UsagePeriod, WhisperModelSize,
};
use kass_api::{ApiClient, Error, Mac};
use serde_json::json;

const STYLES: &str = r#"{"styles":[],"apps":[],"max_styles":5}"#;
const STATUS: &str = r#"{"ready":true,"runs":2,"last_run_at":null,"example_count":14,"habits":["drop_final_period"]}"#;
const SESSION: &str = r#"{"session_id":"t-1","style_id":"s-1","target":6,"replies":0,"suggested_kinds":["email"],"conversations":[]}"#;

fn client(server: &Server) -> ApiClient {
    ApiClient::new(
        &server.url,
        Some(Mac::new("mac-1", Some("Morgan’s MacBook Pro".into()))),
    )
}

#[test]
fn sends_the_mac_headers_percent_encoded() {
    let server = Server::start(vec![Reply::ok(r#"{"internal":true}"#)]);
    assert!(client(&server).internal().unwrap());
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("GET", "/internal")
    );
    assert_eq!(request.header("X-Kass-Mac"), Some("mac-1"));
    assert_eq!(
        request.header("X-Kass-Mac-Name"),
        Some("Morgan%E2%80%99s%20MacBook%20Pro")
    );
    assert_eq!(request.header("Content-Type"), Some("application/json"));
}

#[test]
fn sends_no_mac_headers_without_a_mac() {
    let server = Server::start(vec![
        Reply::ok(r#"{"path":"/Volumes/Models"}"#),
        Reply::ok(r#"{"free_mb":51234.5}"#),
    ]);
    let client = ApiClient::new(&server.url, Some(Mac::new("mac-1", None)));
    client.set_mac(None);
    assert_eq!(client.models_cache_dir().unwrap(), "/Volumes/Models");
    let request = server.request();
    assert_eq!(request.header("X-Kass-Mac"), None);
    assert_eq!(request.header("X-Kass-Mac-Name"), None);

    client.set_mac(Some(Mac::new("mac-2", None)));
    assert_eq!(client.models_disk_space_mb().unwrap(), 51234.5);
    let request = server.request();
    assert_eq!(request.header("X-Kass-Mac"), Some("mac-2"));
    assert_eq!(request.header("X-Kass-Mac-Name"), None);
}

#[test]
fn switching_the_server_switches_every_clone() {
    let first = Server::start(vec![Reply::ok(r#"{"internal":false}"#)]);
    let second = Server::start(vec![Reply::ok(r#"{"internal":true}"#)]);
    let client = ApiClient::new(format!("{}/", first.url), None);
    let clone = client.clone();
    assert_eq!(client.base_url(), first.url);
    assert!(!clone.internal().unwrap());
    first.request();

    client.set_base_url(&second.url);
    assert!(clone.internal().unwrap());
    second.request();
    assert_eq!(
        clone.capture_audio_url("c 1"),
        format!("{}/captures/c%201/audio", second.url)
    );
    assert_eq!(
        clone.migration_progress_url(),
        format!("{}/models/migrate/progress", second.url)
    );
}

#[test]
fn local_points_at_this_macs_server() {
    assert_eq!(ApiClient::local(None).base_url(), "http://127.0.0.1:17493");
}

#[test]
fn lists_captures_with_paging_and_an_app_filter() {
    let page = format!(r#"{{"items":[{CAPTURE}],"total":1}}"#);
    let server = Server::start(vec![Reply::ok(&page), Reply::ok(&page), Reply::ok(&page)]);
    let client = client(&server);

    let list = client.list_captures(50, 0, &CaptureAppFilter::All).unwrap();
    assert_eq!(list.total, 1);
    assert_eq!(list.items[0].app_name.as_deref(), Some("Slack"));
    assert_eq!(server.request().target, "/captures?limit=50&offset=0");

    client
        .list_captures(25, 50, &CaptureAppFilter::App("com.example app".into()))
        .unwrap();
    assert_eq!(
        server.request().target,
        "/captures?limit=25&offset=50&app_bundle_id=com.example+app"
    );

    client
        .list_captures(50, 0, &CaptureAppFilter::Unknown)
        .unwrap();
    assert_eq!(
        server.request().target,
        "/captures?limit=50&offset=0&unknown_app=true"
    );
}

#[test]
fn asks_for_stats_by_period() {
    let server = Server::start(vec![Reply::json(
        503,
        r#"{"detail":"Stats are still loading"}"#,
    )]);
    let error = client(&server)
        .capture_stats(
            UsagePeriod::Days7,
            &CaptureAppFilter::App("com.apple.mail".into()),
        )
        .unwrap_err();
    assert_eq!(error.status(), Some(503));
    assert_eq!(
        server.request().target,
        "/captures/stats?period=7d&app_bundle_id=com.apple.mail"
    );
}

#[test]
fn uploads_a_capture_as_multipart_form_data() {
    let created = format!(
        r#"{{{}, "auto_refine": true, "allow_auto_paste": false}}"#,
        &CAPTURE.trim()[1..CAPTURE.trim().len() - 1]
    );
    let server = Server::start(vec![Reply::ok(&created), Reply::ok(&created)]);
    let client = client(&server);

    let upload = CaptureUpload::wav("take.wav", b"RIFF0000WAVE".to_vec());
    let options = CaptureCreateOptions {
        source: Some(CaptureSource::Dictation),
        language: Some("en".into()),
        stt_model: Some(WhisperModelSize::Turbo),
    };
    let response = client.create_capture(&upload, &options).unwrap();
    assert!(response.auto_refine);
    assert_eq!(response.capture.id, "c-1");

    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("POST", "/captures")
    );
    assert_eq!(request.header("X-Kass-Mac"), Some("mac-1"));
    let content_type = request.header("Content-Type").unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    assert_eq!(
        request.text(),
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"take.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\nRIFF0000WAVE\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"source\"\r\n\r\ndictation\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\nen\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"stt_model\"\r\n\r\nturbo\r\n\
             --{b}--\r\n",
            b = boundary
        )
    );

    client
        .create_capture(&upload, &CaptureCreateOptions::default())
        .unwrap();
    let text = server.request().text();
    assert!(text.contains("name=\"source\"\r\n\r\nfile\r\n"));
    assert!(!text.contains("name=\"language\""));
    assert!(!text.contains("name=\"stt_model\""));
}

#[test]
fn fetches_a_captures_audio() {
    let server = Server::start(vec![Reply::bytes("audio/wav", b"RIFF\x00\x01".to_vec())]);
    assert_eq!(
        client(&server).capture_audio("c-1").unwrap(),
        b"RIFF\x00\x01"
    );
    assert_eq!(server.request().target, "/captures/c-1/audio");
}

#[test]
fn writing_style_calls_carry_the_style() {
    let server = Server::start(vec![
        Reply::ok(STATUS),
        Reply::ok(STATUS),
        Reply::ok("[]"),
        Reply::no_content(),
    ]);
    let client = client(&server);

    assert_eq!(
        client
            .writing_style(Some("work notes"))
            .unwrap()
            .example_count,
        14
    );
    assert_eq!(server.request().target, "/writing-style?style=work%20notes");

    client.reset_writing_style(None).unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("DELETE", "/writing-style")
    );

    assert!(client.list_personal_examples(Some("")).unwrap().is_empty());
    assert_eq!(server.request().target, "/writing-style/examples");

    client.remove_correction_note("n/1", Some("s-2")).unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("DELETE", "/writing-style/notes/n%2F1?style=s-2")
    );
}

#[test]
fn moves_and_confirms_apps() {
    let server = Server::start(vec![
        Reply::ok(STYLES),
        Reply::ok(STYLES),
        Reply::no_content(),
    ]);
    let client = client(&server);

    client
        .assign_app_style("com.example app", "s-2", None, MovedCorrections::Bring)
        .unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("PUT", "/writing-styles/apps/com.example%20app")
    );
    assert_eq!(
        request.json(),
        json!({ "style_id": "s-2", "app_name": null, "corrections": "bring" })
    );

    let apps = [AppToConfirm {
        bundle_id: "com.apple.mail".into(),
        app_name: Some("Mail".into()),
    }];
    client.confirm_apps(&apps).unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("POST", "/writing-styles/apps/confirm")
    );
    assert_eq!(
        request.json(),
        json!({ "apps": [{ "bundle_id": "com.apple.mail", "app_name": "Mail" }] })
    );

    client.delete_writing_style("s-2").unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("DELETE", "/writing-styles/s-2")
    );
}

#[test]
fn teaches_by_replying() {
    let server = Server::start(vec![
        Reply::ok(SESSION),
        Reply::ok(SESSION),
        Reply::ok(SESSION),
        Reply::no_content(),
        Reply::ok(r#"{"text":null}"#),
    ]);
    let client = client(&server);

    client.start_teach(Some("s-1")).unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("POST", "/writing-style/teach?style=s-1")
    );
    assert_eq!(request.header("Content-Length"), Some("0"));

    let session = client
        .add_teach_conversation("t-1", TeachKind::CodingAgent)
        .unwrap();
    assert_eq!(session.suggested_kinds, [TeachKind::Email]);
    let request = server.request();
    assert_eq!(request.target, "/writing-style/teach/t-1/conversations");
    assert_eq!(request.json(), json!({ "kind": "coding_agent" }));

    client
        .send_teach_reply("t-1", "c-2", "sounds good, ship it")
        .unwrap();
    let request = server.request();
    assert_eq!(
        request.target,
        "/writing-style/teach/t-1/conversations/c-2/replies"
    );
    assert_eq!(request.json(), json!({ "written": "sounds good, ship it" }));

    client.restart_teach_turn("t-1", "c-2").unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        (
            "POST",
            "/writing-style/teach/t-1/conversations/c-2/restart-turn"
        )
    );

    assert_eq!(client.teach_dictated("t-1", "c-2").unwrap().text, None);
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("GET", "/writing-style/teach/t-1/conversations/c-2/dictated")
    );
}

#[test]
fn discarding_a_session_thats_gone_is_fine() {
    let server = Server::start(vec![
        Reply::json(404, r#"{"detail":"No such session"}"#),
        Reply::json(500, r#"{"detail":"Disk full"}"#),
    ]);
    let client = client(&server);
    client.discard_teach("t-1").unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("DELETE", "/writing-style/teach/t-1")
    );

    let error = client.discard_teach("t-1").unwrap_err();
    assert_eq!(
        (error.status(), error.to_string()),
        (Some(500), "Disk full".to_string())
    );
}

#[test]
fn patches_send_only_what_changed() {
    let settings = include_str!("fixtures/capture_settings.json");
    let entry = r#"{"id":"d-1","written":"Kass","spoken":null,"places":[{"scope":"global"}]}"#;
    let server = Server::start(vec![
        Reply::ok(settings),
        Reply::ok(entry),
        Reply::ok(r#"{"deleted":true}"#),
    ]);
    let client = client(&server);

    let patch = CaptureSettingsUpdate {
        live_text: Some(false),
        input_device_id: Some(None),
        history_retention_days: Some(HistoryRetentionDays::Days30),
        ..Default::default()
    };
    client.update_capture_settings(&patch).unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("PUT", "/settings/captures")
    );
    assert_eq!(
        request.json(),
        json!({ "live_text": false, "input_device_id": null, "history_retention_days": 30 })
    );

    let patch = DictionaryEntryUpdate {
        spoken: Some(None),
        ..Default::default()
    };
    let entry = client.update_dictionary_entry("d-1", &patch).unwrap();
    assert!(entry.match_sound);
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("PATCH", "/dictionary/d-1")
    );
    assert_eq!(request.json(), json!({ "spoken": null }));

    assert!(client.delete_dictionary_entry("d-1").unwrap().deleted);
    assert_eq!(server.request().method, "DELETE");
}

#[test]
fn resolves_the_dictionary_for_an_app() {
    let server = Server::start(vec![Reply::ok(
        r#"{"entries":[],"prompt_terms":["Kass"],"dropped_terms":[]}"#,
    )]);
    let resolved = client(&server)
        .resolve_dictionary("com.todesktop.230313mzl4w4u92")
        .unwrap();
    assert_eq!(resolved.prompt_terms, ["Kass"]);
    assert_eq!(
        server.request().target,
        "/dictionary/resolved?bundle_id=com.todesktop.230313mzl4w4u92"
    );
}

#[test]
fn previews_retention_and_manages_models() {
    let server = Server::start(vec![
        Reply::ok(r#"{"days":30,"expiring":12}"#),
        Reply::ok(r#"{"message":"Downloading"}"#),
        Reply::ok(r#"{"message":"Cancelled"}"#),
        Reply::ok(r#"{"message":"Deleted"}"#),
        Reply::ok(r#"{"source":"/a","destination":"/b"}"#),
    ]);
    let client = client(&server);

    let preview = client
        .retention_preview(HistoryRetentionDays::Days30)
        .unwrap();
    assert_eq!(
        (preview.days, preview.expiring),
        (HistoryRetentionDays::Days30, 12)
    );
    assert_eq!(
        server.request().target,
        "/settings/captures/retention-preview?days=30"
    );

    client.trigger_model_download("whisper-turbo").unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("POST", "/models/download")
    );
    assert_eq!(request.json(), json!({ "model_name": "whisper-turbo" }));

    assert_eq!(
        client.cancel_download("whisper-turbo").unwrap().message,
        "Cancelled"
    );
    assert_eq!(server.request().target, "/models/download/cancel");

    client.delete_model("qwen3-1.7B").unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("DELETE", "/models/qwen3-1.7B")
    );

    let moved = client.migrate_models("/b").unwrap();
    assert_eq!((moved.moved, moved.errors.len()), (None, 0));
    assert_eq!(server.request().json(), json!({ "destination": "/b" }));
}

#[test]
fn reports_and_learning() {
    let server = Server::start(vec![Reply::no_content(), Reply::ok("{}"), Reply::ok("{}")]);
    let client = client(&server);

    client.withdraw_capture_report("c-1", "r-1").unwrap();
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("DELETE", "/captures/c-1/feedback/r-1")
    );

    client.pause_learning_for_recording().unwrap();
    assert_eq!(server.request().target, "/capture/learning/activity");

    let status = client.rollback_correction_learning().unwrap();
    assert!(!status.can_rollback);
    let request = server.request();
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("POST", "/capture/learning/rollback")
    );
}

#[test]
fn runs_a_command() {
    let server = Server::start(vec![Reply::ok(CAPTURE)]);
    client(&server)
        .run_command("teh text", "fix typos")
        .unwrap();
    let request = server.request();
    assert_eq!(request.target, "/commands/run");
    assert_eq!(
        request.json(),
        json!({ "selection": "teh text", "instruction": "fix typos" })
    );
}

#[test]
fn error_messages_come_from_the_detail() {
    let server = Server::start(vec![
        Reply::json(409, r#"{"detail":"A style called Work already exists"}"#),
        Reply::json(
            422,
            r#"{"detail":[{"loc":["body","name"],"msg":"field required"},{"message":"too long"}]}"#,
        ),
        Reply::json(
            400,
            r#"{"detail":{"message":"Pick a model first","code":"no_model"}}"#,
        ),
        Reply::json(500, r#"{"error":"boom"}"#),
        Reply::text(502, "Bad Gateway", "<html>upstream</html>"),
    ]);
    let client = client(&server);
    let message = |error: Error| (error.status(), error.to_string());

    assert_eq!(
        message(client.create_writing_style("Work").unwrap_err()),
        (Some(409), "A style called Work already exists".into())
    );
    assert_eq!(
        message(client.create_writing_style("").unwrap_err()),
        (Some(422), "field required; too long".into())
    );
    assert_eq!(
        message(client.capture_settings().unwrap_err()),
        (Some(400), "Pick a model first".into())
    );
    assert_eq!(
        message(client.list_dictionary().unwrap_err()),
        (Some(500), "HTTP error! status: 500".into())
    );
    assert_eq!(
        message(client.health().unwrap_err()),
        (Some(502), "Bad Gateway".into())
    );
}

#[test]
fn a_server_that_isnt_there_is_a_transport_error() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let error = ApiClient::new(format!("http://127.0.0.1:{port}"), None)
        .health()
        .unwrap_err();
    assert!(matches!(error, Error::Transport(_)), "{error:?}");
    assert_eq!(error.status(), None);
}

#[test]
fn a_body_that_doesnt_fit_is_a_decode_error() {
    let server = Server::start(vec![Reply::ok(r#"{"styles":"nope"}"#)]);
    assert!(matches!(
        client(&server).list_writing_styles(),
        Err(Error::Decode(_))
    ));
}
