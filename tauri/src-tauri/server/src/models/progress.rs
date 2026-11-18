//! Downloads in flight and their progress: Python's `ProgressManager`
//! (`backend/utils/progress.py`) and `TaskManager` (`backend/utils/tasks.py`)
//! in one place. Progress is kept per model name (`migration` for moving the
//! models folder) and streamed to `/models/progress/{name}` listeners.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::broadcast;
use tokio::task::AbortHandle;

/// Listeners hear at most one update per this long, unless the progress
/// moved by `NOTIFY_DELTA` points or the download ended.
const NOTIFY_INTERVAL: Duration = Duration::from_millis(500);
const NOTIFY_DELTA: f64 = 1.0;

/// One model's latest progress, as `/models/progress/{name}` sends it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Update {
    pub model_name: String,
    pub current: u64,
    pub total: u64,
    /// 0 to 100.
    pub progress: f64,
    pub filename: Option<String>,
    pub status: String,
    /// Local time, as Python's `datetime.now().isoformat()`.
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Update {
    pub fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "downloading" | "extracting")
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.status.as_str(), "complete" | "error")
    }
}

/// A download the app started, in `/tasks/active` until it finishes.
/// One that failed stays, with its error, until it's dismissed.
#[derive(Clone, Debug)]
pub struct Task {
    pub status: String,
    /// UTC, as Python's `datetime.utcnow().isoformat()`.
    pub started_at: String,
    pub error: Option<String>,
    id: u64,
    running: Option<AbortHandle>,
}

#[derive(Default)]
struct Inner {
    progress: HashMap<String, Update>,
    notified: HashMap<String, (Instant, f64)>,
    tasks: BTreeMap<String, Task>,
    next_id: u64,
}

#[derive(Clone)]
pub struct Progress {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<Update>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            events: broadcast::channel(64).0,
        }
    }
}

