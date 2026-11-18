//! Streamed dictation: the `/captures/stream` WebSocket and its result
//! recovery. Python: `backend/routes/capture_stream.py`.
//!
//! The client sends a start object, then PCM frames and small commands; the
//! session answers with events as it recognizes and cleans, and a final
//! event with the saved capture once the user releases. A connection that
//! drops after release still saves the take, and its result waits here for
//! the client to ask.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use kass_text::py;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::request::detail;
use crate::stream::models::{self, Foreground};
use crate::stream::session::{Session, MAX_SELECTION_CHARS};
use crate::stream::{Outcome, Place};
use crate::AppState;

/// How long the start message may take.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the session waits for audio or a command.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_COMMAND_CHARS: usize = 4096;
/// A command session's selection message carries up to
/// `MAX_SELECTION_CHARS` of text, some of it escaped.
const MAX_SELECTION_MESSAGE_CHARS: usize = 2 * MAX_SELECTION_CHARS + MAX_COMMAND_CHARS;
const MAX_APP_FIELD_CHARS: usize = 255;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/captures/stream", get(stream_capture))
        .route(
            "/captures/stream/{session_id}/result",
            get(streaming_result).layer(axum::middleware::from_fn(crate::cors::layer)),
        )
}

async fn streaming_result(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    match state.streams.outcome(&session_id) {
        Outcome::Done(result) => Json(result).into_response(),
        Outcome::Pending => {
            (StatusCode::ACCEPTED, Json(json!({"type": "pending"}))).into_response()
        }
        Outcome::Unknown => detail(
            StatusCode::NOT_FOUND,
            "Streaming result unavailable; inspect Captures before retrying",
        ),
    }
}

/// Browsers must come from an allowed origin; native clients, which send
/// none, only from this Mac. The Host and forwarded headers are the
/// client's own, so they don't count.
fn allowed(headers: &HeaderMap, peer: Option<SocketAddr>) -> bool {
    if let Some(origin) = headers.get("origin") {
        return origin
            .to_str()
            .is_ok_and(|origin| origin != "null" && crate::cors::allowed(origin));
    }
    match peer.map(|peer| peer.ip()) {
        Some(IpAddr::V6(address)) => match address.to_ipv4_mapped() {
            Some(address) => address.is_loopback(),
            None => address.is_loopback(),
        },
        Some(IpAddr::V4(address)) => address.is_loopback(),
        None => false,
    }
}

async fn stream_capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let peer = peer.map(|Extension(ConnectInfo(peer))| peer);
    let place = match state.streams.enter() {
        Some(place) if allowed(&headers, peer) => place,
        // Refused before the handshake, as Starlette closes an unaccepted
        // socket with 1008.
        _ => return StatusCode::FORBIDDEN.into_response(),
    };
    upgrade.on_upgrade(move |socket| serve(state, socket, place))
}

/// The target app field as stored: trimmed, at most 255 characters, None
/// when blank or not text.
pub(crate) fn app_field(value: Option<&Value>) -> Option<String> {
    let cleaned = py::head(py::strip(value?.as_str()?), MAX_APP_FIELD_CHARS);
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// The bundle id, name and category of an app message.
fn target_app(fields: &Map<String, Value>) -> (Option<String>, Option<String>, Option<String>) {
    (
        app_field(fields.get("bundle_id")),
        app_field(fields.get("name")),
        app_field(fields.get("category")),
    )
}

/// What a connection holds, released however it ends.
struct Connection {
    app: AppState,
    events: mpsc::UnboundedSender<Value>,
    session: Option<Arc<Session>>,
    worker: Option<JoinHandle<Result<(), String>>>,
    foreground: Option<Foreground>,
}

/// How a connection ended.
enum Ended {
    /// Cancelled, finished, or the client went away.
    Quietly,
    Failed(String),
}

impl From<String> for Ended {
    fn from(error: String) -> Self {
        Ended::Failed(error)
    }
}

async fn serve(app: AppState, socket: WebSocket, place: Place) {
    let (sink, mut incoming) = socket.split();
    let (events, outgoing) = mpsc::unbounded_channel();
    let writer = tokio::spawn(write(sink, outgoing));
    let mut connection = Connection {
        app,
        events,
        session: None,
        worker: None,
        foreground: None,
    };
    if let Ended::Failed(error) = connection.run(&mut incoming).await {
        connection.failed(error);
    }
    connection.close().await;
    // Null closes the socket once every event before it is sent.
    let _ = connection.events.send(Value::Null);
    drop(connection);
    let _ = writer.await;
    drop(place);
}

/// Send the session's events in order; a send that fails is dropped, since
/// a finished take is saved whether or not the client still listens.
async fn write(
    mut sink: SplitSink<WebSocket, Message>,
    mut outgoing: mpsc::UnboundedReceiver<Value>,
) {
    while let Some(event) = outgoing.recv().await {
        if event.is_null() {
            break;
        }
        let _ = sink.send(Message::Text(event.to_string().into())).await;
    }
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "".into(),
        })))
        .await;
    let _ = sink.close().await;
}

