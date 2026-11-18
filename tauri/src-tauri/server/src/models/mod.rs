//! The model files: downloading them from Hugging Face, measuring, deleting
//! and moving them (`routes::models`), and the downloads in flight
//! (`routes::tasks`). Which models are loaded stays the worker's to say
//! (`worker::loaded_models`, `load_model`, `unload_model`).
//!
//! Downloads the worker starts itself, when a dictation needs a model that
//! isn't there yet, are still its own and don't show here.

pub mod cache;
pub mod download;
pub mod migrate;
pub mod progress;
pub mod registry;

use std::path::{Path, PathBuf};

use crate::proxy::Proxy;
use crate::worker;
use download::Hub;
use progress::Progress;
use registry::Model;

/// The models loaded now; none when the worker can't say.
pub async fn loaded(proxy: &Proxy) -> Vec<String> {
    worker::loaded_models(proxy)
        .await
        .map_err(|error| eprintln!("[server] couldn't ask the model worker what's loaded: {error}"))
        .unwrap_or_default()
}

/// The size of the `engine` model loaded now (`turbo`, `4B`), as the
/// routes take it.
pub async fn loaded_size(proxy: &Proxy, engine: registry::Engine) -> Option<&'static str> {
    let loaded = loaded(proxy).await;
    registry::MODELS
        .iter()
        .find(|model| model.engine == engine && loaded.iter().any(|name| name == model.name))
        .map(|model| model.size)
}

#[derive(Clone)]
pub struct Models {
    cache: PathBuf,
    hub: Hub,
    pub progress: Progress,
}

impl Models {
    /// `chosen` is the folder the app gave the worker (`KASS_MODELS_DIR`).
    pub fn new(chosen: Option<&Path>) -> Self {
        Self {
            cache: cache::resolve(chosen),
            hub: Hub::from_env(),
            progress: Progress::default(),
        }
    }

    /// Where models download to.
    pub fn cache(&self) -> &Path {
        &self.cache
    }

    /// Download `model` in the background, then have the worker load it, as
    /// loading it there used to download it. It's in `/tasks/active` until
    /// it's loaded, or with its error if it failed.
    pub fn download(&self, proxy: &Proxy, model: &'static Model) {
        let name = model.name;
        let id = self.progress.start_task(name);
        self.progress.update(
            name,
            0,
            0,
            Some("Connecting to HuggingFace..."),
            "downloading",
        );
        let (models, proxy) = (self.clone(), proxy.clone());
        let running = tokio::spawn(async move {
            let progress = models.progress.clone();
            let report = move |current: u64, total: u64, file: &str| {
                progress.update(name, current, total, Some(file), "downloading");
            };
            let result = match download::fetch(&models.hub, &models.cache, model, &report).await {
                Ok(()) => worker::load_model(&proxy, name).await,
                Err(error) => Err(error),
            };
            if !models.progress.is_current(name, id) {
                return;
            }
            match result {
                Ok(()) => {
                    models.progress.mark_complete(name);
                    models.progress.complete_task(name, id);
                }
                Err(error) => {
                    eprintln!("[server] downloading {name} failed: {error}");
                    models.progress.mark_error(name, &error);
                    models.progress.fail_task(name, id, &error);
                }
            }
        });
        self.progress.attach(name, id, running.abort_handle());
    }

    /// Download `model` without loading it or showing it in `/tasks`: a
    /// stand-in readying the models a shared Kass dictates with
    /// (`sync::job`).
    pub async fn fetch(&self, model: &'static Model) -> Result<(), String> {
        let quiet = |_: u64, _: u64, _: &str| {};
        download::fetch(&self.hub, &self.cache, model, &quiet).await
    }

    /// Whether `model` can run now: loaded, or its files on disk. When
    /// neither, its download starts (the one running carries on) and it's
    /// false, for a route to answer that it's being downloaded, as
    /// Python's one-shot routes did.
    pub async fn ready_or_fetch(&self, proxy: &Proxy, model: &'static Model) -> bool {
        let cache = self.cache.clone();
        let repo = model.repo;
        let on_disk = tokio::task::spawn_blocking(move || cache::on_disk(&cache, repo).downloaded)
            .await
            .unwrap_or(false);
        if on_disk || loaded(proxy).await.iter().any(|name| name == model.name) {
            return true;
        }
        if !self.progress.is_running(model.name) {
            self.download(proxy, model);
        }
        false
    }

    /// Stop a model's download and forget its progress; true when there
    /// was either. What it fetched stays, to resume from.
    pub fn cancel(&self, name: &str) -> bool {
        let removed = self.progress.cancel_task(name);
        self.progress.forget(name) || removed
    }
}
