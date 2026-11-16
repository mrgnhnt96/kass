//! Downloading the dictation models during onboarding: speech first, then
//! cleanup. Once started, a model that is neither ready nor downloading is
//! requested again, so downloads stopped by a quit (Input Monitoring forces
//! one) pick up after the relaunch. A failed download is retried by itself
//! a few times before the user is asked.

use crate::flow::{missing_disk_mb, should_retry_automatically};
use kass_api::types::{ActiveDownloadTask, ModelReadiness};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelState {
    Waiting,
    Next,
    Downloading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelProgress {
    pub model: Option<ModelReadiness>,
    pub state: ModelState,
    /// 0 to 100.
    pub percent: f64,
    /// Why the last attempt failed, as the server said it.
    pub error: Option<String>,
}

/// What the page should ask the server for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start downloading this model now.
    Request(String),
    /// Cancel it and request it again after the retry delay.
    RetryLater(String),
}

/// Everything the page shows about the downloads.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub speech: ModelProgress,
    pub cleanup: Option<ModelProgress>,
    /// Speech, and cleanup when it's on: dictation can run.
    pub ready: bool,
    /// Retries are used up; the user decides what's next.
    pub failed: Option<ModelProgress>,
    /// Whether the failed one is speech.
    pub failed_is_speech: bool,
    /// MB to free before the downloads fit, or 0.
    pub disk_short_mb: f64,
    /// Share of both downloads done, 0 to 100, for the line along the top.
    pub overall: f64,
}

/// A task's progress as a whole percent, when it says.
pub fn progress_percent(task: Option<&ActiveDownloadTask>) -> Option<f64> {
    let task = task?;
    if let Some(p) = task.progress {
        return Some(p.clamp(0.0, 100.0).round());
    }
    match (task.current, task.total) {
        (Some(c), Some(t)) if t > 0 => Some((c as f64 / t as f64 * 100.0).round()),
        _ => None,
    }
}

/// What the server reports, as the downloads see it.
#[derive(Clone, Debug, Default)]
pub struct Inputs<'a> {
    pub speech: Option<&'a ModelReadiness>,
    pub cleanup: Option<&'a ModelReadiness>,
    /// Whether cleanup is on, so its model is needed.
    pub needs_cleanup: bool,
    pub tasks: &'a [ActiveDownloadTask],
    pub free_mb: Option<f64>,
    pub started: bool,
}

impl<'a> Inputs<'a> {
    fn models(&self) -> Vec<&'a ModelReadiness> {
        let mut list: Vec<&ModelReadiness> = self.speech.into_iter().collect();
        if self.needs_cleanup {
            list.extend(self.cleanup);
        }
        list
    }

    fn task(&self, model: Option<&ModelReadiness>) -> Option<&'a ActiveDownloadTask> {
        let model = model?;
        self.tasks.iter().find(|t| t.model_name == model.model_name)
    }

    fn disk_short_mb(&self) -> f64 {
        let needed: f64 = self
            .models()
            .iter()
            .filter(|m| !m.ready)
            .map(|m| m.size_mb.unwrap_or(0) as f64)
            .sum();
        self.free_mb
            .map(|free| missing_disk_mb(free, needed))
            .unwrap_or(0.0)
    }
}

#[derive(Default)]
pub struct Downloads {
    failures: HashMap<String, u32>,
    /// Models requested and not yet seen failing.
    requested: HashSet<String>,
}

impl Downloads {
    /// What to do now: count new failures (retrying while that's allowed),
    /// and start the next model once downloads were started.
    pub fn step(&mut self, inputs: &Inputs) -> Vec<Action> {
        let mut actions = Vec::new();
        let models = inputs.models();
        for model in &models {
            let task = inputs.task(Some(model));
            if task.map(|t| t.status.as_str()) != Some("error")
                || !self.requested.contains(&model.model_name)
            {
                continue;
            }
            self.requested.remove(&model.model_name);
            let count = self.failures.entry(model.model_name.clone()).or_default();
            *count += 1;
            if should_retry_automatically(*count) {
                // Requested again when the retry runs.
                self.requested.insert(model.model_name.clone());
                actions.push(Action::RetryLater(model.model_name.clone()));
            }
        }
        if inputs.started
            && inputs.disk_short_mb() <= 0.0
            && let Some(next) = models.iter().find(|m| !m.ready)
        {
            let status = inputs.task(Some(next)).map(|t| t.status.as_str());
            let busy = matches!(status, Some("downloading" | "extracting" | "error"));
            if !busy && !self.requested.contains(&next.model_name) {
                self.requested.insert(next.model_name.clone());
                actions.push(Action::Request(next.model_name.clone()));
            }
        }
        actions
    }

