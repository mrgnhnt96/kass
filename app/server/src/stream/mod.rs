//! The `/captures/stream` dictation session, served here: the worker only
//! recognizes and generates.
//!
//! - `speech`: Silero voice detection over the incoming audio.
//! - `intake`: the start message, PCM frames, the recording, pause cuts and
//!   the phrase windows sent to recognition.
//! - `models`: the worker's Whisper and Qwen3 calls the session makes.
//! - `prosody`: how the dictation was said: "!" and drawn-out words written
//!   into its text, and its measurements saved to learn from edits.
//! - `session`: one dictation, from its start message to its saved capture.

pub mod intake;
pub mod models;
pub mod prosody;
pub mod session;
pub mod speech;

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Dictations streaming at once.
pub const MAX_SESSIONS: usize = 2;
/// Finished sessions' results kept for recovery.
const KEPT_RESULTS: usize = 32;
/// How long a result is kept.
const RESULT_LIFETIME: Duration = Duration::from_secs(600);

/// The sessions streaming now and the latest results, for a client that
/// lost its connection after release.
#[derive(Default)]
pub struct Streams {
    results: Mutex<VecDeque<(String, Instant, Value)>>,
    running: Mutex<HashSet<String>>,
    active: AtomicUsize,
}

/// A session's result, while it's running, or neither.
pub enum Outcome {
    Done(Value),
    Pending,
    Unknown,
}

impl Streams {
    /// Take a place for a new connection; None when the sessions are full.
    pub fn enter(self: &Arc<Self>) -> Option<Place> {
        let taken = self
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                (active < MAX_SESSIONS).then_some(active + 1)
            });
        taken.ok().map(|_| Place {
            streams: self.clone(),
        })
    }

    pub fn running(&self, id: &str) {
        lock(&self.running).insert(id.to_string());
    }

    pub fn stopped(&self, id: &str) {
        lock(&self.running).remove(id);
    }

    /// Whether a dictation is running now.
    pub fn dictating(&self) -> bool {
        !lock(&self.running).is_empty()
    }

    pub fn keep(&self, id: &str, result: Value) {
        let mut results = lock(&self.results);
        results.retain(|(kept, _, _)| kept != id);
        results.push_back((id.to_string(), Instant::now(), result));
        while results.len() > KEPT_RESULTS {
            results.pop_front();
        }
    }

    pub fn outcome(&self, id: &str) -> Outcome {
        let mut results = lock(&self.results);
        results.retain(|(_, kept, _)| kept.elapsed() <= RESULT_LIFETIME);
        if let Some((_, _, result)) = results.iter().find(|(kept, _, _)| kept == id) {
            return Outcome::Done(result.clone());
        }
        if lock(&self.running).contains(id) {
            return Outcome::Pending;
        }
        Outcome::Unknown
    }
}

/// A connection's place among the sessions, given back when dropped.
pub struct Place {
    streams: Arc<Streams>,
}

impl Drop for Place {
    fn drop(&mut self) {
        self.streams.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_two_sessions_and_their_results() {
        let streams = Arc::new(Streams::default());
        let first = streams.enter().unwrap();
        let _second = streams.enter().unwrap();
        assert!(streams.enter().is_none());
        drop(first);
        assert!(streams.enter().is_some());

        assert!(matches!(streams.outcome("a"), Outcome::Unknown));
        streams.running("a");
        assert!(matches!(streams.outcome("a"), Outcome::Pending));
        streams.keep("a", json!({"type": "final"}));
        streams.stopped("a");
        assert!(matches!(streams.outcome("a"), Outcome::Done(v) if v["type"] == "final"));
        for n in 0..KEPT_RESULTS {
            streams.keep(&n.to_string(), json!({}));
        }
        assert!(matches!(streams.outcome("a"), Outcome::Unknown));
    }
}
