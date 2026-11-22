//! Settling a take once its stream has an [`Outcome`]: deliver the final
//! capture, recover it after a dropped connection, or fall back to a batch
//! upload when the server never received `finish`.
//!
//! Side effects go through [`TakeEnv`] so every branch is tested with fakes.

use std::future::Future;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::audio;
use super::client::Outcome;
use super::delivery::{self, Delivery};
use super::stream::{self, Recovery};
use crate::sound_cues::Cue;

/// Shortest take worth transcribing.
pub const MIN_RECORDING: Duration = Duration::from_millis(500);
pub const ERROR_VISIBLE_MS: u64 = 6000;
pub const BRIEF_NOTICE_MS: u64 = 2000;

/// Pill state sent to the dictate window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PillEvent {
    Preparing,
    /// The microphone is delivering sound.
    Recording,
    Transcribing {
        elapsed_ms: u64,
    },
    Refining,
    Done,
    Error {
        message: String,
        visible_ms: u64,
    },
    /// Escape cancelled the take: hide the pill at once.
    Cancelled,
}

impl PillEvent {
    /// A short note rather than a failure, shown as briefly as a too-short take.
    pub fn notice(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            visible_ms: BRIEF_NOTICE_MS,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        let visible_ms = if message == delivery::SHORT_RECORDING_MESSAGE {
            BRIEF_NOTICE_MS
        } else {
            ERROR_VISIBLE_MS
        };
        Self::Error {
            message,
            visible_ms,
        }
    }

    /// The sound this state change plays. `Transcribing` is sent the moment
    /// the microphone closes on a take long enough to transcribe, so the stop
    /// cue can't bleed into the recording; every error, including a take too
    /// short to keep, plays the error cue. A take with nothing to paste
    /// finishes as `Done`: silence isn't a failure. The start cue is played
    /// by `dictation::start` itself, before the microphone opens. Escape
    /// plays a softer stop: the user asked for it, so it isn't an error.
    pub fn cue(&self) -> Option<Cue> {
        match self {
            PillEvent::Transcribing { .. } => Some(Cue::Stop),
            PillEvent::Error { .. } => Some(Cue::Error),
            PillEvent::Cancelled => Some(Cue::Cancel),
            _ => None,
        }
    }
}

/// How a take ended, for usage reports, from the last state it showed: its
/// text went in, or it failed. A take too short to keep, declined or
/// cancelled isn't reported.
pub fn report_outcome(last: Option<&PillEvent>) -> Option<&'static str> {
    match last? {
        PillEvent::Done => Some("delivered"),
        PillEvent::Error { visible_ms, .. } if *visible_ms == ERROR_VISIBLE_MS => Some("failed"),
        _ => None,
    }
}

/// The complete recording, kept for the batch fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
}

impl Recorded {
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.pcm.len() as f64 / self.sample_rate as f64)
    }
}

