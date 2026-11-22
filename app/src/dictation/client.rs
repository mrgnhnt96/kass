//! Sans-IO client for one `/captures/stream` take.
//!
//! Feed it socket and audio events; it returns what to send and, once
//! settled, the take's [`Outcome`]. Holding no socket keeps every rule here
//! (buffer until `ready`, contiguous sequence/offsets, finish once, what a
//! failure means) unit-testable.

use serde_json::Value;

use super::protocol::{self, ServerEvent, TargetApp};

/// Something the transport must send.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Text(String),
    Binary(Vec<u8>),
}

/// How a take's stream ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A validated `final` event.
    Final(Value),
    /// The server never received `finish`: nothing was persisted, so the
    /// complete recording can be transcribed through the batch endpoint.
    FailedBeforeFinish(String),
    /// `finish` was sent. Never batch-upload now (the capture may already be
    /// saved). With `terminal_error` the server reported the failure;
    /// otherwise recover through `GET /captures/stream/{id}/result`.
    FailedAfterFinish {
        session_id: String,
        terminal_error: Option<String>,
    },
    /// The take was abandoned (too short, microphone failure, shutdown,
    /// Escape).
    Cancelled,
    /// A command take refused before it began: nothing to rewrite. The
    /// message says why.
    Declined(String),
}

/// The text a voice edit may change, once read from the take's target.
type LastTakeSource = Box<dyn Fn() -> Option<LastTakeText> + Send>;

/// The text before the caret a voice edit may change, and how much of its
/// end Kass's last take wrote, with that take's capture.
#[derive(Debug, Clone, PartialEq)]
pub struct LastTakeText {
    pub text: String,
    pub capture_id: Option<String>,
    pub own_chars: usize,
}

/// Called when the user names a writing style, with the style before (if
/// any) and the one asked for. Returns whether it scheduled a style cue.
type StyleCallback = Box<dyn Fn(Option<String>, String) -> bool + Send>;

pub struct StreamClient {
    sample_rate: Option<u32>,
    opened: bool,
    start_sent: bool,
    ready: bool,
    session_id: Option<String>,
    sequence: u32,
    sample_offset: u32,
    pending: Vec<Vec<u8>>,
    pending_bytes: usize,
    max_pending_bytes: usize,
    finish_requested: bool,
    finish_sent: bool,
    outcome: Option<Outcome>,
    on_provisional: Option<Box<dyn Fn(String) + Send>>,
    on_style: Option<StyleCallback>,
    style_cue_delay_ms: u32,
    style_cue_ms: u32,
    target_app: Option<Box<dyn Fn() -> Option<TargetApp> + Send>>,
    app_sent: bool,
    field_before: Option<Box<dyn Fn() -> Option<String> + Send>>,
    context_sent: bool,
    last_take: Option<LastTakeSource>,
    last_take_sent: bool,
    on_edit: Option<Box<dyn Fn() -> bool + Send>>,
    edit_cue_delay_ms: u32,
    edit_cue_ms: u32,
    start_cue_ms: u32,
    /// A command take (docs/plans/COMMAND_MODE.md): `finish` waits for the
    /// selection, which follows `start`.
    command: bool,
    selection: Selection,
}

/// A command take's selection, as far as it has gone.
#[derive(Debug, Clone, PartialEq)]
enum Selection {
    Unknown,
    Known(String),
    Sent,
}

impl StreamClient {
    /// `max_pending_bytes` bounds audio held while waiting for `ready`.
    pub fn new(max_pending_bytes: usize) -> Self {
        Self {
            sample_rate: None,
            opened: false,
            start_sent: false,
            ready: false,
            session_id: None,
            sequence: 0,
            sample_offset: 0,
            pending: Vec::new(),
            pending_bytes: 0,
            max_pending_bytes,
            finish_requested: false,
            finish_sent: false,
            outcome: None,
            on_provisional: None,
            on_style: None,
            style_cue_delay_ms: 0,
            style_cue_ms: 0,
            target_app: None,
            app_sent: false,
            field_before: None,
            context_sent: false,
            last_take: None,
            last_take_sent: false,
            on_edit: None,
            edit_cue_delay_ms: 0,
            edit_cue_ms: 0,
            start_cue_ms: 0,
            command: false,
            selection: Selection::Unknown,
        }
    }

    /// The start cue played as the take began and may be in its first
    /// `start_cue_ms` of audio.
    pub fn with_start_cue(mut self, start_cue_ms: u32) -> Self {
        self.start_cue_ms = start_cue_ms;
        self
    }

