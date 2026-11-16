//! A toast for every model download, wherever it started (the Models tab,
//! onboarding, dictation asking for a missing model): it follows the
//! download's progress stream and ends on done or failed. Downloads show up
//! in the shell's data, polled every few seconds.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{ModelProgress, ProgressStatus};
use kass_api::{ApiClient, SseCancel};
use kass_ui::{metrics, theme};

use super::data::ShellData;
use crate::host;

const P: &str = "models-download-toast";

fn t(key: &str) -> String {
    kass_i18n::t(&format!("{P}-{key}"))
}

/// What the stream thread sends back.
enum Update {
    /// The model's name as the Models tab shows it.
    Named(String),
    Progress(ModelProgress),
    /// The stream ended before saying the download finished.
    Lost,
}

/// Keys the toasts, one per model.
struct DownloadToast;

#[derive(Clone)]
struct Following {
    name: String,
    progress: Option<ModelProgress>,
}

pub struct DownloadToasts {
    window: AnyWindowHandle,
    data: Entity<ShellData>,
    /// Downloads the server listed last time, toasted already.
    seen: HashSet<String>,
    following: HashMap<String, (Following, SseCancel)>,
    _subscription: Subscription,
}

struct DownloadsGlobal(WeakEntity<DownloadToasts>);

impl Global for DownloadsGlobal {}

fn toasts(cx: &App) -> Option<Entity<DownloadToasts>> {
    cx.try_global::<DownloadsGlobal>()?.0.upgrade()
}

impl DownloadToasts {
    pub fn new(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_global(DownloadsGlobal(cx.entity().downgrade()));
        let subscription = cx.observe(&data, |this, _, cx| this.sync(cx));
        Self {
            window: window.window_handle(),
            data,
            seen: HashSet::new(),
            following: HashMap::new(),
            _subscription: subscription,
        }
    }

    /// Toast each download the server lists that isn't toasted yet. One that
    /// leaves the list is toasted again if it starts over.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let data = self.data.read(cx);
        let listed: HashSet<String> = data
            .downloads
            .iter()
            .filter(|download| download.status != "error")
            .map(|download| download.model_name.clone())
            .collect();
        let api = data.api.clone();
        let new: Vec<String> = listed.difference(&self.seen).cloned().collect();
        self.seen = listed;
        for model in new {
            if !self.following.contains_key(&model) {
                self.follow(model, api.clone(), cx);
            }
        }
    }

    fn follow(&mut self, model: String, api: ApiClient, cx: &mut Context<Self>) {
        let cancel = SseCancel::new();
        let following = Following {
            name: model.clone(),
            progress: None,
        };
        self.following
            .insert(model.clone(), (following, cancel.clone()));
        self.show(progress_toast(&model), cx);
        let thread_model = model.clone();
        let started = std::thread::Builder::new()
            .name("kass-download-toast".into())
            .spawn(move || stream(&api, &thread_model, &cancel));
        if started.is_err() {
            self.following.remove(&model);
        }
    }

    fn update(&mut self, model: &str, update: Update, cx: &mut Context<Self>) {
        let Some((following, _)) = self.following.get_mut(model) else {
            return;
        };
        match update {
            Update::Named(name) => following.name = name,
            Update::Progress(progress) => {
                let ended = ended(&progress);
                let failed = progress.status == ProgressStatus::Error;
                following.progress = Some(progress);
                if ended {
                    let name = following.name.clone();
                    self.following.remove(model);
                    let note = if failed {
                        end_toast(model, &name, NotificationType::Error, t("failed"))
                    } else {
                        end_toast(model, &name, NotificationType::Success, t("complete"))
                    };
                    self.show(note, cx);
                }
            }
            Update::Lost => {
                let name = following.name.clone();
                self.following.remove(model);
                self.show(
                    end_toast(model, &name, NotificationType::Error, t("lost")),
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn show(&self, note: Notification, cx: &mut Context<Self>) {
        let _ = self
            .window
            .update(cx, |_, window, cx| window.push_notification(note, cx));
    }
}

impl Drop for DownloadToasts {
    fn drop(&mut self) {
        for (_, cancel) in self.following.values() {
            cancel.cancel();
        }
    }
}

/// The server sends no `complete` for some downloads: 100% is done too.
fn ended(progress: &ModelProgress) -> bool {
    progress.is_finished() || progress.progress >= 100.
}

/// Runs on its own thread until the download ends or the toasts go away.
fn stream(api: &ApiClient, model: &str, cancel: &SseCancel) {
    let send = |update: Update| {
        let model = model.to_string();
        host::with_app(move |cx| {
            if let Some(toasts) = toasts(cx) {
                toasts.update(cx, |toasts, cx| toasts.update(&model, update, cx));
            }
        });
    };
    if let Some(name) = api.model_status().ok().and_then(|list| {
        list.models
            .into_iter()
            .find(|status| status.model_name == model)
            .map(|status| status.display_name)
    }) {
        send(Update::Named(name));
    }
    if let Ok(events) = api.model_progress(model, cancel) {
        for event in events {
            let Ok(event) = event else { break };
            let Ok(progress) = event.json::<ModelProgress>() else {
                continue;
            };
            let done = ended(&progress);
            send(Update::Progress(progress));
            if done {
                return;
            }
        }
    }
    if !cancel.is_cancelled() {
        send(Update::Lost);
    }
}

/// The toast while the download runs; it draws from the latest progress.
fn progress_toast(model: &str) -> Notification {
    let key = model.to_string();
    Notification::new()
        .id1::<DownloadToast>(SharedString::from(model.to_string()))
        .autohide(false)
        .content(move |_, _, cx| {
            let following = toasts(cx).and_then(|toasts| {
                toasts
                    .read(cx)
                    .following
                    .get(&key)
                    .map(|(following, _)| following.clone())
            });
            match following {
                Some(following) => progress_body(&key, &following, cx),
                None => div().into_any_element(),
            }
        })
}

fn progress_body(model: &str, following: &Following, cx: &App) -> AnyElement {
    let theme = theme(cx);
    let status = match &following.progress {
        None => t("connecting"),
        Some(progress) => match progress.status {
            ProgressStatus::Downloading => progress
                .filename
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| t("downloading")),
            ProgressStatus::Extracting => t("extracting"),
            _ => t("processing"),
        },
    };
    let measured = following.progress.as_ref().filter(|p| p.total > 0.);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(metrics::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .child(following.name.clone()),
        )
        .child(
            div()
                .text_size(metrics::TEXT_SM)
                .text_color(theme.muted_foreground)
                .truncate()
                .child(status),
        )
        .child(match measured {
            Some(progress) => Progress::new(SharedString::from(format!("download-{model}")))
                .value(progress.progress as f32)
                .into_any_element(),
            None => Progress::new(SharedString::from(format!("download-{model}")))
                .loading(true)
                .into_any_element(),
        })
        .when_some(measured, |body, progress| {
            body.child(
                div()
                    .text_size(metrics::TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} / {} ({:.1}%)",
                        format_bytes(progress.current),
                        format_bytes(progress.total),
                        progress.progress
                    )),
            )
        })
        .into_any_element()
}

