//! When to keep the dictation models in RAM, decided here; the worker reads
//! back the pages (`/worker/keep-resident`, `backend/services/model_warmth.py`).
//!
//! After a while unused, macOS swaps or compresses the Whisper and Qwen
//! weights, and the next dictation waits on reading them back. The keeper
//! follows the user: while they are at the Mac (input in the last couple of
//! minutes) it asks the worker every minute to read back whatever was taken.
//! Once they have been away a while it stops, and the models may go cold. The
//! first input after that brings them back, a few seconds before the user can
//! start a dictation.
//!
//! It never loads or unloads a model, and it skips its checks while a dictation runs:
//! the dictation asked for its own read-back when it started
//! (`routes::capture_stream`), and a periodic one then would only compete
//! with it for the disk.

use std::time::{Duration, Instant};

use crate::proxy::Proxy;
use crate::stream::models;
use crate::AppState;

/// How often the user's idle time is read.
const POLL: Duration = Duration::from_secs(5);
/// Input this recent means the user is at the Mac.
const AT_THE_MAC: f64 = 120.0;
/// No input for this long and the models may go cold.
const AWAY: f64 = 600.0;
/// How often the models are checked while the user is here.
const CHECK_EVERY: f64 = 60.0;

/// When to check the models: often while the user is here, never while away.
#[derive(Debug, Default)]
pub struct Keeper {
    away: bool,
    last_check: Option<f64>,
}

impl Keeper {
    /// The reason to check now, or None. `idle` is the seconds since the
    /// last input, `now` any monotonic clock in seconds.
    pub fn check(&mut self, idle: f64, now: f64) -> Option<&'static str> {
        if idle >= AWAY {
            self.away = true;
            return None;
        }
        if idle >= AT_THE_MAC {
            return None;
        }
        if self.away {
            self.away = false;
            self.last_check = Some(now);
            return Some("you're back");
        }
        if self.last_check.is_none_or(|last| now - last >= CHECK_EVERY) {
            self.last_check = Some(now);
            return Some("while you're here");
        }
        None
    }
}

/// Seconds since the last keystroke, click or mouse movement; None off macOS.
///
/// Needs no permission: it reads when the last event happened, not what it was.
#[cfg(target_os = "macos")]
pub fn seconds_since_input() -> Option<f64> {
    /// kCGEventSourceStateHIDSystemState
    const HID_SYSTEM_STATE: i32 = 1;
    /// kCGAnyInputEventType
    const ANY_INPUT_EVENT: u32 = u32::MAX;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    // SAFETY: a pure query of the window server's event times.
    Some(unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) })
}

#[cfg(not(target_os = "macos"))]
pub fn seconds_since_input() -> Option<f64> {
    None
}

/// Keep the models warm while the server runs. The worker reads back only
/// the models it has loaded: a stand-in for a shared Kass leaves them
/// unloaded until the app falls back to it, so until then this asks for
/// nothing.
pub async fn watch(state: AppState) {
    let streams = state.streams.clone();
    keep(
        &state.proxy,
        seconds_since_input,
        move || streams.dictating(),
        POLL,
    )
    .await;
}

/// Read `idle` every `poll` and ask the worker to read the models back when
/// the keeper says to, except while `dictating`.
async fn keep(
    proxy: &Proxy,
    idle: impl Fn() -> Option<f64>,
    dictating: impl Fn() -> bool,
    poll: Duration,
) {
    if idle().is_none() {
        return;
    }
    let started = Instant::now();
    let mut keeper = Keeper::default();
    loop {
        if !dictating() {
            let reason =
                idle().and_then(|idle| keeper.check(idle, started.elapsed().as_secs_f64()));
            if let Some(reason) = reason {
                if let Err(error) = models::keep_resident(proxy, reason, true, true).await {
                    eprintln!("[warmth] couldn't check whether the models are in RAM: {error}");
                }
            }
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn the_keeper_checks_while_the_user_is_here_and_not_while_away() {
        let mut keeper = Keeper::default();
        assert_eq!(keeper.check(1., 0.), Some("while you're here"));
        assert_eq!(keeper.check(1., 30.), None, "checked a minute apart");
        assert_eq!(keeper.check(1., 61.), Some("while you're here"));
        // Input stops: idle but not yet away, then away. Nothing is checked.
        assert_eq!(keeper.check(300., 400.), None);
        assert_eq!(keeper.check(900., 1000.), None);
        assert_eq!(keeper.check(3600., 3700.), None);
        // The first input after being away brings the models back at once.
        assert_eq!(keeper.check(2., 3705.), Some("you're back"));
        assert_eq!(keeper.check(2., 3710.), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn input_idle_time_reads_without_permission() {
        let idle = seconds_since_input().unwrap();
        assert!(idle >= 0., "{idle}");
    }

    async fn worker(calls: Arc<Mutex<Vec<Value>>>) -> SocketAddr {
        async fn keep_resident(
            State(calls): State<Arc<Mutex<Vec<Value>>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            calls.lock().unwrap().push(body);
            Json(json!({"ok": true}))
        }
        let app = Router::new()
            .route("/worker/keep-resident", post(keep_resident))
            .with_state(calls);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        address
    }

    #[tokio::test]
    async fn the_worker_is_asked_to_read_both_models_back_but_not_mid_dictation() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let proxy = Proxy::new(worker(calls.clone()).await);
        let dictating = Arc::new(AtomicBool::new(true));
        let during = dictating.clone();
        let keeping = tokio::spawn(async move {
            keep(
                &proxy,
                || Some(1.),
                move || during.load(Ordering::SeqCst),
                Duration::from_millis(10),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(calls.lock().unwrap().is_empty(), "checked mid-dictation");

        dictating.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        keeping.abort();
        assert_eq!(
            *calls.lock().unwrap(),
            [json!({"reason": "while you're here", "llm": true, "stt": true})],
            "once, then a minute apart"
        );
    }

    #[tokio::test]
    async fn without_an_idle_time_nothing_is_kept() {
        let proxy = Proxy::new("127.0.0.1:9".parse().unwrap());
        let kept = tokio::time::timeout(
            Duration::from_secs(1),
            keep(&proxy, || None, || false, Duration::from_millis(10)),
        )
        .await;
        assert!(kept.is_ok(), "returned at once");
    }
}
