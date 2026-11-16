//! Which models are downloading or failed to: the server's active tasks,
//! merged with errors the progress stream reported and the ones the user
//! dismissed without asking the server.

use kass_api::types::{ActiveDownloadTask, ModelStatus};
use std::collections::{HashMap, HashSet};

/// One model's download, as the page shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DownloadState {
    pub downloading: bool,
    /// Live progress, when the server reports it.
    pub progress: Option<ActiveDownloadTask>,
    /// The failed download, when there is one.
    pub error: Option<ActiveDownloadTask>,
}

#[derive(Default)]
pub struct Downloads {
    tasks: Vec<ActiveDownloadTask>,
    /// Errors from the progress stream, which say more than the task list.
    local_errors: HashMap<String, (String, String)>,
    dismissed: HashSet<String>,
    /// The model this page started downloading, until it finishes.
    pub started: Option<String>,
}

impl Downloads {
    pub fn set_tasks(&mut self, tasks: Vec<ActiveDownloadTask>) {
        self.tasks = tasks;
    }

    /// Any download is under way, so progress is worth polling often.
    pub fn any_active(&self) -> bool {
        self.started.is_some() || self.tasks.iter().any(|task| task.status == "downloading")
    }

    /// The user asked to download it (again).
    pub fn start(&mut self, model_name: &str) {
        self.dismissed.remove(model_name);
        self.local_errors.remove(model_name);
        self.started = Some(model_name.to_string());
    }

    /// Starting it failed.
    pub fn start_failed(&mut self, model_name: &str) {
        if self.started.as_deref() == Some(model_name) {
            self.started = None;
        }
    }

    pub fn finished(&mut self, model_name: &str) {
        if self.started.as_deref() == Some(model_name) {
            self.started = None;
        }
    }

    pub fn failed(&mut self, model_name: &str, error: String, at: String) {
        self.local_errors
            .insert(model_name.to_string(), (error, at));
        self.finished(model_name);
    }

    /// Hides a download or its error; returns what to put back if the server
    /// refuses.
    pub fn dismiss(&mut self, model_name: &str) -> Undo {
        let undo = Undo {
            model_name: model_name.to_string(),
            was_dismissed: self.dismissed.contains(model_name),
            local_error: self.local_errors.get(model_name).cloned(),
            started: self.started.clone(),
        };
        self.dismissed.insert(model_name.to_string());
        self.local_errors.remove(model_name);
        if self.started.as_deref() == Some(model_name) {
            self.started = None;
        }
        undo
    }

    pub fn undo(&mut self, undo: Undo) {
        if !undo.was_dismissed {
            self.dismissed.remove(&undo.model_name);
        }
        if let Some(error) = undo.local_error {
            self.local_errors.insert(undo.model_name, error);
        }
        self.started = undo.started;
    }

    /// Every task was cleared on the server.
    pub fn clear(&mut self) {
        self.dismissed.clear();
        self.local_errors.clear();
        self.started = None;
    }

    /// The failed downloads that aren't dismissed, the server's first.
    pub fn errors(&self) -> Vec<ActiveDownloadTask> {
        let mut errors: Vec<ActiveDownloadTask> = Vec::new();
        for task in &self.tasks {
            if task.status != "error" || self.dismissed.contains(&task.model_name) {
                continue;
            }
            let mut task = task.clone();
            if let Some((error, _)) = self.local_errors.get(&task.model_name) {
                task.error = Some(error.clone());
            }
            errors.push(task);
        }
        let mut local: Vec<_> = self.local_errors.iter().collect();
        local.sort_by(|a, b| a.0.cmp(b.0));
        for (name, (error, at)) in local {
            if self.dismissed.contains(name) || errors.iter().any(|e| &e.model_name == name) {
                continue;
            }
            errors.push(ActiveDownloadTask {
                model_name: name.clone(),
                status: "error".into(),
                started_at: at.clone(),
                error: Some(error.clone()),
                progress: None,
                current: None,
                total: None,
                filename: None,
            });
        }
        errors
    }