    /// Make this a command take: its words are an instruction for the
    /// selection passed to [`Self::set_selection`].
    pub fn with_command(mut self) -> Self {
        self.command = true;
        self
    }

    /// Whether `finish` must wait for the selection.
    fn awaiting_selection(&self) -> bool {
        self.command && self.selection == Selection::Unknown
    }

    /// The `selection` message, once known and the server is ready.
    fn selection_action(&mut self) -> Option<Action> {
        if !self.ready {
            return None;
        }
        match std::mem::replace(&mut self.selection, Selection::Sent) {
            Selection::Known(text) => Some(Action::Text(protocol::selection_message(&text))),
            other => {
                self.selection = other;
                None
            }
        }
    }

    /// Everything that goes out before audio: the app, context, last take
    /// and selection.
    fn preamble(&mut self) -> Vec<Action> {
        self.app_action()
            .into_iter()
            .chain(self.context_action())
            .chain(self.last_take_action())
            .chain(self.selection_action())
            .collect()
    }

    /// The `app` message, the first time the target app is known. The server
    /// picks the app's writing style from it, so it goes out with the first
    /// audio after the focus snapshot, well before a phrase is cleaned up.
    fn app_action(&mut self) -> Option<Action> {
        if self.app_sent || !self.ready {
            return None;
        }
        let app = self.target_app.as_ref().and_then(|app| app())?;
        self.app_sent = true;
        Some(Action::Text(protocol::app_message(&app)))
    }

    /// The selection a command take rewrites, read after key-down. Sends a
    /// `finish` that was waiting for it.
    pub fn set_selection(&mut self, text: String) -> Vec<Action> {
        if self.outcome.is_some() || !self.awaiting_selection() {
            return Vec::new();
        }
        self.selection = Selection::Known(text);
        let mut actions = self.preamble();
        if self.ready && self.finish_requested && !self.finish_sent {
            self.finish_sent = true;
            actions.push(Action::Text(self.finish_message()));
        }
        actions
    }

    /// Receive provisional cleaned text for this take, after `finish`.
    pub fn with_provisional(mut self, on_provisional: impl Fn(String) + Send + 'static) -> Self {
        self.on_provisional = Some(Box::new(on_provisional));
        self
    }

    /// Hear when the user asks for a writing style by name, with the style
    /// it replaced (`None` when the take was already in it) and the new one.
    /// `on_style` returns whether it scheduled a cue, which plays `delay_ms`
    /// later; the microphone may pick up the `cue_ms` from then, which the
    /// server then doesn't take for a voice.
    pub fn with_style(
        mut self,
        delay_ms: u32,
        cue_ms: u32,
        on_style: impl Fn(Option<String>, String) -> bool + Send + 'static,
    ) -> Self {
        self.style_cue_delay_ms = delay_ms;
        self.style_cue_ms = cue_ms;
        self.on_style = Some(Box::new(on_style));
        self
    }

    /// A `cue` message: the audio from `delay_ms` past what was sent so far,
    /// for `ms`, holds a sound Kass played, not the user's voice.
    fn cue_action(&self, delay_ms: u32, ms: u32) -> Option<Action> {
        let rate = self.sample_rate?;
        let samples = |ms: u32| (u64::from(rate) * u64::from(ms) / 1000) as u32;
        let start = self.sample_offset.saturating_add(samples(delay_ms));
        let span = samples(ms);
        let message = serde_json::json!({
            "type": "cue",
            "start_samples": start,
            "end_samples": start.saturating_add(span),
        });
        Some(Action::Text(message.to_string()))
    }

    /// Name the take's target app: in an `app` message as soon as it is known
    /// (the focus snapshot is taken just after the take starts), and again in
    /// `finish` for servers that only read it there.
    pub fn with_target_app(
        mut self,
        target_app: impl Fn() -> Option<TargetApp> + Send + 'static,
    ) -> Self {
        self.target_app = Some(Box::new(target_app));
        self
    }

    /// Send the field's text before the caret once it is known. Read just
    /// after key-down, it goes out with the next audio (or `finish`), before
    /// the server recognizes the first phrase.
    pub fn with_field_before(
        mut self,
        field_before: impl Fn() -> Option<String> + Send + 'static,
    ) -> Self {
        self.field_before = Some(Box::new(field_before));
        self
    }