/// The next text or binary message; None when the client went away.
async fn receive(incoming: &mut SplitStream<WebSocket>) -> Option<Message> {
    loop {
        match incoming.next().await? {
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(message @ (Message::Text(_) | Message::Binary(_))) => return Some(message),
            Ok(_) => continue,
        }
    }
}

fn object(text: &str, problem: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(text).map_err(|e| e.to_string())? {
        Value::Object(fields) => Ok(fields),
        _ => Err(problem.to_string()),
    }
}

impl Connection {
    async fn run(&mut self, incoming: &mut SplitStream<WebSocket>) -> Ended {
        let start = match tokio::time::timeout(START_TIMEOUT, receive(incoming)).await {
            Err(_) => return Ended::Failed("Timed out waiting for the start message".into()),
            Ok(None) => return Ended::Quietly,
            Ok(Some(Message::Text(text))) => text,
            Ok(Some(_)) => return Ended::Failed("Expected a start object".into()),
        };
        if py::len(&start) > MAX_COMMAND_CHARS {
            return Ended::Failed("Start message too large".into());
        }
        let start = match object(&start, "Expected a start object") {
            Ok(start) => Value::Object(start),
            Err(error) => return Ended::Failed(error),
        };
        let (session, foreground) =
            match Session::open(&self.app, &start, self.events.clone()).await {
                Ok(opened) => opened,
                Err(error) => return Ended::Failed(error),
            };
        self.app.streams.running(&session.id);
        self.session = Some(session.clone());
        self.foreground = Some(foreground);
        let _ = self.events.send(json!({
            "type": "ready",
            "session_id": session.id,
            "auto_refine": session.settings.auto_refine,
            "allow_auto_paste": session.settings.allow_auto_paste,
        }));
        self.worker = Some(tokio::spawn(session.clone().run()));
        // A command session prefills the same model when its selection
        // arrives.
        let cleans = session.cleans();
        if cleans {
            let proxy = self.app.proxy.clone();
            let model = session.settings.llm_model.clone();
            let flags = session.flags();
            tokio::spawn(async move {
                if let Err(error) = models::load_cleanup(&proxy, &model, Some(&flags)).await {
                    // The cleanup loads it anyway, just later.
                    eprintln!("[server] Could not load the cleanup model ahead of time: {error}");
                }
            });
        }
        match self.listen(&session, incoming, cleans).await {
            Ok(ended) => ended,
            Err(error) => Ended::Failed(error),
        }
    }