fn now_local() -> String {
    chrono::Local::now()
        .naive_local()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

fn now_utc() -> String {
    chrono::Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

impl Progress {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record progress and tell listeners, at most every half second or
    /// percentage point unless the status is final.
    pub fn update(
        &self,
        name: &str,
        current: u64,
        total: u64,
        filename: Option<&str>,
        status: &str,
    ) {
        let progress = if total > 0 {
            (current as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let update = Update {
            model_name: name.to_string(),
            current,
            total,
            progress,
            filename: filename.map(str::to_string),
            status: status.to_string(),
            timestamp: now_local(),
            error: None,
        };
        let mut inner = self.lock();
        inner.progress.insert(name.to_string(), update.clone());
        let now = Instant::now();
        let notify = match inner.notified.get(name) {
            None => true,
            Some((at, last)) => {
                update.is_finished()
                    || now.duration_since(*at) >= NOTIFY_INTERVAL
                    || (progress - last).abs() >= NOTIFY_DELTA
            }
        };
        if notify {
            inner.notified.insert(name.to_string(), (now, progress));
            drop(inner);
            let _ = self.events.send(update);
        }
    }

    pub fn mark_complete(&self, name: &str) {
        let mut inner = self.lock();
        let Some(update) = inner.progress.get_mut(name) else {
            return;
        };
        update.status = "complete".into();
        update.progress = 100.0;
        let update = update.clone();
        drop(inner);
        let _ = self.events.send(update);
    }

    pub fn mark_error(&self, name: &str, error: &str) {
        let mut inner = self.lock();
        let update = inner
            .progress
            .entry(name.to_string())
            .or_insert_with(|| Update {
                model_name: name.to_string(),
                current: 0,
                total: 0,
                progress: 0.0,
                filename: None,
                status: String::new(),
                timestamp: now_local(),
                error: None,
            });
        update.status = "error".into();
        update.error = Some(error.to_string());
        let update = update.clone();
        drop(inner);
        let _ = self.events.send(update);
    }

    pub fn get(&self, name: &str) -> Option<Update> {
        self.lock().progress.get(name).cloned()
    }

    /// Progress still downloading or extracting.
    pub fn all_active(&self) -> Vec<Update> {
        self.lock()
            .progress
            .values()
            .filter(|update| update.is_active())
            .cloned()
            .collect()
    }

    /// Forget a model's progress; true when there was some.
    pub fn forget(&self, name: &str) -> bool {
        let mut inner = self.lock();
        inner.notified.remove(name);
        inner.progress.remove(name).is_some()
    }

    /// Updates as they're sent, for every model.
    pub fn listen(&self) -> broadcast::Receiver<Update> {
        self.events.subscribe()
    }

    // -- tasks --------------------------------------------------------------

    /// Start tracking a download; the returned id finishes it.
    pub fn start_task(&self, name: &str) -> u64 {
        let mut inner = self.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        if let Some(previous) = inner.tasks.get(name).and_then(|task| task.running.clone()) {
            previous.abort();
        }
        inner.tasks.insert(
            name.to_string(),
            Task {
                status: "downloading".into(),
                started_at: now_utc(),
                error: None,
                id,
                running: None,
            },
        );
        id
    }

    /// The work behind task `id`, stopped if the task is cancelled.
    pub fn attach(&self, name: &str, id: u64, running: AbortHandle) {
        let mut inner = self.lock();
        match inner.tasks.get_mut(name) {
            Some(task) if task.id == id => task.running = Some(running),
            _ => running.abort(),
        }
    }

    pub fn complete_task(&self, name: &str, id: u64) {
        let mut inner = self.lock();
        if inner.tasks.get(name).is_some_and(|task| task.id == id) {
            inner.tasks.remove(name);
        }
    }

    pub fn fail_task(&self, name: &str, id: u64, error: &str) {
        let mut inner = self.lock();
        if let Some(task) = inner.tasks.get_mut(name).filter(|task| task.id == id) {
            task.status = "error".into();
            task.error = Some(error.to_string());
            task.running = None;
        }
    }

    /// Whether task `id` is still the model's current download.
    pub fn is_current(&self, name: &str, id: u64) -> bool {
        self.lock()
            .tasks
            .get(name)
            .is_some_and(|task| task.id == id)
    }

    /// Stop and dismiss a model's download; true when there was one.
    pub fn cancel_task(&self, name: &str) -> bool {
        let removed = self.lock().tasks.remove(name);
        if let Some(running) = removed.as_ref().and_then(|task| task.running.as_ref()) {
            running.abort();
        }
        removed.is_some()
    }

    /// Downloads still running: a failed one isn't.
    pub fn pending(&self) -> Vec<String> {
        self.lock()
            .tasks
            .iter()
            .filter(|(_, task)| matches!(task.status.as_str(), "downloading" | "extracting"))
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.pending().iter().any(|pending| pending == name)
    }

    pub fn tasks(&self) -> Vec<(String, Task)> {
        self.lock()
            .tasks
            .iter()
            .map(|(name, task)| (name.clone(), task.clone()))
            .collect()
    }

    /// Stop every download and forget all progress.
    pub fn clear(&self) {
        let mut inner = self.lock();
        for task in inner.tasks.values() {
            if let Some(running) = &task.running {
                running.abort();
            }
        }
        inner.tasks.clear();
        inner.progress.clear();
        inner.notified.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_download_stays_listed_but_is_not_pending() {
        let progress = Progress::default();
        let id = progress.start_task("whisper-turbo");
        assert_eq!(progress.pending(), ["whisper-turbo"]);
        progress.fail_task("whisper-turbo", id, "boom");
        assert!(progress.pending().is_empty());
        let tasks = progress.tasks();
        assert_eq!(tasks[0].1.status, "error");
        assert_eq!(tasks[0].1.error.as_deref(), Some("boom"));
        assert!(progress.cancel_task("whisper-turbo"));
        assert!(progress.tasks().is_empty());
    }

    #[test]
    fn a_retried_download_is_not_finished_by_the_old_one() {
        let progress = Progress::default();
        let old = progress.start_task("qwen3-4b");
        let new = progress.start_task("qwen3-4b");
        progress.complete_task("qwen3-4b", old);
        assert_eq!(progress.pending(), ["qwen3-4b"]);
        progress.complete_task("qwen3-4b", new);
        assert!(progress.tasks().is_empty());
    }

    #[test]
    fn updates_are_throttled_but_the_end_always_gets_through() {
        let progress = Progress::default();
        let mut events = progress.listen();
        progress.update("m", 0, 1000, Some("a"), "downloading");
        progress.update("m", 1, 1000, Some("a"), "downloading");
        progress.update("m", 50, 1000, Some("a"), "downloading");
        progress.mark_complete("m");
        let seen: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|update| (update.current, update.status))
            .collect();
        assert_eq!(
            seen,
            [
                (0, "downloading".to_string()),
                (50, "downloading".to_string()),
                (50, "complete".to_string())
            ]
        );
        assert_eq!(progress.get("m").unwrap().progress, 100.0);
    }

    #[test]
    fn an_error_without_progress_is_recorded() {
        let progress = Progress::default();
        progress.mark_error("migration", "disk full");
        let update = progress.get("migration").unwrap();
        assert_eq!(update.status, "error");
        assert_eq!(update.error.as_deref(), Some("disk full"));
        assert!(progress.all_active().is_empty());
    }
}