    /// The `context` message, the first time the text is known. Only once
    /// the server is ready: it expects the start message first.
    fn context_action(&mut self) -> Option<Action> {
        if self.context_sent || !self.ready {
            return None;
        }
        let before = self.field_before.as_ref().and_then(|f| f())?;
        self.context_sent = true;
        Some(Action::Text(protocol::context_message(&before)))
    }

    /// Send the text before the caret in the app this take goes to, once
    /// read, so the take can be a voice edit of it
    /// (docs/plans/VOICE_EDITS.md).
    pub fn with_last_take(
        mut self,
        last_take: impl Fn() -> Option<LastTakeText> + Send + 'static,
    ) -> Self {
        self.last_take = Some(Box::new(last_take));
        self
    }

    fn last_take_action(&mut self) -> Option<Action> {
        if self.last_take_sent || !self.ready {
            return None;
        }
        let last = self.last_take.as_ref().and_then(|f| f())?;
        self.last_take_sent = true;
        Some(Action::Text(protocol::last_take_message(
            &last.text,
            last.capture_id.as_deref(),
            last.own_chars,
        )))
    }

    /// Hear when the take opens as a voice edit, while the user still
    /// speaks. `on_edit` returns whether it scheduled a cue, which plays
    /// `delay_ms` later and may reach the microphone for `cue_ms`, as with
    /// [`Self::with_style`].
    pub fn with_edit(
        mut self,
        delay_ms: u32,
        cue_ms: u32,
        on_edit: impl Fn() -> bool + Send + 'static,
    ) -> Self {
        self.edit_cue_delay_ms = delay_ms;
        self.edit_cue_ms = cue_ms;
        self.on_edit = Some(Box::new(on_edit));
        self
    }