    /// Audio and commands until finish, cancel or a failure.
    async fn listen(
        &mut self,
        session: &Arc<Session>,
        incoming: &mut SplitStream<WebSocket>,
        cleans: bool,
    ) -> Result<Ended, String> {
        let limit = if session.is_command() {
            MAX_SELECTION_MESSAGE_CHARS
        } else {
            MAX_COMMAND_CHARS
        };
        loop {
            let worker = self.worker.as_mut().expect("the session runs");
            let message = tokio::select! {
                joined = worker => {
                    self.worker = None;
                    return Err(match joined {
                        Ok(Err(error)) => error,
                        Ok(Ok(())) => "Recognition ended before finish".into(),
                        Err(error) => error.to_string(),
                    });
                }
                message = tokio::time::timeout(IDLE_TIMEOUT, receive(incoming)) => message,
            };
            let message = match message {
                Err(_) => return Err("Streaming session timed out waiting for audio".into()),
                Ok(None) => return Ok(Ended::Quietly),
                Ok(Some(message)) => message,
            };
            let text = match message {
                Message::Binary(frame) => {
                    session.append(&frame)?;
                    continue;
                }
                Message::Text(text) => text,
                _ => continue,
            };
            if py::len(&text) > limit {
                return Err("Command too large".into());
            }
            let command = object(&text, "Expected a command object")?;
            let field = |name: &str| command.get(name).cloned().unwrap_or(Value::Null);
            match command.get("type").and_then(Value::as_str) {
                Some("cancel") => return Ok(Ended::Quietly),
                // A sound the app played (the style cue), which the
                // microphone may pick up.
                Some("cue") => {
                    session.ignore_cue(&field("start_samples"), &field("end_samples"))?
                }
                // The field's text before the caret, read just after key-down.
                Some("context") => session.set_context(&field("before"))?,
                // The text before the caret, which a voice edit may change.
                Some("last_take") => session.set_last_take(
                    &field("text"),
                    &field("capture_id"),
                    &field("own_chars"),
                )?,
                // The target app, from the focus snapshot at key-down.
                Some("app") => {
                    let (bundle_id, name, category) = target_app(&command);
                    if session.set_app(bundle_id, name, category).await? && cleans {
                        prefill_style(&self.app, session);
                    }
                }
                // A command session's selected text, read just after key-down.
                Some("selection") => session.set_selection(&field("text"))?,
                Some("finish") => {
                    if session.samples() == 0 {
                        return Err("Cannot finish empty audio".into());
                    }
                    if let Some(Value::Object(app)) = command.get("app") {
                        let (bundle_id, name, category) = target_app(app);
                        session.set_app(bundle_id, name, category).await?;
                    }
                    return self.finish(session, incoming).await;
                }
                _ => {
                    return Err(
                        "Expected app, context, last_take, cue, selection, finish or cancel".into(),
                    )
                }
            }
        }
    }

    /// The user released: finish the take, save it and send the result. A
    /// dropped connection still saves it, for the client to recover; only a
    /// cancel (Escape after release) discards it.
    async fn finish(
        &mut self,
        session: &Arc<Session>,
        incoming: &mut SplitStream<WebSocket>,
    ) -> Result<Ended, String> {
        session.finish();
        if self.cancelled_while_finishing(incoming).await? {
            return Ok(Ended::Quietly);
        }
        let capture = session.persist().await?;
        let mut result = json!({
            "type": "final",
            "session_id": session.id,
            "revision": session.revision() + 1,
            "covered_samples": session.samples(),
            "capture": capture,
            "refinement_complete": true,
            "refinement_error": session.refinement_error(),
            "degraded_reason": session.degraded_reason(),
        });
        let edit = session.edit_result();
        if let Some(edit) = &edit {
            // A voice edit: the app changes its last take instead of pasting.
            result["edit"] = edit.clone();
        }
        eprintln!(
            "[server] Dictation stream finished: {}",
            session.timing_summary()
        );
        self.app.streams.keep(&session.id, result.clone());
        let _ = self.events.send(result);
        let after = session.clone();
        if edit.is_some() {
            tokio::spawn(async move { after.learn_from_edit().await });
        } else if !session.is_command() {
            // How it was said, measured once the text is out.
            let text = [&capture["transcript_refined"], &capture["transcript_raw"]]
                .into_iter()
                .filter_map(Value::as_str)
                .find(|text| !text.is_empty())
                .unwrap_or_default()
                .to_string();
            tokio::spawn(async move { after.measure(text).await });
        }
        Ok(Ended::Quietly)
    }

