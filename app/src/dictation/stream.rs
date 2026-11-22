//! Async driver for one streamed take, plus post-finish result recovery.
//!
//! The driver only talks to channels: the WebSocket transport (or a test
//! fake) sits on the other side of `out` / `incoming`, and the microphone
//! thread feeds `audio`. That keeps the timing rules testable without a
//! socket or a microphone.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;

use super::client::{Action, Outcome, StreamClient};
use super::protocol;

/// Messages from the microphone thread.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioMsg {
    /// The device is open at this sample rate. Sent before any frame.
    Format(u32),
    /// About 100 ms of mono PCM.
    Frame(Vec<i16>),
    /// Recording ended normally and every frame has been sent.
    End,
    /// Abandon the take.
    Cancel,
    /// From the selection reader of a command take: the text to rewrite.
    Selection(String),
    /// From the selection reader: nothing to rewrite; end the take with this
    /// message.
    Decline(String),
}

/// Events from the transport.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Open,
    Text(String),
    Closed,
}

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// From the start of the take until `ready`.
    pub handshake: Duration,
    /// From `finish` until `final`.
    pub finalize: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(5),
            finalize: Duration::from_secs(120),
        }
    }
}

/// Run one take's stream to its [`Outcome`]. `cancel` resolves when the user
/// presses Escape: the take is abandoned, even after `finish`.
pub async fn drive(
    client: StreamClient,
    out: UnboundedSender<Action>,
    incoming: UnboundedReceiver<Incoming>,
    audio: UnboundedReceiver<AudioMsg>,
    timeouts: Timeouts,
    cancel: impl Future<Output = ()>,
) -> Outcome {
    let mut client = client;
    let mut cancel = std::pin::pin!(cancel);
    let mut incoming = incoming;
    let mut audio = audio;
    let handshake_deadline = Instant::now() + timeouts.handshake;
    let mut finalize_deadline: Option<Instant> = None;
    let mut audio_open = true;
    // Recording ended normally. A command take's selection may still follow
    // on the same channel, so it stays open until every sender is gone.
    let mut ended = false;
    let mut incoming_open = true;
    loop {
        if let Some(outcome) = client.outcome() {
            return outcome.clone();
        }
        if client.finish_sent() && finalize_deadline.is_none() {
            finalize_deadline = Some(Instant::now() + timeouts.finalize);
        }
        let deadline = if client.is_ready() {
            finalize_deadline
        } else {
            Some(handshake_deadline)
        };
        let actions = tokio::select! {
            biased;
            // First: Escape wins over a `final` or `End` arriving with it.
            () = &mut cancel => client.abort(),
            message = incoming.recv(), if incoming_open => match message {
                Some(Incoming::Open) => client.on_open(),
                Some(Incoming::Text(text)) => client.on_text(&text),
                Some(Incoming::Closed) | None => {
                    incoming_open = false;
                    client.on_closed();
                    Vec::new()
                }
            },
            message = audio.recv(), if audio_open => match message {
                Some(AudioMsg::Format(rate)) => client.set_format(rate),
                Some(AudioMsg::Frame(pcm)) => client.push_audio(&pcm),
                Some(AudioMsg::End) => {
                    ended = true;
                    client.request_finish()
                }
                Some(AudioMsg::Selection(text)) => client.set_selection(text),
                Some(AudioMsg::Decline(message)) => {
                    audio_open = false;
                    client.decline(&message)
                }
                None if ended => {
                    audio_open = false;
                    Vec::new()
                }
                Some(AudioMsg::Cancel) | None => {
                    audio_open = false;
                    client.cancel()
                }
            },
            _ = sleep_until_opt(deadline), if deadline.is_some() => {
                client.fail(if client.is_ready() {
                    "Timed out waiting for the final transcript"
                } else {
                    "Timed out connecting to the server"
                });
                Vec::new()
            },
            else => {
                client.fail("Streaming stopped");
                Vec::new()
            }
        };
        for action in actions {
            if out.send(action).is_err() {
                client.on_closed();
                break;
            }
        }
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// One poll of `GET /captures/stream/{id}/result`, reduced to what matters.
#[derive(Debug, Clone, PartialEq)]
pub enum Recovery {
    Final(Value),
    /// The server retained a terminal failure for this session.
    Error(String),
    /// 202, 404, a network error or an unrelated body: try again.
    Pending,
}

/// Interpret one recovery response. `body` is `None` when the request failed.
pub fn classify_recovery(status: Option<u16>, body: Option<&Value>, session_id: &str) -> Recovery {
    let (Some(status), Some(body)) = (status, body) else {
        return Recovery::Pending;
    };
    if !(200..300).contains(&status) {
        return Recovery::Pending;
    }
    if body.get("type").and_then(Value::as_str) == Some("error")
        && body.get("session_id").and_then(Value::as_str) == Some(session_id)
    {
        return Recovery::Error(
            body.get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .unwrap_or("Streaming finalization failed")
                .to_string(),
        );
    }
    if protocol::is_valid_final(body, session_id) {
        return Recovery::Final(body.clone());
    }
    Recovery::Pending
}

pub const RECOVERY_ATTEMPTS: usize = 20;
pub const RECOVERY_DELAY: Duration = Duration::from_secs(1);
pub const INTERRUPTED_MESSAGE: &str =
    "Streaming finalization interrupted. Check Captures before recording again.";

/// Poll for a finished session's result. Never re-uploads audio.
pub async fn recover<F, Fut>(
    mut fetch: F,
    attempts: usize,
    delay: Duration,
) -> Result<Value, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Recovery>,
{
    for _ in 0..attempts {
        match fetch().await {
            Recovery::Final(event) => return Ok(event),
            Recovery::Error(message) => return Err(message),
            Recovery::Pending => {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(INTERRUPTED_MESSAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    struct Harness {
        out_rx: UnboundedReceiver<Action>,
        incoming: UnboundedSender<Incoming>,
        audio: UnboundedSender<AudioMsg>,
        escape: Option<tokio::sync::oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<Outcome>,
    }

    fn spawn(timeouts: Timeouts) -> Harness {
        spawn_with(StreamClient::new(1 << 20), timeouts)
    }

    fn spawn_with(client: StreamClient, timeouts: Timeouts) -> Harness {
        let (out_tx, out_rx) = unbounded_channel();
        let (in_tx, in_rx) = unbounded_channel();
        let (audio_tx, audio_rx) = unbounded_channel();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel = async move {
            if cancel_rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let task = tokio::spawn(drive(client, out_tx, in_rx, audio_rx, timeouts, cancel));
        Harness {
            out_rx,
            incoming: in_tx,
            audio: audio_tx,
            escape: Some(cancel_tx),
            task,
        }
    }

    impl Harness {
        fn press_escape(&mut self) {
            let _ = self.escape.take().unwrap().send(());
        }
    }

    async fn next_text(h: &mut Harness) -> Value {
        loop {
            match h.out_rx.recv().await.expect("driver closed") {
                Action::Text(t) => return serde_json::from_str(&t).unwrap(),
                Action::Binary(_) => continue,
            }
        }
    }

    fn final_event() -> String {
        serde_json::json!({
            "type": "final",
            "refinement_complete": true,
            "capture": {"id": "s1", "transcript_raw": "hi"}
        })
        .to_string()
    }

    #[tokio::test]
    async fn audio_spoken_while_connecting_reaches_the_server_before_finish() {
        let mut h = spawn(Timeouts::default());
        // Key-down: the mic opens and records before the socket connects.
        h.audio.send(AudioMsg::Format(48_000)).unwrap();
        h.audio.send(AudioMsg::Frame(vec![1; 4800])).unwrap();
        h.audio.send(AudioMsg::Frame(vec![2; 4800])).unwrap();
        // Key-up before the server is ready.
        h.audio.send(AudioMsg::End).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        let mut binary = 0;
        loop {
            match h.out_rx.recv().await.unwrap() {
                Action::Binary(frame) => {
                    assert_eq!(frame.len(), protocol::HEADER_BYTES + 9600);
                    binary += 1;
                }
                Action::Text(t) => {
                    assert_eq!(t, protocol::finish_message(None));
                    break;
                }
            }
        }
        assert_eq!(binary, 2);
        h.incoming.send(Incoming::Text(final_event())).unwrap();
        assert!(matches!(h.task.await.unwrap(), Outcome::Final(_)));
    }

    #[tokio::test]
    async fn a_command_released_before_its_selection_is_read_finishes_after_it() {
        let mut h = spawn_with(
            StreamClient::new(1 << 20).with_command(),
            Timeouts::default(),
        );
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        assert_eq!(next_text(&mut h).await["source"], "command");
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        h.audio.send(AudioMsg::Frame(vec![1; 1600])).unwrap();
        h.audio.send(AudioMsg::End).unwrap();
        // The selection reader is still working: nothing may finish yet.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(h.out_rx.try_recv(), Ok(Action::Binary(_))));
        assert!(h.out_rx.try_recv().is_err());
        h.audio
            .send(AudioMsg::Selection("the text".into()))
            .unwrap();
        assert_eq!(next_text(&mut h).await["type"], "selection");
        assert_eq!(next_text(&mut h).await["type"], "finish");
        // Every sender gone after a normal end is not a cancel.
        drop(h.audio);
        h.incoming.send(Incoming::Text(final_event())).unwrap();
        assert!(matches!(h.task.await.unwrap(), Outcome::Final(_)));
    }

    #[tokio::test]
    async fn a_command_with_nothing_selected_is_declined_at_once() {
        let mut h = spawn_with(
            StreamClient::new(1 << 20).with_command(),
            Timeouts::default(),
        );
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        h.audio
            .send(AudioMsg::Decline("Select text".into()))
            .unwrap();
        assert_eq!(next_text(&mut h).await["type"], "cancel");
        assert_eq!(
            h.task.await.unwrap(),
            Outcome::Declined("Select text".into())
        );
    }

    #[tokio::test]
    async fn no_ready_within_the_handshake_timeout_falls_back() {
        let h = spawn(Timeouts {
            handshake: Duration::from_millis(30),
            finalize: Duration::from_secs(5),
        });
        h.audio.send(AudioMsg::Format(48_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        assert!(matches!(
            h.task.await.unwrap(),
            Outcome::FailedBeforeFinish(_)
        ));
    }

    #[tokio::test]
    async fn no_final_within_the_finalize_timeout_needs_recovery() {
        let mut h = spawn(Timeouts {
            handshake: Duration::from_secs(5),
            finalize: Duration::from_millis(30),
        });
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        h.audio.send(AudioMsg::Frame(vec![1; 1600])).unwrap();
        h.audio.send(AudioMsg::End).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        assert_eq!(next_text(&mut h).await["type"], "finish");
        assert_eq!(
            h.task.await.unwrap(),
            Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: None
            }
        );
    }

    #[tokio::test]
    async fn cancel_sends_cancel_and_settles() {
        let mut h = spawn(Timeouts::default());
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        h.audio.send(AudioMsg::Cancel).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        assert_eq!(next_text(&mut h).await["type"], "cancel");
        assert_eq!(h.task.await.unwrap(), Outcome::Cancelled);
    }

    #[tokio::test]
    async fn escape_while_recording_tells_the_server_to_discard() {
        let mut h = spawn(Timeouts::default());
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        h.audio.send(AudioMsg::Frame(vec![1; 1600])).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        h.press_escape();
        // The microphone's end, right behind Escape, finishes nothing.
        h.audio.send(AudioMsg::End).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "cancel");
        assert_eq!(h.task.await.unwrap(), Outcome::Cancelled);
        assert!(h.out_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn escape_while_the_server_finishes_discards_the_take() {
        let mut h = spawn(Timeouts::default());
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        h.audio.send(AudioMsg::Frame(vec![1; 1600])).unwrap();
        h.audio.send(AudioMsg::End).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        assert_eq!(next_text(&mut h).await["type"], "finish");
        // Transcribing or refining: Escape still cancels.
        h.press_escape();
        assert_eq!(next_text(&mut h).await["type"], "cancel");
        assert_eq!(h.task.await.unwrap(), Outcome::Cancelled);
    }

    #[tokio::test]
    async fn microphone_thread_vanishing_cancels_the_take() {
        let h = spawn(Timeouts::default());
        drop(h.audio);
        assert_eq!(h.task.await.unwrap(), Outcome::Cancelled);
    }

    #[tokio::test]
    async fn transport_closing_after_finish_needs_recovery() {
        let mut h = spawn(Timeouts::default());
        h.audio.send(AudioMsg::Format(16_000)).unwrap();
        h.incoming.send(Incoming::Open).unwrap();
        h.incoming
            .send(Incoming::Text(
                r#"{"type":"ready","session_id":"s1"}"#.into(),
            ))
            .unwrap();
        h.audio.send(AudioMsg::Frame(vec![1; 1600])).unwrap();
        h.audio.send(AudioMsg::End).unwrap();
        assert_eq!(next_text(&mut h).await["type"], "start");
        assert_eq!(next_text(&mut h).await["type"], "finish");
        h.incoming.send(Incoming::Closed).unwrap();
        assert!(matches!(
            h.task.await.unwrap(),
            Outcome::FailedAfterFinish { .. }
        ));
    }

    fn final_for(id: &str) -> Value {
        serde_json::json!({
            "type": "final",
            "refinement_complete": true,
            "capture": {"id": id, "transcript_raw": ""}
        })
    }

    #[test]
    fn recovery_classification() {
        let good = final_for("s1");
        assert_eq!(
            classify_recovery(Some(200), Some(&good), "s1"),
            Recovery::Final(good.clone())
        );
        assert_eq!(
            classify_recovery(Some(200), Some(&final_for("other")), "s1"),
            Recovery::Pending
        );
        assert_eq!(
            classify_recovery(
                Some(202),
                Some(&serde_json::json!({"type": "pending"})),
                "s1"
            ),
            Recovery::Pending
        );
        assert_eq!(classify_recovery(Some(404), None, "s1"), Recovery::Pending);
        assert_eq!(classify_recovery(None, None, "s1"), Recovery::Pending);
        let error = serde_json::json!({"type": "error", "session_id": "s1", "message": "Recognition failed"});
        assert_eq!(
            classify_recovery(Some(200), Some(&error), "s1"),
            Recovery::Error("Recognition failed".into())
        );
    }

    #[tokio::test]
    async fn recovery_polls_until_the_final_result() {
        let mut calls = 0;
        let result = recover(
            || {
                calls += 1;
                let reply = if calls < 3 {
                    Recovery::Pending
                } else {
                    Recovery::Final(final_for("s1"))
                };
                async move { reply }
            },
            RECOVERY_ATTEMPTS,
            Duration::ZERO,
        )
        .await;
        assert_eq!(result.unwrap()["capture"]["id"], "s1");
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn recovery_stops_at_a_retained_error() {
        let mut calls = 0;
        let result = recover(
            || {
                calls += 1;
                async { Recovery::Error("Recognition failed".into()) }
            },
            RECOVERY_ATTEMPTS,
            Duration::ZERO,
        )
        .await;
        assert_eq!(result.unwrap_err(), "Recognition failed");
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn recovery_gives_up_with_a_check_captures_message() {
        let result = recover(|| async { Recovery::Pending }, 3, Duration::ZERO).await;
        assert_eq!(result.unwrap_err(), INTERRUPTED_MESSAGE);
    }
}