/// Replaces the running toast; it hides after a few seconds.
fn end_toast(model: &str, name: &str, kind: NotificationType, message: String) -> Notification {
    Notification::new()
        .id1::<DownloadToast>(SharedString::from(model.to_string()))
        .with_type(kind)
        .title(name.to_string())
        .message(message)
}

/// Bytes the way the Models tab shows them: `1.5 GB`.
fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if bytes <= 0. {
        return "0 B".into();
    }
    let unit = ((bytes.ln() / 1024f64.ln()).floor() as usize).min(UNITS.len() - 1);
    format!("{:.1} {}", bytes / 1024f64.powi(unit as i32), UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::{ended, format_bytes, ModelProgress, ProgressStatus};

    fn progress(status: ProgressStatus, percent: f64) -> ModelProgress {
        ModelProgress {
            model_name: "whisper-base".into(),
            current: 0.,
            total: 0.,
            progress: percent,
            filename: None,
            status,
            timestamp: None,
            error: None,
        }
    }

    #[test]
    fn a_download_ends_on_done_failed_or_full() {
        assert!(ended(&progress(ProgressStatus::Complete, 40.)));
        assert!(ended(&progress(ProgressStatus::Error, 0.)));
        assert!(ended(&progress(ProgressStatus::Downloading, 100.)));
        assert!(!ended(&progress(ProgressStatus::Downloading, 99.9)));
        assert!(!ended(&progress(ProgressStatus::Extracting, 50.)));
    }

    #[test]
    fn bytes_read_like_the_models_tab() {
        assert_eq!(format_bytes(0.), "0 B");
        assert_eq!(format_bytes(512.), "512.0 B");
        assert_eq!(format_bytes(1536.), "1.5 KB");
        assert_eq!(format_bytes(3. * 1024. * 1024. * 1024.), "3.0 GB");
        assert_eq!(format_bytes(5000. * 1024. * 1024. * 1024.), "5000.0 GB");
    }
}