    /// Wait for the last recognition and cleanup, listening for a cancel;
    /// the session's failure, if it failed.
    async fn cancelled_while_finishing(
        &mut self,
        incoming: &mut SplitStream<WebSocket>,
    ) -> Result<bool, String> {
        let Some(worker) = self.worker.as_mut() else {
            return Ok(false);
        };
        let mut listening = true;
        let ran = loop {
            let message = tokio::select! {
                joined = &mut *worker => break joined,
                message = receive(incoming), if listening => message,
            };
            match message {
                None => listening = false,
                Some(Message::Text(text)) => {
                    let cancel = serde_json::from_str::<Value>(&text)
                        .is_ok_and(|command| command["type"] == "cancel");
                    if cancel {
                        return Ok(true);
                    }
                }
                Some(_) => {}
            }
        };
        self.worker = None;
        match ran {
            Ok(ran) => ran.map(|()| false),
            Err(error) => Err(error.to_string()),
        }
    }

    fn failed(&self, error: String) {
        let summary = match &self.session {
            Some(session) => session.timing_summary(),
            None => "before start".to_string(),
        };
        eprintln!("[server] Streaming capture failed: {error} ({summary})");
        let event = json!({
            "type": "error",
            "message": error,
            "session_id": self.session.as_ref().map(|session| session.id.clone()),
        });
        if let Some(session) = self.session.as_ref().filter(|session| session.finished()) {
            self.app.streams.keep(&session.id, event.clone());
        }
        let _ = self.events.send(event);
    }

    /// Stop the session, wait for the inference it runs and drop its
    /// recording unless it was saved.
    async fn close(&mut self) {
        if let Some(session) = &self.session {
            session.abort();
            session.finish();
            if let Some(worker) = self.worker.take() {
                let _ = worker.await;
            }
            session.close();
            self.app.streams.stopped(&session.id);
        }
        if let Some(foreground) = self.foreground.take() {
            foreground.end().await;
        }
    }
}

/// Cache the prompt of the session's style while the user speaks.
fn prefill_style(app: &AppState, session: &Arc<Session>) {
    let proxy = app.proxy.clone();
    let (prompt, name) = session.style_prompt();
    tokio::spawn(async move {
        let started = Instant::now();
        let prefilled = async {
            // Read back what macOS paged out first, so the prefill doesn't
            // hold recognition up faulting it in.
            models::keep_resident(&proxy, "dictation started", true, false).await?;
            models::prefill_cleanup(&proxy, &prompt).await
        }
        .await;
        match prefilled {
            Ok(()) => eprintln!(
                "[server] Prefilled the {name} style in {:.3}s",
                started.elapsed().as_secs_f64()
            ),
            // Only a head start: the first cleanup prefills it anyway.
            Err(error) => eprintln!("[server] Could not prefill the cleanup prompt: {error}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn originless_sockets_only_from_this_mac() {
        let none = HeaderMap::new();
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5000".parse().unwrap();
        let remote: SocketAddr = "192.168.1.4:5000".parse().unwrap();
        assert!(allowed(&none, Some(local)));
        assert!(allowed(&none, Some(mapped)));
        assert!(!allowed(&none, Some(remote)));
        assert!(!allowed(&none, None));
        let mut browser = HeaderMap::new();
        browser.insert("origin", "tauri://localhost".parse().unwrap());
        assert!(allowed(&browser, Some(remote)));
        browser.insert("origin", "null".parse().unwrap());
        assert!(!allowed(&browser, Some(local)));
    }

    #[test]
    fn app_fields_are_trimmed_text() {
        assert_eq!(app_field(Some(&json!("  Mail "))), Some("Mail".into()));
        assert_eq!(app_field(Some(&json!("   "))), None);
        assert_eq!(app_field(Some(&json!(3))), None);
        assert_eq!(app_field(None), None);
        assert_eq!(app_field(Some(&json!("x".repeat(300)))).unwrap().len(), 255);
    }
}