    pub fn state_of(&self, model: &ModelStatus) -> DownloadState {
        let name = &model.model_name;
        let error = self.errors().into_iter().find(|e| &e.model_name == name);
        let downloading = (model.downloading || self.started.as_ref() == Some(name))
            && error.is_none()
            && !self.dismissed.contains(name);
        let progress = self
            .tasks
            .iter()
            .find(|task| &task.model_name == name && task.status == "downloading")
            .cloned();
        DownloadState {
            downloading,
            progress,
            error,
        }
    }
}

/// What [`Downloads::dismiss`] changed.
pub struct Undo {
    model_name: String,
    was_dismissed: bool,
    local_error: Option<(String, String)>,
    started: Option<String>,
}

/// Download percent (0 to 100), or `None` while the total size is unknown.
pub fn percent(progress: Option<&ActiveDownloadTask>) -> Option<f64> {
    let progress = progress?;
    match progress.total {
        Some(total) if total > 0 => Some(progress.progress.unwrap_or(0.)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str, downloading: bool) -> ModelStatus {
        ModelStatus {
            model_name: name.into(),
            display_name: name.into(),
            hf_repo_id: None,
            downloaded: false,
            downloading,
            size_mb: None,
            loaded: false,
        }
    }

    fn task(name: &str, status: &str, error: Option<&str>) -> ActiveDownloadTask {
        ActiveDownloadTask {
            model_name: name.into(),
            status: status.into(),
            started_at: "2026-11-16T10:00:00Z".into(),
            error: error.map(Into::into),
            progress: Some(40.),
            current: Some(40),
            total: Some(100),
            filename: None,
        }
    }

    #[test]
    fn started_download_shows_until_it_finishes() {
        let mut downloads = Downloads::default();
        downloads.start("whisper-base");
        assert!(
            downloads
                .state_of(&model("whisper-base", false))
                .downloading
        );
        assert!(downloads.any_active());
        downloads.finished("whisper-base");
        assert!(
            !downloads
                .state_of(&model("whisper-base", false))
                .downloading
        );
    }

    #[test]
    fn stream_errors_win_over_the_task_list() {
        let mut downloads = Downloads::default();
        downloads.set_tasks(vec![task("whisper-base", "error", Some("short"))]);
        downloads.failed("whisper-base", "the long story".into(), "now".into());
        let errors = downloads.errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error.as_deref(), Some("the long story"));
        let state = downloads.state_of(&model("whisper-base", true));
        assert!(!state.downloading);
        assert!(state.error.is_some());
    }

    #[test]
    fn dismissing_hides_and_undo_restores() {
        let mut downloads = Downloads::default();
        downloads.failed("qwen3-4b", "disk full".into(), "now".into());
        let undo = downloads.dismiss("qwen3-4b");
        assert!(downloads.errors().is_empty());
        downloads.undo(undo);
        assert_eq!(downloads.errors().len(), 1);
    }

    #[test]
    fn downloading_again_clears_the_dismissal() {
        let mut downloads = Downloads::default();
        downloads.set_tasks(vec![task("whisper-small", "error", None)]);
        downloads.dismiss("whisper-small");
        assert!(downloads.errors().is_empty());
        downloads.start("whisper-small");
        assert_eq!(downloads.errors().len(), 1);
    }

    #[test]
    fn progress_and_percent() {
        let mut downloads = Downloads::default();
        downloads.set_tasks(vec![task("whisper-turbo", "downloading", None)]);
        let state = downloads.state_of(&model("whisper-turbo", true));
        assert!(state.downloading);
        assert_eq!(percent(state.progress.as_ref()), Some(40.));
        let mut unknown = task("whisper-turbo", "downloading", None);
        unknown.total = None;
        assert_eq!(percent(Some(&unknown)), None);
        assert_eq!(percent(None), None);
    }
}
