//! Cleaned text shown in the target app while it is still being generated
//! (docs/plans/STREAMING_INSERTION.md).
//!
//! After release the server sends `provisional` events: the part of the
//! final text it expects to keep. [`Live`] writes them into the focused field
//! through Accessibility, only where the field can be read back, and makes the
//! field match the `final` text exactly when it arrives. Where live text
//! never started (terminals, clipboard-only apps, unreadable fields), the final
//! text takes today's paste path unchanged.
//!
//! The field's side effects sit behind [`LiveTarget`], so every rule here runs
//! against fakes.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::text_insert::{LiveError, LiveStart, Owned};

pub const EDITED_MESSAGE: &str =
    "The field changed while Kass was typing, so the rest was not inserted.";

/// Least time between live writes after the first. Tokens arrive every
/// ~15 ms; rewriting the field that often adds undo steps and flicker for
/// no visible gain. The final text never waits for it.
pub const WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The focused field of the dictation's target app.
pub trait LiveTarget: Send + Sync + 'static {
    /// Whether live text may be tried at all (permission, target frontmost).
    fn eligible(&self) -> bool;
    fn begin(&self, text: String) -> impl Future<Output = LiveStart> + Send;
    fn extend(
        &self,
        owned: Owned,
        text: String,
    ) -> impl Future<Output = Result<Owned, LiveError>> + Send;
    fn finish(
        &self,
        owned: Owned,
        text: String,
    ) -> impl Future<Output = Result<(), LiveError>> + Send;
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    /// Nothing written yet.
    Idle,
    Active(Owned),
    /// The app stopped applying writes. Nothing more is offered, but the
    /// final text is still written over the owned text.
    Stalled(Owned),
    /// Never started; nothing was written.
    Declined,
    /// Something may be in the field that can't be tracked.
    Broken(String),
    Closed,
}

/// How the final text was delivered.
#[derive(Debug, Clone, PartialEq)]
pub enum Finish {
    /// No live text was written: deliver the final text the usual way.
    NotStarted,
    /// The live text was made final (or removed). Same meaning as a paste
    /// result: `Ok(true)` when the text is in place.
    Done(Result<bool, String>),
}

pub struct Live<T: LiveTarget> {
    target: T,
    state: tokio::sync::Mutex<State>,
    latest: Mutex<Option<String>>,
    working: AtomicBool,
    closed: AtomicBool,
}