pub trait TakeEnv {
    fn emit(&self, event: PillEvent);
    fn capture_created(&self, capture: &Value);
    fn capture_updated(&self, capture_id: &str);
    fn accessibility_missing(&self);
    /// Paste into the target focused at chord start.
    fn paste(&self, text: String) -> impl Future<Output = Result<bool, String>> + Send;
    /// Change the last take in the target focused at chord start so it ends
    /// in `after` instead of `before` (a voice edit). `Err` says why nothing
    /// changed.
    fn edit(
        &self,
        before: String,
        after: String,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn fetch_result(&self, session_id: String) -> impl Future<Output = Recovery> + Send;
    /// `POST /captures` with a WAV file; returns the create response.
    fn upload(&self, wav: Vec<u8>) -> impl Future<Output = Result<Value, String>> + Send;
    /// `POST /captures/{id}/refine`; returns the refined capture.
    fn refine(&self, capture_id: String) -> impl Future<Output = Result<Value, String>> + Send;
    /// Whether the user pressed Escape on this take.
    fn cancelled(&self) -> bool;
    /// Claim the take for insertion; `false` once Escape cancelled it.
    /// Escape leaves a claimed take alone.
    fn begin_delivery(&self) -> bool;
    /// Delete a capture the server saved for a take cancelled too late to
    /// stop the save.
    fn discard(&self, capture_id: String) -> impl Future<Output = ()> + Send;
    fn recovery_delay(&self) -> Duration {
        stream::RECOVERY_DELAY
    }
}

/// Finish a take. `recorded` resolves once recording has stopped, with the
/// complete audio (`None` if the microphone failed or the take was dropped).
pub async fn settle<E, R>(env: &E, outcome: Outcome, recorded: R)
where
    E: TakeEnv,
    R: Future<Output = Option<Recorded>>,
{
    match outcome {
        Outcome::Final(event) => deliver_final(env, &event).await,
        Outcome::FailedAfterFinish {
            terminal_error: Some(message),
            ..
        } => env.emit(PillEvent::error(message)),
        Outcome::FailedAfterFinish {
            session_id,
            terminal_error: None,
        } => {
            // The server may already have saved this capture: never upload
            // it again, only ask for the result.
            let delay = env.recovery_delay();
            let fetch = || env.fetch_result(session_id.clone());
            match stream::recover(fetch, stream::RECOVERY_ATTEMPTS, delay).await {
                Ok(event) => deliver_final(env, &event).await,
                Err(message) => env.emit(PillEvent::error(message)),
            }
        }
        Outcome::FailedBeforeFinish(reason) => {
            let Some(recorded) = recorded.await else {
                return;
            };
            if env.cancelled() {
                return;
            }
            if recorded.duration() < MIN_RECORDING {
                env.emit(PillEvent::error(delivery::SHORT_RECORDING_MESSAGE));
                return;
            }
            eprintln!(
                "[dictation] streaming unavailable ({reason}); transcribing the complete recording"
            );
            batch(env, recorded).await;
        }
        // Escape: the pill is already hidden, and a short take says nothing.
        Outcome::Cancelled if env.cancelled() => {}
        Outcome::Cancelled => {
            if let Some(recorded) = recorded.await {
                if recorded.duration() < MIN_RECORDING {
                    env.emit(PillEvent::error(delivery::SHORT_RECORDING_MESSAGE));
                }
            }
        }
        Outcome::Declined(message) => {
            // After the microphone has stopped, so its own last state
            // doesn't replace the message.
            let _ = recorded.await;
            env.emit(PillEvent::notice(message));
        }
    }
}

async fn deliver_final<E: TakeEnv>(env: &E, event: &Value) {
    let capture = event.get("capture");
    let id = capture
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    // Before the capture is announced, so a cancelled one never shows up.
    if !env.begin_delivery() {
        if let Some(id) = id {
            env.discard(id).await;
        }
        return;
    }
    if let Some(capture) = capture {
        env.capture_created(capture);
    }
    deliver(env, delivery::plan_final(event), id).await;
}

/// Insert the take's text. `capture_id` is discarded instead if Escape got
/// there first.
async fn deliver<E: TakeEnv>(env: &E, plan: Delivery, capture_id: Option<String>) {
    if !env.begin_delivery() {
        if let Some(id) = capture_id {
            env.discard(id).await;
        }
        return;
    }
    match plan {
        Delivery::Paste(text) => match delivery::paste_failure(env.paste(text).await) {
            None => env.emit(PillEvent::Done),
            Some((message, accessibility)) => {
                if accessibility {
                    env.accessibility_missing();
                }
                env.emit(PillEvent::error(message));
            }
        },
        Delivery::Edit { before, after } => match env.edit(before, after).await {
            Ok(()) => env.emit(PillEvent::Done),
            Err(message) => env.emit(PillEvent::error(message)),
        },
        Delivery::Nothing => env.emit(PillEvent::Done),
        Delivery::Error(message) => env.emit(PillEvent::error(message)),
    }
}

async fn batch<E: TakeEnv>(env: &E, recorded: Recorded) {
    let wav = match audio::encode_wav(&recorded.pcm, recorded.sample_rate) {
        Ok(wav) => wav,
        Err(message) => return env.emit(PillEvent::error(message)),
    };
    let capture = match env.upload(wav).await {
        Ok(capture) => capture,
        Err(message) => return env.emit(PillEvent::error(delivery::upload_failure(&message))),
    };
    let id = capture
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    if env.cancelled() {
        if let Some(id) = id {
            env.discard(id).await;
        }
        return;
    }
    env.capture_created(&capture);
    let allow_auto_paste = capture
        .get("allow_auto_paste")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let auto_refine = capture
        .get("auto_refine")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(id) = id else {
        return env.emit(PillEvent::error("Upload returned no capture"));
    };
    if !auto_refine {
        let plan = delivery::plan(&capture, allow_auto_paste, None);
        return deliver(env, plan, Some(id)).await;
    }
    env.emit(PillEvent::Refining);
    let refined = env.refine(id.clone()).await;
    if env.cancelled() {
        return env.discard(id).await;
    }
    match refined {
        Ok(refined) => {
            env.capture_updated(&id);
            let plan = delivery::plan(&refined, allow_auto_paste, None);
            deliver(env, plan, Some(id)).await;
        }
        Err(message) if message.is_empty() => env.emit(PillEvent::error("Refinement failed")),
        Err(message) => env.emit(PillEvent::error(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::cancel::CancelSwitch;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[test]
    fn reports_delivered_and_failed_takes_only() {
        assert_eq!(report_outcome(Some(&PillEvent::Done)), Some("delivered"));
        assert_eq!(
            report_outcome(Some(&PillEvent::error("Transcription failed"))),
            Some("failed")
        );
        assert_eq!(
            report_outcome(Some(&PillEvent::error(delivery::SHORT_RECORDING_MESSAGE))),
            None
        );
        assert_eq!(
            report_outcome(Some(&PillEvent::notice("Nothing selected"))),
            None
        );
        assert_eq!(
            report_outcome(Some(&PillEvent::Transcribing { elapsed_ms: 1 })),
            None
        );
        assert_eq!(report_outcome(None), None);
    }

    #[derive(Default)]
    struct FakeEnv {
        events: Mutex<Vec<PillEvent>>,
        created: Mutex<Vec<Value>>,
        updated: Mutex<Vec<String>>,
        pasted: Mutex<Vec<String>>,
        edits: Mutex<Vec<(String, String)>>,
        edit_result: Mutex<Option<Result<(), String>>>,
        accessibility: Mutex<u32>,
        paste_result: Mutex<Option<Result<bool, String>>>,
        recoveries: Mutex<VecDeque<Recovery>>,
        fetches: Mutex<u32>,
        uploads: Mutex<Vec<Vec<u8>>>,
        upload_result: Mutex<Option<Result<Value, String>>>,
        refine_result: Mutex<Option<Result<Value, String>>>,
        switch: Arc<CancelSwitch>,
        /// Escape is pressed while the capture is being refined.
        escape_while_refining: bool,
        discarded: Mutex<Vec<String>>,
    }

    impl TakeEnv for FakeEnv {
        fn emit(&self, event: PillEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn capture_created(&self, capture: &Value) {
            self.created.lock().unwrap().push(capture.clone());
        }
        fn capture_updated(&self, capture_id: &str) {
            self.updated.lock().unwrap().push(capture_id.to_string());
        }
        fn accessibility_missing(&self) {
            *self.accessibility.lock().unwrap() += 1;
        }
        fn paste(&self, text: String) -> impl Future<Output = Result<bool, String>> + Send {
            self.pasted.lock().unwrap().push(text);
            let result = self
                .paste_result
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(Ok(true));
            async move { result }
        }
        fn edit(
            &self,
            before: String,
            after: String,
        ) -> impl Future<Output = Result<(), String>> + Send {
            self.edits.lock().unwrap().push((before, after));
            let result = self.edit_result.lock().unwrap().clone().unwrap_or(Ok(()));
            async move { result }
        }
        fn fetch_result(&self, _session_id: String) -> impl Future<Output = Recovery> + Send {
            *self.fetches.lock().unwrap() += 1;
            let next = self
                .recoveries
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Recovery::Pending);
            async move { next }
        }
        fn upload(&self, wav: Vec<u8>) -> impl Future<Output = Result<Value, String>> + Send {
            self.uploads.lock().unwrap().push(wav);
            let result = self
                .upload_result
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err("no upload configured".into()));
            async move { result }
        }
        fn refine(
            &self,
            _capture_id: String,
        ) -> impl Future<Output = Result<Value, String>> + Send {
            if self.escape_while_refining {
                self.switch.cancel();
            }
            let result = self
                .refine_result
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err("no refine configured".into()));
            async move { result }
        }
        fn cancelled(&self) -> bool {
            self.switch.is_cancelled()
        }
        fn begin_delivery(&self) -> bool {
            self.switch.begin_delivery()
        }
        fn discard(&self, capture_id: String) -> impl Future<Output = ()> + Send {
            self.discarded.lock().unwrap().push(capture_id);
            async {}
        }
        fn recovery_delay(&self) -> Duration {
            Duration::ZERO
        }
    }

    impl FakeEnv {
        fn events(&self) -> Vec<PillEvent> {
            self.events.lock().unwrap().clone()
        }
        fn pasted(&self) -> Vec<String> {
            self.pasted.lock().unwrap().clone()
        }
    }

    fn final_event(text: &str) -> Value {
        json!({
            "type": "final",
            "refinement_complete": true,
            "refinement_error": null,
            "capture": {"id": "s1", "transcript_raw": text, "transcript_refined": text, "allow_auto_paste": true}
        })
    }

    fn recorded(seconds: f64) -> Recorded {
        Recorded {
            pcm: vec![1; (16_000.0 * seconds) as usize],
            sample_rate: 16_000,
        }
    }

    fn edit_event(edit: Value) -> Value {
        let mut event = final_event("Hi Morgan.");
        event["edit"] = edit;
        event
    }

    #[tokio::test]
    async fn a_voice_edit_changes_the_last_take_and_pastes_nothing() {
        let env = FakeEnv::default();
        let event = edit_event(json!({"before": "Hi Megan.", "after": "Hi Morgan."}));
        settle(&env, Outcome::Final(event), async { None }).await;
        assert!(env.pasted().is_empty());
        assert_eq!(
            *env.edits.lock().unwrap(),
            vec![("Hi Megan.".to_string(), "Hi Morgan.".to_string())]
        );
        assert_eq!(env.events(), vec![PillEvent::Done]);
        // The capture still shows in Captures.
        assert_eq!(env.created.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_voice_edit_that_changes_nothing_plays_the_error_cue() {
        let env = FakeEnv::default();
        *env.edit_result.lock().unwrap() = Some(Err("The text changed".into()));
        let event = edit_event(json!({"before": "Hi Megan.", "after": "Hi Morgan."}));
        settle(&env, Outcome::Final(event), async { None }).await;
        assert_eq!(env.events(), vec![PillEvent::error("The text changed")]);
        assert_eq!(env.events()[0].cue(), Some(Cue::Error));

        let env = FakeEnv::default();
        let event = edit_event(json!({"declined": "Nothing to fix"}));
        settle(&env, Outcome::Final(event), async { None }).await;
        assert!(env.pasted().is_empty() && env.edits.lock().unwrap().is_empty());
        assert_eq!(env.events(), vec![PillEvent::error("Nothing to fix")]);
    }

    #[tokio::test]
    async fn final_capture_is_announced_and_pasted() {
        let env = FakeEnv::default();
        settle(&env, Outcome::Final(final_event("Hello.")), async { None }).await;
        assert_eq!(env.pasted(), vec!["Hello."]);
        assert_eq!(env.created.lock().unwrap()[0]["id"], "s1");
        assert_eq!(env.events(), vec![PillEvent::Done]);
        assert!(env.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_final_output_finishes_without_pasting() {
        let env = FakeEnv::default();
        settle(&env, Outcome::Final(final_event("")), async { None }).await;
        assert!(env.pasted().is_empty());
        assert_eq!(env.events(), vec![PillEvent::Done]);
    }

    #[tokio::test]
    async fn accessibility_paste_failure_is_reported() {
        let env = FakeEnv::default();
        *env.paste_result.lock().unwrap() = Some(Err("Accessibility permission required".into()));
        settle(&env, Outcome::Final(final_event("Hi.")), async { None }).await;
        assert_eq!(*env.accessibility.lock().unwrap(), 1);
        assert_eq!(
            env.events(),
            vec![PillEvent::error(
                "Text saved in Captures. Accessibility permission required"
            )]
        );
    }

    #[tokio::test]
    async fn dropped_connection_after_finish_recovers_without_uploading() {
        let env = FakeEnv::default();
        env.recoveries.lock().unwrap().extend([
            Recovery::Pending,
            Recovery::Final(final_event("Recovered.")),
        ]);
        settle(
            &env,
            Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: None,
            },
            async { Some(recorded(2.0)) },
        )
        .await;
        assert_eq!(*env.fetches.lock().unwrap(), 2);
        assert_eq!(env.pasted(), vec!["Recovered."]);
        assert!(env.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn terminal_error_after_finish_is_shown_without_recovery_or_upload() {
        let env = FakeEnv::default();
        settle(
            &env,
            Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: Some("Recognition failed".into()),
            },
            async { Some(recorded(2.0)) },
        )
        .await;
        assert_eq!(*env.fetches.lock().unwrap(), 0);
        assert!(env.uploads.lock().unwrap().is_empty());
        assert_eq!(env.events(), vec![PillEvent::error("Recognition failed")]);
    }

    #[tokio::test]
    async fn unrecoverable_session_tells_the_user_to_check_captures() {
        let env = FakeEnv::default();
        settle(
            &env,
            Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: None,
            },
            async { None },
        )
        .await;
        assert_eq!(
            env.events(),
            vec![PillEvent::error(stream::INTERRUPTED_MESSAGE)]
        );
        assert!(env.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failure_before_finish_uploads_the_complete_recording() {
        let env = FakeEnv::default();
        *env.upload_result.lock().unwrap() = Some(Ok(json!({
            "id": "c1", "transcript_raw": "hello", "transcript_refined": null,
            "auto_refine": false, "allow_auto_paste": true
        })));
        settle(&env, Outcome::FailedBeforeFinish("closed".into()), async {
            Some(recorded(1.0))
        })
        .await;
        let uploads = env.uploads.lock().unwrap().clone();
        assert_eq!(uploads.len(), 1);
        let reader = hound::WavReader::new(std::io::Cursor::new(&uploads[0])).unwrap();
        assert_eq!(reader.len(), 16_000);
        assert_eq!(env.created.lock().unwrap()[0]["id"], "c1");
        assert_eq!(env.pasted(), vec!["hello"]);
        assert_eq!(env.events(), vec![PillEvent::Done]);
    }

    #[tokio::test]
    async fn batch_fallback_refines_when_the_server_asks() {
        let env = FakeEnv::default();
        *env.upload_result.lock().unwrap() = Some(Ok(json!({
            "id": "c1", "transcript_raw": "hello", "transcript_refined": null,
            "auto_refine": true, "allow_auto_paste": true
        })));
        *env.refine_result.lock().unwrap() = Some(Ok(json!({
            "id": "c1", "transcript_raw": "hello", "transcript_refined": "Hello."
        })));
        settle(&env, Outcome::FailedBeforeFinish("closed".into()), async {
            Some(recorded(1.0))
        })
        .await;
        assert_eq!(env.events(), vec![PillEvent::Refining, PillEvent::Done]);
        assert_eq!(*env.updated.lock().unwrap(), vec!["c1".to_string()]);
        assert_eq!(env.pasted(), vec!["Hello."]);
    }

    #[tokio::test]
    async fn batch_upload_errors_are_translated() {
        let env = FakeEnv::default();
        *env.upload_result.lock().unwrap() = Some(Err("Could not decode audio".into()));
        settle(&env, Outcome::FailedBeforeFinish("closed".into()), async {
            Some(recorded(1.0))
        })
        .await;
        assert_eq!(
            env.events(),
            vec![PillEvent::error(delivery::SHORT_RECORDING_MESSAGE)]
        );
    }

    #[tokio::test]
    async fn short_take_is_canceled_with_a_brief_notice() {
        let env = FakeEnv::default();
        settle(&env, Outcome::Cancelled, async { Some(recorded(0.2)) }).await;
        settle(&env, Outcome::FailedBeforeFinish("closed".into()), async {
            Some(recorded(0.2))
        })
        .await;
        assert!(env.uploads.lock().unwrap().is_empty());
        let brief = PillEvent::Error {
            message: delivery::SHORT_RECORDING_MESSAGE.into(),
            visible_ms: BRIEF_NOTICE_MS,
        };
        assert_eq!(env.events(), vec![brief.clone(), brief]);
    }

    #[tokio::test]
    async fn a_declined_command_says_why_once_recording_has_stopped() {
        let env = FakeEnv::default();
        settle(
            &env,
            Outcome::Declined("Select text to rewrite first".into()),
            async { Some(recorded(0.2)) },
        )
        .await;
        assert_eq!(
            env.events(),
            vec![PillEvent::notice("Select text to rewrite first")]
        );
        assert!(env.pasted().is_empty());
        assert!(env.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_take_without_audio_stays_silent() {
        let env = FakeEnv::default();
        settle(&env, Outcome::Cancelled, async { None }).await;
        settle(&env, Outcome::FailedBeforeFinish("x".into()), async {
            None
        })
        .await;
        assert!(env.events().is_empty());
    }

    #[tokio::test]
    async fn a_cancelled_take_says_nothing_even_when_short() {
        let env = FakeEnv::default();
        env.switch.cancel();
        settle(&env, Outcome::Cancelled, async { Some(recorded(0.2)) }).await;
        assert!(env.events().is_empty());
    }

    #[tokio::test]
    async fn escape_while_refining_inserts_nothing_and_discards_the_capture() {
        let env = FakeEnv {
            escape_while_refining: true,
            ..FakeEnv::default()
        };
        *env.upload_result.lock().unwrap() = Some(Ok(json!({
            "id": "c1", "transcript_raw": "hello", "transcript_refined": null,
            "auto_refine": true, "allow_auto_paste": true
        })));
        *env.refine_result.lock().unwrap() = Some(Ok(json!({
            "id": "c1", "transcript_raw": "hello", "transcript_refined": "Hello."
        })));
        settle(&env, Outcome::FailedBeforeFinish("closed".into()), async {
            Some(recorded(1.0))
        })
        .await;
        assert!(env.pasted().is_empty());
        assert_eq!(*env.discarded.lock().unwrap(), vec!["c1".to_string()]);
        assert!(!env.events().contains(&PillEvent::Done));
    }

    #[tokio::test]
    async fn a_final_arriving_after_escape_is_discarded_not_pasted() {
        let env = FakeEnv::default();
        env.switch.cancel();
        settle(&env, Outcome::Final(final_event("Hello.")), async { None }).await;
        assert!(env.pasted().is_empty());
        assert!(env.created.lock().unwrap().is_empty());
        assert_eq!(*env.discarded.lock().unwrap(), vec!["s1".to_string()]);
        assert!(env.events().is_empty());
    }

    #[tokio::test]
    async fn a_recovered_capture_is_discarded_after_escape() {
        let env = FakeEnv::default();
        env.switch.cancel();
        env.recoveries
            .lock()
            .unwrap()
            .push_back(Recovery::Final(final_event("Recovered.")));
        settle(
            &env,
            Outcome::FailedAfterFinish {
                session_id: "s1".into(),
                terminal_error: None,
            },
            async { None },
        )
        .await;
        assert!(env.pasted().is_empty());
        assert_eq!(*env.discarded.lock().unwrap(), vec!["s1".to_string()]);
    }

    #[tokio::test]
    async fn escape_before_the_batch_fallback_uploads_nothing() {
        let env = FakeEnv::default();
        env.switch.cancel();
        settle(&env, Outcome::FailedBeforeFinish("closed".into()), async {
            Some(recorded(1.0))
        })
        .await;
        assert!(env.uploads.lock().unwrap().is_empty());
        assert!(env.events().is_empty());
    }

    #[tokio::test]
    async fn escape_after_the_paste_began_changes_nothing() {
        let env = FakeEnv::default();
        settle(&env, Outcome::Final(final_event("Hello.")), async { None }).await;
        assert!(!env.switch.cancel());
        assert_eq!(env.pasted(), vec!["Hello."]);
        assert!(env.discarded.lock().unwrap().is_empty());
    }

    #[test]
    fn stopping_and_errors_play_cues_but_done_is_silent() {
        assert_eq!(
            PillEvent::Transcribing { elapsed_ms: 900 }.cue(),
            Some(Cue::Stop)
        );
        assert_eq!(PillEvent::error("boom").cue(), Some(Cue::Error));
        assert_eq!(PillEvent::Cancelled.cue(), Some(Cue::Cancel));
        assert_eq!(
            PillEvent::error(delivery::SHORT_RECORDING_MESSAGE).cue(),
            Some(Cue::Error)
        );
        for silent in [
            PillEvent::Preparing,
            PillEvent::Recording,
            PillEvent::Refining,
            PillEvent::Done,
        ] {
            assert_eq!(silent.cue(), None);
        }
    }

    #[test]
    fn pill_events_serialize_for_the_webview() {
        assert_eq!(
            serde_json::to_value(PillEvent::Transcribing { elapsed_ms: 1200 }).unwrap(),
            json!({"state": "transcribing", "elapsed_ms": 1200})
        );
        assert_eq!(
            serde_json::to_value(PillEvent::error("boom")).unwrap(),
            json!({"state": "error", "message": "boom", "visible_ms": ERROR_VISIBLE_MS})
        );
    }
}
