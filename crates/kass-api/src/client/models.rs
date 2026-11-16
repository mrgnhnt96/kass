//! Model downloads, the models folder and their progress streams.

use serde::Deserialize;
use serde_json::json;

use super::ApiClient;
use crate::encode::component;
use crate::error::Result;
use crate::sse::{SseCancel, SseStream};
use crate::types::{Message, MigrateResult, ModelDownloadRequest, ModelStatusListResponse};

impl ApiClient {
    pub fn model_status(&self) -> Result<ModelStatusListResponse> {
        self.fetch("GET", "/models/status")
    }

    /// The folder models download to.
    pub fn models_cache_dir(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct CacheDir {
            path: String,
        }
        self.fetch::<CacheDir>("GET", "/models/cache-dir")
            .map(|body| body.path)
    }

    /// Move the models folder; progress follows on `migration_progress`.
    pub fn migrate_models(&self, destination: &str) -> Result<MigrateResult> {
        self.send(
            "POST",
            "/models/migrate",
            &json!({ "destination": destination }),
        )
    }

    pub fn migration_progress_url(&self) -> String {
        self.url("/models/migrate/progress")
    }

    /// Progress of `migrate_models`, as `ModelProgress` events.
    pub fn migration_progress(&self, cancel: &SseCancel) -> Result<SseStream> {
        self.events(&self.migration_progress_url(), cancel)
    }

    pub fn trigger_model_download(&self, model_name: &str) -> Result<Message> {
        self.send("POST", "/models/download", &download(model_name))
    }

    pub fn cancel_download(&self, model_name: &str) -> Result<Message> {
        self.send("POST", "/models/download/cancel", &download(model_name))
    }

    /// A model's download progress, as `ModelProgress` events.
    pub fn model_progress(&self, model_name: &str, cancel: &SseCancel) -> Result<SseStream> {
        self.events(
            &self.url(&format!("/models/progress/{}", component(model_name))),
            cancel,
        )
    }

    pub fn delete_model(&self, model_name: &str) -> Result<Message> {
        self.fetch("DELETE", &format!("/models/{}", component(model_name)))
    }

    pub fn unload_model(&self, model_name: &str) -> Result<Message> {
        self.fetch("POST", &format!("/models/{}/unload", component(model_name)))
    }

    /// Free MB on the volume models download to.
    pub fn models_disk_space_mb(&self) -> Result<f64> {
        #[derive(Deserialize)]
        struct DiskSpace {
            free_mb: f64,
        }
        self.fetch::<DiskSpace>("GET", "/models/disk-space")
            .map(|body| body.free_mb)
    }
}

fn download(model_name: &str) -> ModelDownloadRequest {
    ModelDownloadRequest {
        model_name: model_name.to_string(),
    }
}
