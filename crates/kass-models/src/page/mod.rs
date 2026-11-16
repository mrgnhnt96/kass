//! The Models page: the speech and cleanup models on the left, grouped by
//! family, and the chosen one on the right with its download, Hugging Face
//! facts and what can be done with it. Also where the models folder lives
//! and moves.

mod stats;
mod view;

use crate::catalog::{self, Role};
use crate::downloads::Downloads;
use crate::host::{ModelsHost, NoHost};
use gpui_kit::*;
use kass_api::types::{
    ActiveDownloadTask, CaptureSettings, HuggingFaceModelInfo, ModelProgress, ModelStatus,
    ProgressStatus,
};
use kass_api::{ApiClient, SseCancel, hugging_face_model_info};
use kass_i18n::{t, tr};
use kass_ui::{Toast, ToastKind, close_dialog, show_toast};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long Hugging Face facts are kept before they're fetched again.
const INFO_TTL: Duration = Duration::from_secs(30 * 60);
/// Status is fetched this often; downloads every second while one runs.
const POLL_TICKS: u32 = 5;
const TICK: Duration = Duration::from_secs(1);
/// How often a background stream's news is picked up.
const STREAM_CHECK: Duration = Duration::from_millis(250);

/// A model's Hugging Face facts.
enum Info {
    Loading,
    Loaded(Box<HuggingFaceModelInfo>, Instant),
    Failed(Instant),
}

/// What's being done to which model, so its buttons can say so.
#[derive(Default)]
struct Busy {
    using: Option<String>,
    unloading: Option<String>,
    deleting: Option<String>,
    cancelling: Option<String>,
    clearing: bool,
}

/// Moving the models folder: the server is offline until it's done.
struct Migration {
    restarting: bool,
    progress: Option<ModelProgress>,
}

/// How a download's progress stream ended.
enum Outcome {
    Done,
    Failed(String),
    /// The stream dropped; the status poll says what happened.
    Lost,
}

enum MigrationNews {
    Progress(ModelProgress),
    NothingToMove,
    Moved,
    Failed(String),
}

/// One round of fetching.
struct Fetched {
    models: Option<Result<Vec<ModelStatus>, String>>,
    tasks: Option<Vec<ActiveDownloadTask>>,
    settings: Option<CaptureSettings>,
    cache_dir: Option<String>,
}

impl Fetched {
    fn load(client: &ApiClient, full: bool, cache_dir: bool) -> Self {
        Self {
            models: full.then(|| {
                client
                    .model_status()
                    .map(|status| status.models)
                    .map_err(|e| e.to_string())
            }),
            tasks: client.active_tasks().ok().map(|tasks| tasks.downloads),
            settings: full.then(|| client.capture_settings().ok()).flatten(),
            cache_dir: cache_dir.then(|| client.models_cache_dir().ok()).flatten(),
        }
    }
}

pub struct ModelsPage {
    client: ApiClient,
    host: Rc<dyn ModelsHost>,

    /// `None` until the first fetch.
    models: Option<Vec<ModelStatus>>,
    settings: Option<CaptureSettings>,
    cache_dir: Option<String>,
    downloads: Downloads,
    /// The model asked for (a click or `?model=`); the default when unset or gone.
    selected: Option<String>,
    /// Hugging Face facts, by repository.
    info: HashMap<String, Info>,
    busy: Busy,
    migration: Option<Migration>,

    poll: Option<Task<()>>,
    /// Open download progress streams, by model.
    listening: HashMap<String, SseCancel>,
    migration_events: SseCancel,
}

impl Drop for ModelsPage {
    fn drop(&mut self) {
        for cancel in self.listening.values() {
            cancel.cancel();
        }
        self.migration_events.cancel();
    }
}

