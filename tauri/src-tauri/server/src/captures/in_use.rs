//! Captures being refined or retranscribed, which the history sweep must
//! leave alone until they're saved (`history_retention.in_use`).
//!
//! The sweep still runs in the worker, so each hold is passed on to it as
//! well; the registry here is for the sweep once it runs in this server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::proxy::Proxy;
use crate::worker;

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

/// Hold `capture_id` here and in the worker until the guard drops, however
/// the request ends.
pub async fn hold(in_use: &Arc<InUse>, proxy: &Proxy, capture_id: &str) -> Held {
    in_use.add(capture_id);
    // Made first, so a request dropped while the worker answers lets go here.
    let mut held = Held {
        in_use: in_use.clone(),
        proxy: proxy.clone(),
        capture_id: capture_id.to_string(),
        in_worker: false,
    };
    held.in_worker = worker::hold_capture(proxy, capture_id).await;
    held
}

pub struct Held {
    in_use: Arc<InUse>,
    proxy: Proxy,
    capture_id: String,
    /// Whether the worker took the hold, so has one to release.
    in_worker: bool,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.in_use.remove(&self.capture_id);
        if !self.in_worker {
            return;
        }
        let (proxy, capture_id) = (self.proxy.clone(), std::mem::take(&mut self.capture_id));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { worker::release_capture(&proxy, &capture_id).await });
        }
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
}