    fn finish_message(&self) -> String {
        let app = self.target_app.as_ref().and_then(|app| app());
        protocol::finish_message(app.as_ref())
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn finish_sent(&self) -> bool {
        self.finish_sent
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// The capture's sample rate is known (the device is open).
    pub fn set_format(&mut self, sample_rate: u32) -> Vec<Action> {
        if self.sample_rate.is_none() {
            self.sample_rate = Some(sample_rate);
        }
        self.maybe_start()
    }

    /// The socket connected.
    pub fn on_open(&mut self) -> Vec<Action> {
        self.opened = true;
        self.maybe_start()
    }

    fn maybe_start(&mut self) -> Vec<Action> {
        match (
            self.outcome.is_none(),
            self.opened,
            self.start_sent,
            self.sample_rate,
        ) {
            (true, true, false, Some(rate)) => {
                self.start_sent = true;
                vec![Action::Text(protocol::start_message(
                    rate,
                    self.on_provisional.is_some(),
                    self.start_cue_ms,
                    self.command,
                ))]
            }
            _ => Vec::new(),
        }
    }

    /// One mono PCM frame from the microphone.
    pub fn push_audio(&mut self, pcm: &[i16]) -> Vec<Action> {
        if self.outcome.is_some() || self.finish_requested {
            return Vec::new();
        }
        let mut actions = self.preamble();
        for chunk in pcm.chunks(protocol::MAX_SAMPLES_PER_MESSAGE) {
            let frame = protocol::encode_frame(self.sequence, self.sample_offset, chunk);
            self.sequence = self.sequence.wrapping_add(1);
            self.sample_offset = self.sample_offset.wrapping_add(chunk.len() as u32);
            if self.ready {
                actions.push(Action::Binary(frame));
            } else {
                self.pending_bytes += frame.len();
                self.pending.push(frame);
                if self.pending_bytes > self.max_pending_bytes {
                    self.fail("Server did not accept audio in time");
                    return Vec::new();
                }
            }
        }
        actions
    }

    /// The hotkey was released and the last frame pushed.
    pub fn request_finish(&mut self) -> Vec<Action> {
        if self.outcome.is_some() || self.finish_requested {
            return Vec::new();
        }
        self.finish_requested = true;
        if self.ready && !self.awaiting_selection() {
            self.finish_sent = true;
            let mut actions = self.preamble();
            actions.push(Action::Text(self.finish_message()));
            actions
        } else {
            Vec::new()
        }
    }

    /// A text message from the server.
    pub fn on_text(&mut self, text: &str) -> Vec<Action> {
        if self.outcome.is_some() {
            return Vec::new();
        }
        match protocol::parse_server_event(text) {
            ServerEvent::Ready { session_id } if !self.ready => {
                self.ready = true;
                self.session_id = Some(session_id);
                self.pending_bytes = 0;
                let mut actions = self.preamble();
                actions.extend(self.pending.drain(..).map(Action::Binary));
                if self.finish_requested && !self.awaiting_selection() {
                    self.finish_sent = true;
                    actions.push(Action::Text(self.finish_message()));
                }
                actions
            }
            ServerEvent::Final(event) => {
                if let Some(id) = self.session_id.as_deref() {
                    if self.finish_sent && protocol::is_valid_final(&event, id) {
                        self.outcome = Some(Outcome::Final(event));
                    }
                }
                Vec::new()
            }
            ServerEvent::Provisional { session_id, text } => {
                let ours = session_id.is_none() || session_id == self.session_id;
                if let (true, true, Some(sink)) = (self.finish_sent, ours, &self.on_provisional) {
                    sink(text);
                }
                Vec::new()
            }
            ServerEvent::Style {
                session_id,
                from,
                to,
            } => {
                let ours = session_id.is_none() || session_id == self.session_id;
                let played = match (ours, &self.on_style) {
                    (true, Some(sink)) => sink(from, to),
                    _ => false,
                };
                // After finish the server has all the audio; nothing to mark.
                if played && !self.finish_sent {
                    self.cue_action(self.style_cue_delay_ms, self.style_cue_ms)
                        .into_iter()
                        .collect()
                } else {
                    Vec::new()
                }
            }
            ServerEvent::Edit { session_id } => {
                let ours = session_id.is_none() || session_id == self.session_id;
                let played = ours && self.on_edit.as_ref().is_some_and(|sink| sink());
                if played && !self.finish_sent {
                    self.cue_action(self.edit_cue_delay_ms, self.edit_cue_ms)
                        .into_iter()
                        .collect()
                } else {
                    Vec::new()
                }
            }
            ServerEvent::Error(message) => {
                self.settle_failure(&message, true);
                Vec::new()
            }
            ServerEvent::Invalid => {
                self.fail("Invalid message from server");
                Vec::new()
            }
            ServerEvent::Ready { .. } | ServerEvent::Update => Vec::new(),
        }
    }

    /// The socket closed or errored.
    pub fn on_closed(&mut self) {
        self.fail("Streaming connection closed");
    }

    /// Give up (timeout, overload). Meaning depends on whether finish was sent.
    pub fn fail(&mut self, reason: &str) {
        self.settle_failure(reason, false);
    }

    fn settle_failure(&mut self, reason: &str, from_server: bool) {
        if self.outcome.is_some() {
            return;
        }
        self.pending.clear();
        self.pending_bytes = 0;
        self.outcome = Some(match (self.finish_sent, self.session_id.clone()) {
            (true, Some(session_id)) => Outcome::FailedAfterFinish {
                session_id,
                terminal_error: from_server.then(|| reason.to_string()),
            },
            _ => Outcome::FailedBeforeFinish(reason.to_string()),
        });
    }

    /// Abandon the take. Returns `cancel` when the server is listening.
    pub fn cancel(&mut self) -> Vec<Action> {
        self.abandon(Outcome::Cancelled)
    }

    /// Refuse a command take (nothing selected). Like [`Self::cancel`], with
    /// the reason to show.
    pub fn decline(&mut self, message: &str) -> Vec<Action> {
        self.abandon(Outcome::Declined(message.to_string()))
    }

    /// The user pressed Escape. Unlike [`Self::cancel`], also after `finish`:
    /// the server then discards the session instead of saving its capture.
    pub fn abort(&mut self) -> Vec<Action> {
        if self.outcome.is_some() {
            return Vec::new();
        }
        self.abandon(Outcome::Cancelled);
        if self.start_sent {
            vec![Action::Text(protocol::cancel_message())]
        } else {
            Vec::new()
        }
    }

    fn abandon(&mut self, outcome: Outcome) -> Vec<Action> {
        if self.outcome.is_some() {
            return Vec::new();
        }
        self.outcome = Some(outcome);
        self.pending.clear();
        self.pending_bytes = 0;
        if self.start_sent && !self.finish_sent {
            vec![Action::Text(protocol::cancel_message())]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::protocol::HEADER_BYTES;

    fn texts(actions: &[Action]) -> Vec<Value> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Text(t) => Some(serde_json::from_str(t).unwrap()),
                _ => None,
            })
            .collect()
    }

    fn frames(actions: &[Action]) -> Vec<Vec<u8>> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Binary(b) => Some(b.clone()),
                _ => None,
            })
            .collect()
    }

    fn header(frame: &[u8]) -> (u32, u32) {
        (
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            u32::from_le_bytes(frame[4..8].try_into().unwrap()),
        )
    }

    fn final_event(id: &str) -> String {
        serde_json::json!({
            "type": "final",
            "refinement_complete": true,
            "capture": {"id": id, "transcript_raw": "hello", "transcript_refined": "Hello."}
        })
        .to_string()
    }

    fn ready(client: &mut StreamClient) -> Vec<Action> {
        client.on_text(r#"{"type":"ready","session_id":"s1"}"#)
    }

    #[test]
    fn start_waits_for_both_socket_and_format() {
        let mut client = StreamClient::new(1 << 20);
        assert!(client.on_open().is_empty());
        let sent = client.set_format(16_000);
        assert_eq!(texts(&sent)[0]["sample_rate"], 16_000);

        let mut client = StreamClient::new(1 << 20);
        assert!(client.set_format(48_000).is_empty());
        assert_eq!(texts(&client.on_open())[0]["type"], "start");
    }

    #[test]
    fn buffers_audio_until_ready_then_flushes_in_order() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        // Audio arrives while the socket is still connecting.
        assert!(client.push_audio(&[1, -2]).is_empty());
        client.on_open();
        assert!(client.push_audio(&[3]).is_empty());
        let flushed = frames(&ready(&mut client));
        assert_eq!(flushed.len(), 2);
        assert_eq!(header(&flushed[0]), (0, 0));
        assert_eq!(&flushed[0][HEADER_BYTES..], &[1, 0, 0xFE, 0xFF]);
        assert_eq!(header(&flushed[1]), (1, 2));
        // Once ready, audio goes straight out with contiguous numbering.
        let live = frames(&client.push_audio(&[4, 5]));
        assert_eq!(header(&live[0]), (2, 3));
    }

    #[test]
    fn oversized_frames_are_split_within_the_message_limit() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        let pcm = vec![7i16; protocol::MAX_SAMPLES_PER_MESSAGE + 10];
        let sent = frames(&client.push_audio(&pcm));
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|f| f.len() <= protocol::MAX_MESSAGE_BYTES));
        assert_eq!(
            header(&sent[1]),
            (1, protocol::MAX_SAMPLES_PER_MESSAGE as u32)
        );
    }

    #[test]
    fn finish_before_ready_is_sent_after_the_buffered_audio() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        client.push_audio(&[1]);
        assert!(client.request_finish().is_empty());
        assert!(!client.finish_sent());
        let sent = ready(&mut client);
        assert!(matches!(sent[0], Action::Binary(_)));
        assert_eq!(texts(&sent), vec![serde_json::json!({"type": "finish"})]);
        assert!(client.finish_sent());
    }

    #[test]
    fn finish_names_the_target_app_known_by_then() {
        let mut client = StreamClient::new(1 << 20).with_target_app(|| {
            Some(TargetApp {
                bundle_id: Some("com.apple.Notes".into()),
                name: Some("Notes".into()),
                category: None,
            })
        });
        client.set_format(48_000);
        client.on_open();
        // Known at ready, so the app message went out then.
        assert_eq!(texts(&ready(&mut client))[0]["type"], "app");
        assert_eq!(
            texts(&client.request_finish()),
            vec![serde_json::json!({
                "type": "finish",
                "app": { "bundle_id": "com.apple.Notes", "name": "Notes" },
            })]
        );
    }

    #[test]
    fn the_app_goes_out_once_with_the_first_audio_after_focus_is_known() {
        let focus = std::sync::Arc::new(std::sync::Mutex::new(None::<TargetApp>));
        let read = focus.clone();
        let mut client =
            StreamClient::new(1 << 20).with_target_app(move || read.lock().unwrap().clone());
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        // The focus snapshot hasn't landed: audio goes alone.
        assert!(texts(&client.push_audio(&[1])).is_empty());
        *focus.lock().unwrap() = Some(TargetApp {
            bundle_id: Some("com.tinyspeck.slackmacgap".into()),
            name: Some("Slack".into()),
            category: None,
        });
        let actions = client.push_audio(&[2]);
        // Ahead of the audio, so the style is set before anything is cleaned.
        assert!(matches!(actions[0], Action::Text(_)));
        assert_eq!(
            texts(&actions),
            vec![serde_json::json!({
                "type": "app",
                "bundle_id": "com.tinyspeck.slackmacgap",
                "name": "Slack",
            })]
        );
        assert!(texts(&client.push_audio(&[3])).is_empty());
    }

    #[test]
    fn the_app_known_before_ready_goes_out_with_the_buffered_audio() {
        let mut client = StreamClient::new(1 << 20).with_target_app(|| {
            Some(TargetApp {
                bundle_id: Some("com.apple.mail".into()),
                name: Some("Mail".into()),
                category: None,
            })
        });
        client.set_format(48_000);
        client.on_open();
        client.push_audio(&[1]);
        let sent = ready(&mut client);
        assert!(matches!(sent[0], Action::Text(_)));
        assert!(matches!(sent[1], Action::Binary(_)));
        assert_eq!(texts(&sent)[0]["type"], "app");
    }

    #[test]
    fn field_text_goes_out_once_with_the_first_audio_after_it_is_known() {
        let known = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let read = known.clone();
        let mut client =
            StreamClient::new(1 << 20).with_field_before(move || read.lock().unwrap().clone());
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        // Not read yet: audio goes alone.
        assert!(texts(&client.push_audio(&[1])).is_empty());
        *known.lock().unwrap() = Some("I think we should".into());
        let actions = client.push_audio(&[2]);
        assert_eq!(
            texts(&actions),
            vec![serde_json::json!({"type": "context", "before": "I think we should"})]
        );
        // Ahead of the audio it came with.
        assert!(matches!(actions[0], Action::Text(_)));
        assert_eq!(frames(&actions).len(), 1);
        assert!(texts(&client.push_audio(&[3])).is_empty());
        assert_eq!(texts(&client.request_finish()).len(), 1);
    }

    #[test]
    fn field_text_waits_for_ready_and_precedes_buffered_audio() {
        let mut client = StreamClient::new(1 << 20).with_field_before(|| Some("Can you".into()));
        client.set_format(48_000);
        client.on_open();
        assert!(texts(&client.push_audio(&[1])).is_empty());
        let actions = ready(&mut client);
        assert!(matches!(&actions[0], Action::Text(t) if t.contains("\"context\"")));
        assert_eq!(frames(&actions).len(), 1);
    }

    #[test]
    fn field_text_known_only_at_release_precedes_finish() {
        let known = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let read = known.clone();
        let mut client =
            StreamClient::new(1 << 20).with_field_before(move || read.lock().unwrap().clone());
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        *known.lock().unwrap() = Some("Can you".into());
        let sent: Vec<String> = texts(&client.request_finish())
            .iter()
            .map(|v| v["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(sent, ["context", "finish"]);
    }

    #[test]
    fn finish_is_sent_once() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        assert_eq!(texts(&client.request_finish()).len(), 1);
        assert!(client.request_finish().is_empty());
        // Audio after finish is ignored.
        assert!(client.push_audio(&[1]).is_empty());
    }

    #[test]
    fn valid_final_settles_the_take() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.request_finish();
        client.on_text(&final_event("other-session"));
        assert_eq!(client.outcome(), None);
        client.on_text(&final_event("s1"));
        match client.outcome() {
            Some(Outcome::Final(v)) => assert_eq!(v["capture"]["id"], "s1"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn the_start_cue_is_announced_in_the_start_message() {
        let mut client = StreamClient::new(1 << 20).with_start_cue(300);
        client.set_format(48_000);
        assert_eq!(texts(&client.on_open())[0]["start_cue_ms"], 300);
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        assert_eq!(texts(&client.on_open())[0].get("start_cue_ms"), None);
    }

    #[test]
    fn provisional_text_is_asked_for_only_with_a_sink() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        assert_eq!(texts(&client.on_open())[0].get("provisional"), None);
        let mut client = StreamClient::new(1 << 20).with_provisional(|_| {});
        client.set_format(48_000);
        assert_eq!(texts(&client.on_open())[0]["provisional"], true);
    }

    #[test]
    fn provisional_text_after_finish_reaches_the_sink() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut client =
            StreamClient::new(1 << 20).with_provisional(move |t| sink.lock().unwrap().push(t));
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        let event = r#"{"type":"provisional","session_id":"s1","text":"Hello"}"#;
        // Before release there is no live text.
        client.on_text(event);
        client.request_finish();
        client.on_text(event);
        client.on_text(r#"{"type":"provisional","session_id":"other","text":"Nope"}"#);
        assert_eq!(*seen.lock().unwrap(), vec!["Hello".to_string()]);
        assert_eq!(client.outcome(), None);
    }

    #[test]
    fn a_spoken_style_reaches_the_sink_while_speaking() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut client = StreamClient::new(1 << 20).with_style(0, 350, move |from, to| {
            sink.lock().unwrap().push((from, to));
            false
        });
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.on_text(r#"{"type":"style","session_id":"s1","name":"Formal","from_name":"Chat"}"#);
        client.on_text(r#"{"type":"style","session_id":"s1","name":"Chat","from_name":null}"#);
        client.on_text(r#"{"type":"style","session_id":"other","name":"Nope"}"#);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (Some("Chat".to_string()), "Formal".to_string()),
                (None, "Chat".to_string())
            ]
        );
        assert_eq!(client.outcome(), None);
    }

    #[test]
    fn a_style_cue_marks_the_audio_it_may_reach() {
        let mut client = StreamClient::new(1 << 20).with_style(920, 350, |_, _| true);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.push_audio(&vec![0i16; 4800]);
        let event = r#"{"type":"style","session_id":"s1","name":"Formal","from_name":"Chat"}"#;
        assert_eq!(
            texts(&client.on_text(event)),
            // It plays 920 ms on, as the chip glows.
            vec![
                serde_json::json!({"type": "cue", "start_samples": 4800 + 44_160, "end_samples": 4800 + 44_160 + 16_800})
            ]
        );
        // Cues off: nothing played, nothing to mark.
        let mut quiet = StreamClient::new(1 << 20).with_style(920, 350, |_, _| false);
        quiet.set_format(48_000);
        quiet.on_open();
        ready(&mut quiet);
        assert!(quiet.on_text(event).is_empty());
    }

    #[test]
    fn the_last_take_goes_out_once_its_target_is_known() {
        let known = std::sync::Arc::new(std::sync::Mutex::new(false));
        let read = known.clone();
        let mut client = StreamClient::new(1 << 20).with_last_take(move || {
            (*read.lock().unwrap()).then(|| LastTakeText {
                text: "I typed this. Hi Megan.".into(),
                capture_id: Some("c1".into()),
                own_chars: 9,
            })
        });
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        assert!(texts(&client.push_audio(&[1])).is_empty());
        *known.lock().unwrap() = true;
        assert_eq!(
            texts(&client.push_audio(&[2])),
            vec![serde_json::json!({
                "type": "last_take",
                "text": "I typed this. Hi Megan.",
                "capture_id": "c1",
                "own_chars": 9,
            })]
        );
        assert!(texts(&client.push_audio(&[3])).is_empty());
    }

    #[test]
    fn an_edit_cue_marks_the_audio_it_may_reach() {
        let mut client = StreamClient::new(1 << 20).with_edit(0, 350, || true);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.push_audio(&vec![0i16; 4800]);
        let event = r#"{"type":"edit","session_id":"s1"}"#;
        assert_eq!(
            texts(&client.on_text(event)),
            vec![
                serde_json::json!({"type": "cue", "start_samples": 4800, "end_samples": 4800 + 16_800})
            ]
        );
        // Another session's, or without a sink: nothing.
        assert!(client
            .on_text(r#"{"type":"edit","session_id":"other"}"#)
            .is_empty());
        let mut plain = StreamClient::new(1 << 20);
        plain.set_format(48_000);
        plain.on_open();
        ready(&mut plain);
        assert!(plain.on_text(event).is_empty());
    }

    #[test]
    fn close_before_finish_allows_batch_fallback() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.on_closed();
        assert!(matches!(
            client.outcome(),
            Some(Outcome::FailedBeforeFinish(_))
        ));
    }

    #[test]
    fn close_after_finish_requires_recovery_not_batch() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.request_finish();
        client.on_closed();
        assert_eq!(
            client.outcome(),
            Some(&Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: None
            })
        );
    }

    #[test]
    fn server_error_after_finish_is_terminal() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.request_finish();
        client.on_text(r#"{"type":"error","message":"Recognition failed"}"#);
        assert_eq!(
            client.outcome(),
            Some(&Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: Some("Recognition failed".into())
            })
        );
    }

    #[test]
    fn server_error_before_finish_falls_back() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.on_text(r#"{"type":"error","message":"overloaded"}"#);
        assert!(matches!(
            client.outcome(),
            Some(Outcome::FailedBeforeFinish(_))
        ));
    }

    #[test]
    fn exceeding_the_pending_bound_falls_back_instead_of_dropping_audio() {
        let mut client = StreamClient::new(64);
        client.set_format(48_000);
        client.push_audio(&[0; 20]);
        assert_eq!(client.outcome(), None);
        client.push_audio(&[0; 20]);
        assert!(matches!(
            client.outcome(),
            Some(Outcome::FailedBeforeFinish(_))
        ));
    }

    #[test]
    fn finish_requested_while_connecting_then_close_still_falls_back() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.push_audio(&[1]);
        client.request_finish();
        client.on_closed();
        assert!(matches!(
            client.outcome(),
            Some(Outcome::FailedBeforeFinish(_))
        ));
    }

    fn open_and_ready(client: &mut StreamClient) -> Vec<Action> {
        client.set_format(16_000);
        client.on_open();
        client.on_text(r#"{"type":"ready","session_id":"s1"}"#)
    }

    #[test]
    fn a_command_take_sends_its_selection_before_audio() {
        let mut client = StreamClient::new(1 << 20).with_command();
        client.set_format(16_000);
        let start = texts(&client.on_open());
        assert_eq!(start[0]["source"], "command");
        assert!(client.set_selection("the text".into()).is_empty());
        let sent = texts(&client.on_text(r#"{"type":"ready","session_id":"s1"}"#));
        assert_eq!(
            sent,
            vec![serde_json::json!({"type": "selection", "text": "the text"})]
        );
        // Only once, and never a second selection.
        assert!(texts(&client.push_audio(&[1, 2])).is_empty());
        assert!(client.set_selection("other".into()).is_empty());
    }

    #[test]
    fn finish_waits_for_a_selection_still_being_read() {
        let mut client = StreamClient::new(1 << 20).with_command();
        open_and_ready(&mut client);
        client.push_audio(&[1, 2]);
        assert!(client.request_finish().is_empty());
        assert!(!client.finish_sent());
        let sent: Vec<_> = texts(&client.set_selection("late".into()))
            .iter()
            .map(|t| t["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(sent, ["selection", "finish"]);
        assert!(client.finish_sent());
    }

    #[test]
    fn a_dictation_take_finishes_without_a_selection() {
        let mut client = StreamClient::new(1 << 20);
        open_and_ready(&mut client);
        let sent = texts(&client.request_finish());
        assert_eq!(sent[0]["type"], "finish");
    }

    #[test]
    fn a_declined_command_cancels_the_session_and_says_why() {
        let mut client = StreamClient::new(1 << 20).with_command();
        open_and_ready(&mut client);
        assert_eq!(texts(&client.decline("Select text"))[0]["type"], "cancel");
        assert_eq!(
            client.outcome(),
            Some(&Outcome::Declined("Select text".into()))
        );
        assert!(client.set_selection("late".into()).is_empty());
    }

    #[test]
    fn escape_after_finish_still_tells_the_server_to_discard() {
        let mut client = StreamClient::new(1 << 20);
        open_and_ready(&mut client);
        client.push_audio(&[1]);
        client.request_finish();
        // A plain cancel after finish says nothing: the capture is committed.
        let aborted = texts(&client.abort());
        assert_eq!(aborted, vec![serde_json::json!({"type": "cancel"})]);
        assert_eq!(client.outcome(), Some(&Outcome::Cancelled));
        // A final arriving afterwards is ignored.
        assert!(client.on_text(&final_event("s1")).is_empty());
        assert_eq!(client.outcome(), Some(&Outcome::Cancelled));
        assert!(client.abort().is_empty());
    }

    #[test]
    fn escape_before_the_server_hears_the_start_sends_nothing() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        assert!(client.abort().is_empty());
        assert_eq!(client.outcome(), Some(&Outcome::Cancelled));
        // The socket opening afterwards doesn't start a session.
        assert!(client.on_open().is_empty());
    }

    #[test]
    fn cancel_tells_a_listening_server_and_settles() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        assert_eq!(texts(&client.cancel())[0]["type"], "cancel");
        assert_eq!(client.outcome(), Some(&Outcome::Cancelled));
        // A settled take ignores later events.
        client.on_closed();
        assert_eq!(client.outcome(), Some(&Outcome::Cancelled));
    }

    #[test]
    fn fail_after_finish_needs_recovery() {
        let mut client = StreamClient::new(1 << 20);
        client.set_format(48_000);
        client.on_open();
        ready(&mut client);
        client.request_finish();
        client.fail("timed out");
        assert!(matches!(
            client.outcome(),
            Some(Outcome::FailedAfterFinish {
                terminal_error: None,
                ..
            })
        ));
    }
}
