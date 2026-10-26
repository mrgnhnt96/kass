//! HTTP calls the native take needs: result recovery, the batch fallback
//! upload and its refinement, commands, and the learning pause.

use std::time::Duration;

use serde_json::Value;

use super::protocol::TargetApp;
use super::stream::{classify_recovery, Recovery};

/// Says which Mac sent each request, so a shared Kass knows where captures came from.
pub fn client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in crate::this_mac::headers() {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
            headers.insert(name, value);
        }
    }
    reqwest::Client::builder()
        .tcp_nodelay(true)
        .default_headers(headers)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn base(server_url: &str) -> &str {
    server_url.trim_end_matches('/')
}

/// `GET /captures/stream/{id}/result`.
pub async fn fetch_result(http: &reqwest::Client, server_url: &str, session_id: &str) -> Recovery {
    let url = format!("{}/captures/stream/{session_id}/result", base(server_url));
    let response = match http.get(url).timeout(Duration::from_secs(5)).send().await {
        Ok(response) => response,
        Err(_) => return Recovery::Pending,
    };
    let status = response.status().as_u16();
    let body = response.json::<Value>().await.ok();
    classify_recovery(Some(status), body.as_ref(), session_id)
}

/// The server's `detail` (string or validation list) or the HTTP status.
fn error_detail(status: reqwest::StatusCode, body: Option<Value>) -> String {
    match body.as_ref().and_then(|b| b.get("detail")) {
        Some(Value::String(detail)) => detail.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("msg").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("; "),
        _ => format!("HTTP error! status: {}", status.as_u16()),
    }
}

async fn json_or_error(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if status.is_success() {
        response.json::<Value>().await.map_err(|e| e.to_string())
    } else {
        Err(error_detail(status, response.json::<Value>().await.ok()))
    }
}

/// Whether a server answers `GET /health` within a second.
pub async fn reachable(http: &reqwest::Client, server_url: &str) -> bool {
    http.get(format!("{}/health", base(server_url)))
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// `POST /captures` with the complete recording. `source` is `dictation`,
/// or `command` for a command take's instruction.
pub async fn upload(
    http: &reqwest::Client,
    server_url: &str,
    wav: Vec<u8>,
    source: &'static str,
    app: Option<TargetApp>,
) -> Result<Value, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let file = reqwest::multipart::Part::bytes(wav)
        .file_name(format!("dictation-{millis}.wav"))
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("source", source);
    if let Some(app) = app {
        if let Some(bundle_id) = app.bundle_id {
            form = form.text("app_bundle_id", bundle_id);
        }
        if let Some(name) = app.name {
            form = form.text("app_name", name);
        }
    }
    let response = http
        .post(format!("{}/captures", base(server_url)))
        .multipart(form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    json_or_error(response).await
}

/// `POST /captures/{id}/refine` with an empty body (server-side settings).
pub async fn refine(
    http: &reqwest::Client,
    server_url: &str,
    capture_id: &str,
) -> Result<Value, String> {
    let response = http
        .post(format!("{}/captures/{capture_id}/refine", base(server_url)))
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    json_or_error(response).await
}

/// `DELETE /captures/{id}`, for a take cancelled after the server saved it.
pub async fn delete_capture(http: &reqwest::Client, server_url: &str, capture_id: &str) {
    let url = format!("{}/captures/{capture_id}", base(server_url));
    match http.delete(url).send().await {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => eprintln!(
            "[dictation] could not delete cancelled capture {capture_id}: HTTP {}",
            response.status().as_u16()
        ),
        Err(e) => eprintln!("[dictation] could not delete cancelled capture {capture_id}: {e}"),
    }
}

/// What a command runs on its selection.
pub enum CommandInput<'a> {
    /// Typed or chosen in Kass: an instruction or a transform's name.
    Instruction {
        instruction: &'a str,
        bundle_id: Option<&'a str>,
        app_name: Option<&'a str>,
    },
    /// A command take's recording, saved through the batch upload.
    Recording { capture_id: &'a str },
}

/// `POST /commands/run`: rewrite `selection`; returns the command capture.
pub async fn run_command(
    http: &reqwest::Client,
    server_url: &str,
    selection: &str,
    input: CommandInput<'_>,
) -> Result<Value, String> {
    let body = match input {
        CommandInput::Instruction {
            instruction,
            bundle_id,
            app_name,
        } => serde_json::json!({
            "selection": selection,
            "instruction": instruction,
            "app_bundle_id": bundle_id,
            "app_name": app_name,
        }),
        CommandInput::Recording { capture_id } => {
            serde_json::json!({ "selection": selection, "capture_id": capture_id })
        }
    };
    let response = http
        .post(format!("{}/commands/run", base(server_url)))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    json_or_error(response).await
}

/// Tell background model learning to yield while the user records.
pub async fn pause_learning(http: &reqwest::Client, server_url: &str) {
    let _ = http
        .post(format!("{}/capture/learning/activity", base(server_url)))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
}

/// `POST /usage/takes`: how a take ended, for usage reports.
pub async fn report_take(
    http: &reqwest::Client,
    server_url: &str,
    mode: &str,
    outcome: &str,
    latency_ms: Option<u64>,
) {
    let _ = http
        .post(format!("{}/usage/takes", base(server_url)))
        .json(&serde_json::json!({ "mode": mode, "outcome": outcome, "latency_ms": latency_ms }))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: the batch fallback's upload and refine against a real server
    /// (see `transport::tests::real_server_smoke`; it saves a capture).
    #[tokio::test]
    #[ignore]
    async fn real_server_batch_smoke() {
        let server = std::env::var("KASS_SMOKE_SERVER").expect("KASS_SMOKE_SERVER");
        let wav = std::fs::read(std::env::var("KASS_SMOKE_WAV").expect("KASS_SMOKE_WAV")).unwrap();
        let http = client();
        let capture = upload(&http, &server, wav, "dictation", None)
            .await
            .unwrap();
        eprintln!(
            "[smoke] batch raw: {} auto_refine {} allow_auto_paste {}",
            capture["transcript_raw"], capture["auto_refine"], capture["allow_auto_paste"]
        );
        let id = capture["id"].as_str().unwrap();
        let refined = refine(&http, &server, id).await.unwrap();
        eprintln!("[smoke] batch refined: {}", refined["transcript_refined"]);
        assert_eq!(refined["id"], capture["id"]);
        let missing = fetch_result(&http, &server, "no-such-session").await;
        assert_eq!(missing, Recovery::Pending);
        let rejected = upload(&http, &server, b"not audio".to_vec(), "dictation", None)
            .await
            .unwrap_err();
        eprintln!("[smoke] bad upload: {rejected}");
    }

    #[test]
    fn error_detail_prefers_server_detail() {
        let status = reqwest::StatusCode::BAD_REQUEST;
        assert_eq!(
            error_detail(
                status,
                Some(serde_json::json!({"detail": "Could not decode audio"}))
            ),
            "Could not decode audio"
        );
        assert_eq!(
            error_detail(
                status,
                Some(serde_json::json!({"detail": [{"msg": "field required"}]}))
            ),
            "field required"
        );
        assert_eq!(error_detail(status, None), "HTTP error! status: 400");
    }
}