    /// The user asked to try `model` again: its count starts over.
    pub fn retry(&mut self, model: &str) {
        self.failures.insert(model.to_string(), 0);
        self.requested.insert(model.to_string());
    }

    /// A request didn't reach the server: let the next step try again.
    pub fn request_failed(&mut self, model: &str) {
        self.requested.remove(model);
    }

    pub fn summary(&self, inputs: &Inputs) -> Summary {
        let models = inputs.models();
        let first_missing = models
            .iter()
            .find(|m| !m.ready)
            .map(|m| m.model_name.clone());
        let progress = |model: Option<&ModelReadiness>| -> ModelProgress {
            let task = inputs.task(model);
            let failures = model
                .and_then(|m| self.failures.get(&m.model_name).copied())
                .unwrap_or(0);
            let owned = model.cloned();
            if model.is_some_and(|m| m.ready) {
                return ModelProgress {
                    model: owned,
                    state: ModelState::Ready,
                    percent: 100.0,
                    error: None,
                };
            }
            let errored = task.is_some_and(|t| t.status == "error");
            if errored && !should_retry_automatically(failures) && failures > 0 {
                return ModelProgress {
                    model: owned,
                    state: ModelState::Failed,
                    percent: progress_percent(task).unwrap_or(0.0),
                    error: task.and_then(|t| t.error.clone()),
                };
            }
            if let Some(task) = task.filter(|t| t.status != "error") {
                return ModelProgress {
                    model: owned,
                    state: ModelState::Downloading,
                    percent: progress_percent(Some(task)).unwrap_or(0.0),
                    error: None,
                };
            }
            let is_next = model.is_some_and(|m| Some(&m.model_name) == first_missing.as_ref());
            ModelProgress {
                model: owned,
                state: match (inputs.started, is_next) {
                    (false, _) => ModelState::Waiting,
                    (true, true) => ModelState::Downloading,
                    (true, false) => ModelState::Next,
                },
                percent: 0.0,
                error: None,
            }
        };
        let speech = progress(inputs.speech);
        let cleanup = inputs.needs_cleanup.then(|| progress(inputs.cleanup));
        let failed_is_speech = speech.state == ModelState::Failed;
        let failed = if failed_is_speech {
            Some(speech.clone())
        } else {
            cleanup.clone().filter(|c| c.state == ModelState::Failed)
        };
        let ready = speech.state == ModelState::Ready
            && cleanup
                .as_ref()
                .is_none_or(|c| c.state == ModelState::Ready);
        let overall = match &cleanup {
            Some(c) => (speech.percent + c.percent) / 2.0,
            None => speech.percent,
        };
        Summary {
            speech,
            cleanup,
            ready,
            failed,
            failed_is_speech,
            disk_short_mb: inputs.disk_short_mb(),
            overall,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str, ready: bool) -> ModelReadiness {
        ModelReadiness {
            ready,
            model_name: name.into(),
            display_name: name.to_uppercase(),
            size: String::new(),
            size_mb: Some(1000),
        }
    }

    fn task(name: &str, status: &str, progress: Option<f64>) -> ActiveDownloadTask {
        ActiveDownloadTask {
            model_name: name.into(),
            status: status.into(),
            started_at: String::new(),
            error: (status == "error").then(|| "network".into()),
            progress,
            current: None,
            total: None,
            filename: None,
        }
    }

    #[test]
    fn nothing_starts_before_the_user_does() {
        let (s, c) = (model("whisper", false), model("qwen", false));
        let inputs = Inputs {
            speech: Some(&s),
            cleanup: Some(&c),
            needs_cleanup: true,
            ..Default::default()
        };
        let mut d = Downloads::default();
        assert!(d.step(&inputs).is_empty());
        let sum = d.summary(&inputs);
        assert_eq!(sum.speech.state, ModelState::Waiting);
        assert!(!sum.ready);
    }

    #[test]
    fn speech_downloads_first_then_cleanup() {
        let (s, c) = (model("whisper", false), model("qwen", false));
        let mut inputs = Inputs {
            speech: Some(&s),
            cleanup: Some(&c),
            needs_cleanup: true,
            started: true,
            ..Default::default()
        };
        let mut d = Downloads::default();
        assert_eq!(d.step(&inputs), vec![Action::Request("whisper".into())]);
        // Asked once only, while the server catches up.
        assert!(d.step(&inputs).is_empty());
        let sum = d.summary(&inputs);
        assert_eq!(sum.speech.state, ModelState::Downloading);
        assert_eq!(sum.cleanup.unwrap().state, ModelState::Next);

        let s_ready = model("whisper", true);
        inputs.speech = Some(&s_ready);
        assert_eq!(d.step(&inputs), vec![Action::Request("qwen".into())]);
    }

    #[test]
    fn overall_is_the_average_of_both() {
        let (s, c) = (model("whisper", true), model("qwen", false));
        let tasks = vec![task("qwen", "downloading", Some(40.0))];
        let inputs = Inputs {
            speech: Some(&s),
            cleanup: Some(&c),
            needs_cleanup: true,
            tasks: &tasks,
            started: true,
            ..Default::default()
        };
        let sum = Downloads::default().summary(&inputs);
        assert_eq!(sum.overall, 70.0);
        assert_eq!(sum.cleanup.unwrap().percent, 40.0);
    }

    #[test]
    fn cleanup_off_needs_speech_only() {
        let s = model("whisper", true);
        let inputs = Inputs {
            speech: Some(&s),
            needs_cleanup: false,
            started: true,
            ..Default::default()
        };
        let sum = Downloads::default().summary(&inputs);
        assert!(sum.ready);
        assert!(sum.cleanup.is_none());
        assert_eq!(sum.overall, 100.0);
    }

    #[test]
    fn failures_retry_then_give_up() {
        let s = model("whisper", false);
        let tasks = vec![task("whisper", "error", Some(12.0))];
        let inputs = Inputs {
            speech: Some(&s),
            tasks: &tasks,
            started: true,
            ..Default::default()
        };
        let mut d = Downloads::default();
        d.requested.insert("whisper".into());
        for _ in 0..3 {
            assert_eq!(d.step(&inputs), vec![Action::RetryLater("whisper".into())]);
            assert_eq!(d.summary(&inputs).speech.state, ModelState::Downloading);
        }
        assert!(d.step(&inputs).is_empty());
        let sum = d.summary(&inputs);
        assert_eq!(sum.speech.state, ModelState::Failed);
        assert_eq!(sum.speech.percent, 12.0);
        assert!(sum.failed_is_speech);
        assert_eq!(sum.failed.unwrap().error.as_deref(), Some("network"));

        d.retry("whisper");
        assert_eq!(d.summary(&inputs).speech.state, ModelState::Downloading);
    }

    #[test]
    fn a_short_disk_holds_the_downloads() {
        let s = model("whisper", false);
        let inputs = Inputs {
            speech: Some(&s),
            free_mb: Some(1200.0),
            started: true,
            ..Default::default()
        };
        let mut d = Downloads::default();
        assert!(d.step(&inputs).is_empty());
        assert_eq!(d.summary(&inputs).disk_short_mb, 300.0);
    }

    #[test]
    fn percent_falls_back_to_bytes() {
        let mut t = task("x", "downloading", None);
        assert_eq!(progress_percent(Some(&t)), None);
        t.current = Some(25);
        t.total = Some(100);
        assert_eq!(progress_percent(Some(&t)), Some(25.0));
        t.progress = Some(140.0);
        assert_eq!(progress_percent(Some(&t)), Some(100.0));
    }
}