impl ModelsPage {
    pub fn new(client: ApiClient, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            client,
            host: Rc::new(NoHost),
            models: None,
            settings: None,
            cache_dir: None,
            downloads: Downloads::default(),
            selected: None,
            info: HashMap::new(),
            busy: Busy::default(),
            migration: None,
            poll: None,
            listening: HashMap::new(),
            migration_events: SseCancel::new(),
        }
    }

    /// The app around the page: the models folder and settings it keeps.
    pub fn with_host(mut self, host: Rc<dyn ModelsHost>) -> Self {
        self.host = host;
        self
    }

    /// Fetch everything now and keep polling until [`Self::hide`].
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.poll = Some(cx.spawn(async move |page, cx| {
            let mut tick: u32 = 0;
            loop {
                let Ok((active, needs_dir)) = page.read_with(cx, |page, _| {
                    (page.downloads.any_active(), page.cache_dir.is_none())
                }) else {
                    return;
                };
                let full = tick.is_multiple_of(POLL_TICKS);
                if full || active {
                    let client = client.clone();
                    let fetched = cx
                        .background_executor()
                        .spawn(async move { Fetched::load(&client, full, full && needs_dir) })
                        .await;
                    if page.update(cx, |page, cx| page.apply(fetched, cx)).is_err() {
                        return;
                    }
                }
                cx.background_executor().timer(TICK).await;
                tick = tick.wrapping_add(1);
            }
        }));
    }

    /// Stop polling while the page isn't on screen.
    pub fn hide(&mut self) {
        self.poll = None;
    }

    /// Show `model` (a `?model=` link), or keep the current choice.
    pub fn show(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        if let Some(model) = model {
            self.selected = Some(model);
            cx.notify();
        }
    }

    fn apply(&mut self, fetched: Fetched, cx: &mut Context<Self>) {
        match fetched.models {
            Some(Ok(models)) => self.models = Some(models),
            Some(Err(e)) => {
                eprintln!("[models] status: {e}");
                self.models.get_or_insert_with(Vec::new);
            }
            None => {}
        }
        if let Some(tasks) = fetched.tasks {
            self.downloads.set_tasks(tasks);
        }
        if let Some(settings) = fetched.settings {
            self.settings = Some(settings);
        }
        if let Some(dir) = fetched.cache_dir {
            self.cache_dir = Some(dir);
        }
        cx.notify();
    }

    /// One fetch of everything, after something changed.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |page, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move { Fetched::load(&client, true, false) })
                .await;
            page.update(cx, |page, cx| page.apply(fetched, cx)).ok();
        })
        .detach();
    }

    fn call<T: Send + 'static>(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        request: impl FnOnce(&ApiClient) -> kass_api::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let client = self.client.clone();
        cx.spawn_in(window, async move |page, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { request(&client).map_err(|e| e.to_string()) })
                .await;
            page.update_in(cx, |page, window, cx| done(page, result, window, cx))
                .ok();
        })
        .detach();
    }

    fn models(&self) -> &[ModelStatus] {
        self.models.as_deref().unwrap_or_default()
    }

    fn model(&self, name: &str) -> Option<&ModelStatus> {
        self.models().iter().find(|model| model.model_name == name)
    }

    fn display_name(&self, name: &str) -> String {
        self.model(name)
            .map_or_else(|| name.to_string(), |model| model.display_name.clone())
    }

    fn in_use(&self) -> HashMap<String, Role> {
        catalog::in_use(self.settings.as_ref())
    }

    /// The model on the right: the one asked for, else the transcription
    /// model in use, else the first.
    fn current(&self) -> Option<&ModelStatus> {
        let models = self.models();
        if let Some(model) = self.selected.as_deref().and_then(|name| self.model(name)) {
            return Some(model);
        }
        let in_use = self.in_use();
        models
            .iter()
            .find(|model| in_use.get(&model.model_name) == Some(&Role::Transcription))
            .or_else(|| models.first())
    }

    fn select(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = Some(name);
        cx.notify();
    }

    /// Fetch the current model's Hugging Face facts if they're missing or old.
    fn ensure_info(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.current().and_then(|model| model.hf_repo_id.clone()) else {
            return;
        };
        let fresh = match self.info.get(&repo) {
            Some(Info::Loading) => true,
            Some(Info::Loaded(_, at) | Info::Failed(at)) => at.elapsed() < INFO_TTL,
            None => false,
        };
        if fresh {
            return;
        }
        self.info.insert(repo.clone(), Info::Loading);
        cx.spawn(async move |page, cx| {
            let id = repo.clone();
            let result = cx
                .background_executor()
                .spawn(async move { hugging_face_model_info(&id) })
                .await;
            page.update(cx, |page, cx| {
                let info = match result {
                    Ok(info) => Info::Loaded(Box::new(info), Instant::now()),
                    Err(e) => {
                        eprintln!("[models] {repo} on Hugging Face: {e}");
                        Info::Failed(Instant::now())
                    }
                };
                page.info.insert(repo, info);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn download(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.downloads.start(&name);
        cx.notify();
        let model = name.clone();
        self.call(
            window,
            cx,
            move |client| client.trigger_model_download(&model),
            move |page, result, window, cx| {
                match result {
                    Ok(_) => page.listen(name, window, cx),
                    Err(e) => {
                        page.downloads.start_failed(&name);
                        toast_error(t("models-toast-download-failed"), e, window, cx);
                    }
                }
                page.reload(cx);
            },
        );
    }

    /// Follows a download's progress stream until it ends.
    fn listen(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.listening.remove(&name) {
            old.cancel();
        }
        let cancel = SseCancel::new();
        self.listening.insert(name.clone(), cancel.clone());
        let (sender, receiver) = mpsc::channel::<Outcome>();
        let client = self.client.clone();
        let model = name.clone();
        std::thread::Builder::new()
            .name("kass-model-progress".into())
            .spawn(move || {
                let _ = sender.send(follow_download(&client, &model, &cancel));
            })
            .ok();
        cx.spawn_in(window, async move |page, cx| {
            loop {
                cx.background_executor().timer(STREAM_CHECK).await;
                match receiver.try_recv() {
                    Ok(outcome) => {
                        page.update_in(cx, |page, window, cx| {
                            page.download_ended(name, outcome, window, cx)
                        })
                        .ok();
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
        })
        .detach();
    }

    fn download_ended(
        &mut self,
        name: String,
        outcome: Outcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.listening.remove(&name).is_none() {
            // Cancelled meanwhile.
            return;
        }
        match outcome {
            Outcome::Done => {
                self.downloads.finished(&name);
                show_toast(
                    window,
                    cx,
                    Toast::new(t("models-toast-download-complete")).description(tr!(
                        "models-toast-download-complete-description",
                        name = self.display_name(&name)
                    )),
                );
                self.host.models_changed(cx);
            }
            Outcome::Failed(error) => {
                let error = if error.is_empty() {
                    t("common-unknown-error")
                } else {
                    error
                };
                self.downloads
                    .failed(&name, error.clone(), chrono::Utc::now().to_rfc3339());
                toast_error(t("models-toast-download-failed"), error, window, cx);
            }
            Outcome::Lost => self.downloads.finished(&name),
        }
        self.reload(cx);
        cx.notify();
    }

    /// Cancels a download, or dismisses a failed one.
    fn cancel(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let undo = self.downloads.dismiss(&name);
        self.busy.cancelling = Some(name.clone());
        cx.notify();
        let model = name.clone();
        self.call(
            window,
            cx,
            move |client| client.cancel_download(&model),
            move |page, result, window, cx| {
                page.busy.cancelling = None;
                match result {
                    Ok(_) => {
                        if let Some(cancel) = page.listening.remove(&name) {
                            cancel.cancel();
                        }
                        page.reload(cx);
                    }
                    Err(_) => {
                        page.downloads.undo(undo);
                        show_toast(
                            window,
                            cx,
                            Toast::new(t("models-toast-cancel-failed"))
                                .description(t("models-toast-cancel-failed-description"))
                                .kind(ToastKind::Destructive),
                        );
                    }
                }
                cx.notify();
            },
        );
    }

    fn clear_problems(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.clearing = true;
        cx.notify();
        self.call(
            window,
            cx,
            |client| client.clear_all_tasks(),
            |page, result, window, cx| {
                page.busy.clearing = false;
                match result {
                    Ok(_) => {
                        page.downloads.clear();
                        page.reload(cx);
                    }
                    Err(e) => toast_error(t("common-error"), e, window, cx),
                }
                cx.notify();
            },
        );
    }

    /// Makes dictation use the model for its role.
    fn use_model(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(patch) = catalog::setting_for(&name) else {
            return;
        };
        self.busy.using = Some(name);
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.update_capture_settings(&patch),
            |page, result, window, cx| {
                page.busy.using = None;
                match result {
                    Ok(settings) => {
                        page.host.settings_changed(&settings, cx);
                        page.settings = Some(settings);
                    }
                    Err(e) => toast_error(t("common-error"), e, window, cx),
                }
                cx.notify();
            },
        );
    }

    fn unload(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.unloading = Some(name.clone());
        cx.notify();
        let model = name.clone();
        self.call(
            window,
            cx,
            move |client| client.unload_model(&model),
            move |page, result, window, cx| {
                page.busy.unloading = None;
                match result {
                    Ok(_) => {
                        show_toast(
                            window,
                            cx,
                            Toast::new(t("models-toast-unloaded")).description(tr!(
                                "models-toast-unloaded-description",
                                name = page.display_name(&name)
                            )),
                        );
                        page.host.models_changed(cx);
                        page.reload(cx);
                    }
                    Err(e) => toast_error(t("models-toast-unload-failed"), e, window, cx),
                }
                cx.notify();
            },
        );
    }

    fn delete(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.deleting = Some(name.clone());
        cx.notify();
        let model = name.clone();
        self.call(
            window,
            cx,
            move |client| client.delete_model(&model),
            move |page, result, window, cx| {
                page.busy.deleting = None;
                close_dialog(window, cx);
                match result {
                    Ok(_) => {
                        show_toast(
                            window,
                            cx,
                            Toast::new(t("models-toast-deleted")).description(tr!(
                                "models-toast-deleted-description",
                                name = page.display_name(&name)
                            )),
                        );
                        page.host.models_changed(cx);
                        page.reload(cx);
                    }
                    Err(e) => toast_error(t("models-toast-delete-failed"), e, window, cx),
                }
                cx.notify();
            },
        );
    }

    /// Opens a folder in Finder.
    fn open_folder(&self, folder: &str, window: &mut Window, cx: &mut Context<Self>) {
        let path = Path::new(folder);
        if path.exists() {
            cx.open_with_system(path);
        } else {
            show_toast(
                window,
                cx,
                Toast::new(t("models-toast-open-folder-failed")).kind(ToastKind::Destructive),
            );
        }
    }

    /// Asks for a new models folder, then confirms the move.
    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t("models-storage-picker-title").into()),
        });
        cx.spawn_in(window, async move |page, cx| {
            let folder = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(e)) => {
                    eprintln!("[models] folder picker: {e}");
                    page.update_in(cx, |_, window, cx| {
                        show_toast(
                            window,
                            cx,
                            Toast::new(t("models-toast-picker-failed"))
                                .kind(ToastKind::Destructive),
                        );
                    })
                    .ok();
                    None
                }
                Err(_) => None,
            };
            if let Some(folder) = folder {
                page.update_in(cx, |page, window, cx| page.confirm_move(folder, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn migrate(&mut self, destination: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let destination = destination.to_string_lossy().into_owned();
        self.migration = Some(Migration {
            restarting: false,
            progress: None,
        });
        cx.notify();
        self.migration_events.cancel();
        let cancel = SseCancel::new();
        self.migration_events = cancel.clone();
        let (sender, receiver) = mpsc::channel::<MigrationNews>();
        let client = self.client.clone();
        let to = destination.clone();
        std::thread::Builder::new()
            .name("kass-models-migrate".into())
            .spawn(move || follow_migration(&client, &to, &cancel, &sender))
            .ok();
        cx.spawn_in(window, async move |page, cx| {
            let ended = loop {
                cx.background_executor().timer(STREAM_CHECK).await;
                let mut ended = None;
                let mut latest = None;
                loop {
                    match receiver.try_recv() {
                        Ok(MigrationNews::Progress(progress)) => latest = Some(progress),
                        Ok(news) => {
                            ended = Some(news);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            ended = Some(MigrationNews::Failed(t(
                                "models-toast-migration-connection-lost",
                            )));
                            break;
                        }
                    }
                }
                if let Some(progress) = latest {
                    let alive = page.update(cx, |page, cx| {
                        if let Some(migration) = &mut page.migration {
                            migration.progress = Some(progress);
                        }
                        cx.notify();
                    });
                    if alive.is_err() {
                        return;
                    }
                }
                if let Some(news) = ended {
                    break news;
                }
            };
            let restart = page.update_in(cx, |page, window, cx| match ended {
                MigrationNews::Moved => {
                    if let Some(migration) = &mut page.migration {
                        migration.restarting = true;
                    }
                    cx.notify();
                    Some(page.host.set_models_dir(Some(destination), cx))
                }
                MigrationNews::NothingToMove => {
                    page.migration = None;
                    show_toast(
                        window,
                        cx,
                        Toast::new(t("models-toast-no-models-to-migrate"))
                            .description(t("models-toast-no-models-to-migrate-description")),
                    );
                    cx.notify();
                    None
                }
                MigrationNews::Failed(e) => {
                    page.migration = None;
                    toast_error(t("models-toast-migration-failed"), e, window, cx);
                    cx.notify();
                    None
                }
                MigrationNews::Progress(_) => None,
            });
            let Ok(Some(restart)) = restart else {
                return;
            };
            let result = restart.await;
            page.update_in(cx, |page, window, cx| {
                page.migration = None;
                match result {
                    Ok(()) => show_toast(window, cx, Toast::new(t("models-toast-migrated"))),
                    Err(e) => toast_error(t("models-toast-migration-failed"), e, window, cx),
                }
                page.after_server_restart(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Back to the default models folder.
    fn reset_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        show_toast(window, cx, Toast::new(t("models-toast-reset-to-default")));
        let restart = self.host.set_models_dir(None, cx);
        cx.spawn_in(window, async move |page, cx| {
            let result = restart.await;
            page.update_in(cx, |page, window, cx| {
                if let Err(e) = result {
                    toast_error(t("common-error"), e, window, cx);
                }
                page.after_server_restart(cx);
            })
            .ok();
        })
        .detach();
    }

    fn after_server_restart(&mut self, cx: &mut Context<Self>) {
        self.cache_dir = None;
        self.host.models_changed(cx);
        if self.poll.is_some() {
            self.refresh(cx);
        } else {
            self.reload(cx);
        }
        cx.notify();
    }
}

/// Reads a download's progress stream until it says how it ended.
fn follow_download(client: &ApiClient, model: &str, cancel: &SseCancel) -> Outcome {
    let Ok(stream) = client.model_progress(model, cancel) else {
        return Outcome::Lost;
    };
    for event in stream {
        let Ok(event) = event else { break };
        let Ok(progress) = event.json::<ModelProgress>() else {
            continue;
        };
        match progress.status {
            ProgressStatus::Complete => return Outcome::Done,
            ProgressStatus::Error => return Outcome::Failed(progress.error.unwrap_or_default()),
            _ => {}
        }
    }
    Outcome::Lost
}

/// Starts moving the models to `destination` and reports how it goes.
fn follow_migration(
    client: &ApiClient,
    destination: &str,
    cancel: &SseCancel,
    news: &mpsc::Sender<MigrationNews>,
) {
    let ended = match client.migrate_models(destination) {
        Err(e) => MigrationNews::Failed(e.to_string()),
        Ok(result) if result.moved == Some(0) => MigrationNews::NothingToMove,
        Ok(_) => match client.migration_progress(cancel) {
            Err(_) => MigrationNews::Failed(t("models-toast-migration-connection-lost")),
            Ok(stream) => {
                let mut ended = MigrationNews::Failed(t("models-toast-migration-connection-lost"));
                for event in stream {
                    let Ok(event) = event else { break };
                    let Ok(progress) = event.json::<ModelProgress>() else {
                        continue;
                    };
                    match progress.status {
                        ProgressStatus::Complete => {
                            ended = MigrationNews::Moved;
                            break;
                        }
                        ProgressStatus::Error => {
                            ended = MigrationNews::Failed(
                                progress
                                    .error
                                    .unwrap_or_else(|| t("models-toast-migration-failed")),
                            );
                            break;
                        }
                        _ => {
                            if news.send(MigrationNews::Progress(progress)).is_err() {
                                return;
                            }
                        }
                    }
                }
                ended
            }
        },
    };
    let _ = news.send(ended);
}

fn toast_error(title: String, error: String, window: &mut Window, cx: &mut App) {
    show_toast(
        window,
        cx,
        Toast::new(title)
            .description(error)
            .kind(ToastKind::Destructive),
    );
}
