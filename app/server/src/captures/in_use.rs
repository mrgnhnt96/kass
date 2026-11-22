//! Captures being refined or retranscribed, which the history sweep must
//! leave alone until they're saved (`retention`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How many holds each busy capture has.
#[derive(Debug, Default)]
pub struct InUse(Mutex<HashMap<String, usize>>);

impl InUse {
    /// Whether the history sweep must skip `capture_id` now.
    pub fn busy(&self, capture_id: &str) -> bool {
        self.lock().contains_key(capture_id)
    }

    /// The captures the history sweep must skip now.
    pub fn all(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// Run `work` with the captures held now, keeping new holds waiting
    /// until it's done: a capture the sweep is deleting can't be taken for
    /// refining meanwhile.
    pub fn holding<T>(&self, work: impl FnOnce(Vec<String>) -> T) -> T {
        let busy = self.lock();
        work(busy.keys().cloned().collect())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn add(&self, capture_id: &str) {
        *self.lock().entry(capture_id.to_string()).or_default() += 1;
    }

    fn remove(&self, capture_id: &str) {
        let mut busy = self.lock();
        if let Some(count) = busy.get_mut(capture_id) {
            *count -= 1;
            if *count == 0 {
                busy.remove(capture_id);
            }
        }
    }
}

/// Hold `capture_id` until the guard drops, however the request ends.
pub fn hold(in_use: &Arc<InUse>, capture_id: &str) -> Held {
    in_use.add(capture_id);
    Held {
        in_use: in_use.clone(),
        capture_id: capture_id.to_string(),
    }
}

pub struct Held {
    in_use: Arc<InUse>,
    capture_id: String,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.in_use.remove(&self.capture_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_held_twice_is_busy_until_both_let_go() {
        let in_use = InUse::default();
        in_use.add("a");
        in_use.add("a");
        in_use.remove("a");
        assert!(in_use.busy("a"));
        in_use.remove("a");
        assert!(!in_use.busy("a"));
        in_use.remove("a");
        assert!(in_use.all().is_empty());
    }

    #[test]
    fn a_hold_lets_go_when_dropped() {
        let in_use = Arc::new(InUse::default());
        let first = hold(&in_use, "a");
        let second = hold(&in_use, "a");
        assert_eq!(in_use.holding(|busy| busy), ["a"]);
        drop(first);
        assert!(in_use.busy("a"));
        drop(second);
        assert!(!in_use.busy("a"));
    }
}