impl<T: LiveTarget> Live<T> {
    pub fn new(target: T) -> Arc<Self> {
        Arc::new(Self {
            target,
            state: tokio::sync::Mutex::new(State::Idle),
            latest: Mutex::new(None),
            working: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    /// New provisional text. Never blocks: a worker applies the latest one,
    /// skipping any that arrive while a write is in flight.
    pub fn offer(self: &Arc<Self>, text: String) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(text);
        }
        if !self.working.swap(true, Ordering::AcqRel) {
            let live = self.clone();
            tokio::spawn(async move { live.work().await });
        }
    }

    async fn work(&self) {
        let mut last_write: Option<tokio::time::Instant> = None;
        loop {
            if let Some(at) = last_write {
                // Not holding the state lock, so the final text never waits.
                tokio::time::sleep_until(at + WRITE_INTERVAL).await;
            }
            let next = self.latest.lock().ok().and_then(|mut l| l.take());
            let Some(text) = next else {
                self.working.store(false, Ordering::Release);
                // An offer may have landed between the take and the store.
                let pending = self.latest.lock().map(|l| l.is_some()).unwrap_or(false);
                if pending && !self.working.swap(true, Ordering::AcqRel) {
                    continue;
                }
                return;
            };
            let mut state = self.state.lock().await;
            if self.closed.load(Ordering::Acquire) {
                continue;
            }
            let previous = state.clone();
            *state = self.apply(previous.clone(), text).await;
            if *state != previous {
                last_write = Some(tokio::time::Instant::now());
            }
        }
    }

    async fn apply(&self, state: State, text: String) -> State {
        match state {
            State::Idle if !self.target.eligible() => State::Declined,
            State::Idle => match self.target.begin(text).await {
                LiveStart::Started(owned) => State::Active(owned),
                LiveStart::Declined(_) => State::Declined,
                LiveStart::Broken(message) => State::Broken(message),
            },
            State::Active(owned) if owned.grows_to(&text) => {
                match self.target.extend(owned.clone(), text).await {
                    Ok(grown) => State::Active(grown),
                    Err(LiveError::NotApplied) => State::Stalled(owned),
                    Err(LiveError::Edited) => State::Broken(EDITED_MESSAGE.into()),
                    Err(LiveError::Uncertain(message)) => State::Broken(message),
                }
            }
            // A revision waits for the final text.
            other => other,
        }
    }

    /// Deliver the final text (`None` withdraws any live text: the take ended
    /// without a paste). Waits for a write in flight, then ignores later offers.
    pub async fn finish(&self, text: Option<String>) -> Finish {
        self.closed.store(true, Ordering::Release);
        let mut state = self.state.lock().await;
        let previous = std::mem::replace(&mut *state, State::Closed);
        match previous {
            State::Idle | State::Declined | State::Closed => Finish::NotStarted,
            State::Broken(message) => Finish::Done(Err(message)),
            State::Active(owned) | State::Stalled(owned) => {
                let result = self
                    .target
                    .finish(owned, text.unwrap_or_default())
                    .await
                    .map(|()| true)
                    .map_err(|error| match error {
                        LiveError::Edited => EDITED_MESSAGE.to_string(),
                        LiveError::NotApplied => {
                            "The app stopped accepting the dictated text.".to_string()
                        }
                        LiveError::Uncertain(message) => message,
                    });
                Finish::Done(result)
            }
        }
    }

    #[cfg(test)]
    async fn settled(&self) {
        while self.working.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Begin(String),
        Extend(String, String),
        Finish(String, String),
    }

    #[derive(Default)]
    struct Fake {
        ineligible: bool,
        calls: Mutex<Vec<Call>>,
        begins: Mutex<VecDeque<LiveStart>>,
        extends: Mutex<VecDeque<Result<Owned, LiveError>>>,
        finishes: Mutex<VecDeque<Result<(), LiveError>>>,
    }

    impl LiveTarget for Fake {
        fn eligible(&self) -> bool {
            !self.ineligible
        }
        fn begin(&self, text: String) -> impl Future<Output = LiveStart> + Send {
            self.calls.lock().unwrap().push(Call::Begin(text.clone()));
            let out = self
                .begins
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(LiveStart::Started(Owned {
                    start: 0,
                    text,
                    join: Default::default(),
                }));
            async move { out }
        }
        fn extend(
            &self,
            owned: Owned,
            text: String,
        ) -> impl Future<Output = Result<Owned, LiveError>> + Send {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Extend(owned.text.clone(), text.clone()));
            let out = self
                .extends
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(Owned {
                    start: owned.start,
                    text,
                    join: owned.join.clone(),
                }));
            async move { out }
        }
        fn finish(
            &self,
            owned: Owned,
            text: String,
        ) -> impl Future<Output = Result<(), LiveError>> + Send {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Finish(owned.text, text));
            let out = self.finishes.lock().unwrap().pop_front().unwrap_or(Ok(()));
            async move { out }
        }
    }

    fn calls(live: &Live<Fake>) -> Vec<Call> {
        live.target.calls.lock().unwrap().clone()
    }

    async fn offer(live: &Arc<Live<Fake>>, text: &str) {
        live.offer(text.to_string());
        live.settled().await;
    }

    #[tokio::test]
    async fn live_text_grows_then_becomes_the_final_text() {
        let live = Live::new(Fake::default());
        offer(&live, "Hello").await;
        offer(&live, "Hello there").await;
        let finish = live.finish(Some("Hello there, friend.".into())).await;
        assert_eq!(finish, Finish::Done(Ok(true)));
        assert_eq!(
            calls(&live),
            vec![
                Call::Begin("Hello".into()),
                Call::Extend("Hello".into(), "Hello there".into()),
                Call::Finish("Hello there".into(), "Hello there, friend.".into()),
            ]
        );
    }

    #[tokio::test]
    async fn without_provisional_text_the_final_takes_the_usual_path() {
        let live = Live::new(Fake::default());
        assert_eq!(live.finish(Some("Hi.".into())).await, Finish::NotStarted);
        assert!(calls(&live).is_empty());
    }

    #[tokio::test]
    async fn ineligible_targets_are_never_touched() {
        let live = Live::new(Fake {
            ineligible: true,
            ..Fake::default()
        });
        offer(&live, "Hello").await;
        offer(&live, "Hello there").await;
        assert_eq!(
            live.finish(Some("Hello there.".into())).await,
            Finish::NotStarted
        );
        assert!(calls(&live).is_empty());
    }

    #[tokio::test]
    async fn a_declined_start_leaves_the_final_to_the_usual_path() {
        let fake = Fake::default();
        fake.begins.lock().unwrap().push_back(LiveStart::Declined(
            crate::text_insert::FallbackReason::ClipboardOnlyApp,
        ));
        let live = Live::new(fake);
        offer(&live, "ls").await;
        offer(&live, "ls -la").await;
        assert_eq!(live.finish(Some("ls -la".into())).await, Finish::NotStarted);
        assert_eq!(calls(&live), vec![Call::Begin("ls".into())]);
    }

    #[tokio::test]
    async fn text_that_does_not_extend_what_is_shown_waits_for_the_final() {
        let live = Live::new(Fake::default());
        offer(&live, "Send the").await;
        offer(&live, "Do not send").await;
        live.finish(Some("Do not send the update.".into())).await;
        assert_eq!(
            calls(&live),
            vec![
                Call::Begin("Send the".into()),
                Call::Finish("Send the".into(), "Do not send the update.".into()),
            ]
        );
    }

    #[tokio::test]
    async fn a_take_without_a_paste_withdraws_its_live_text() {
        let live = Live::new(Fake::default());
        offer(&live, "Um").await;
        assert_eq!(live.finish(None).await, Finish::Done(Ok(true)));
        assert_eq!(
            calls(&live).last(),
            Some(&Call::Finish("Um".into(), "".into()))
        );
    }

    #[tokio::test]
    async fn untrackable_text_is_an_error_and_never_pasted_again() {
        let fake = Fake::default();
        fake.begins
            .lock()
            .unwrap()
            .push_back(LiveStart::Broken("landed differently".into()));
        let live = Live::new(fake);
        offer(&live, "Hello").await;
        offer(&live, "Hello there").await;
        assert_eq!(
            live.finish(Some("Hello there.".into())).await,
            Finish::Done(Err("landed differently".into()))
        );
        assert_eq!(calls(&live), vec![Call::Begin("Hello".into())]);
    }

    #[tokio::test]
    async fn a_user_edit_stops_live_text_and_is_reported() {
        let fake = Fake::default();
        fake.extends
            .lock()
            .unwrap()
            .push_back(Err(LiveError::Edited));
        let live = Live::new(fake);
        offer(&live, "Hello").await;
        offer(&live, "Hello there").await;
        offer(&live, "Hello there my").await;
        assert_eq!(
            live.finish(Some("Hello there my friend.".into())).await,
            Finish::Done(Err(EDITED_MESSAGE.into()))
        );
        assert_eq!(calls(&live).len(), 2);
    }

    #[tokio::test]
    async fn an_app_that_stops_applying_still_gets_the_final_text() {
        let fake = Fake::default();
        fake.extends
            .lock()
            .unwrap()
            .push_back(Err(LiveError::NotApplied));
        let live = Live::new(fake);
        offer(&live, "Hello").await;
        offer(&live, "Hello there").await;
        offer(&live, "Hello there my").await;
        assert_eq!(
            live.finish(Some("Hello there my friend.".into())).await,
            Finish::Done(Ok(true))
        );
        assert_eq!(
            calls(&live).last(),
            Some(&Call::Finish(
                "Hello".into(),
                "Hello there my friend.".into()
            ))
        );
        assert_eq!(calls(&live).len(), 3);
    }

    #[tokio::test]
    async fn writes_are_spaced_so_the_field_is_not_rewritten_every_token() {
        let live = Live::new(Fake::default());
        let started = tokio::time::Instant::now();
        offer(&live, "One").await;
        offer(&live, "One two").await;
        offer(&live, "One two three").await;
        // The first write is immediate; later ones wait out the interval.
        assert!(started.elapsed() >= 2 * WRITE_INTERVAL);
        // The final text never waits for the interval.
        let before = tokio::time::Instant::now();
        live.offer("One two three four".into());
        live.finish(Some("One two three four.".into())).await;
        assert!(before.elapsed() < WRITE_INTERVAL);
    }

    #[tokio::test]
    async fn offers_after_the_final_are_ignored() {
        let live = Live::new(Fake::default());
        offer(&live, "Hi").await;
        live.finish(Some("Hi.".into())).await;
        offer(&live, "Hi there").await;
        assert_eq!(calls(&live).len(), 2);
    }

    #[tokio::test]
    async fn the_final_waits_for_a_write_in_flight() {
        let live = Live::new(Fake::default());
        live.offer("Hello".into());
        // No settling: finish must still see the begin and finish over it.
        let finish = live.finish(Some("Hello.".into())).await;
        let calls = calls(&live);
        match calls.as_slice() {
            [] => assert_eq!(finish, Finish::NotStarted),
            [Call::Begin(_), Call::Finish(_, text)] => assert_eq!(text, "Hello."),
            other => panic!("unexpected {other:?}"),
        }
    }
}
