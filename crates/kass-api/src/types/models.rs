//! Speech and cleanup models: status, downloads and the storage folder.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressStatus {
    #[default]
    Downloading,
    Extracting,
    Complete,
    Error,
    #[serde(other)]
    Unknown,
}

/// An event of `/models/progress/{name}` or `/models/migrate/progress`
/// (where `model_name` is `migration` and the counts are bytes moved).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelProgress {
    pub model_name: String,
    pub current: f64,
    pub total: f64,
    /// 0 to 100.
    pub progress: f64,
    #[serde(default)]
    pub filename: Option<String>,
    pub status: ProgressStatus,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl ModelProgress {
    /// The stream sends nothing more after this.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            ProgressStatus::Complete | ProgressStatus::Error
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStatus {
    pub model_name: String,
    pub display_name: String,
    /// The Hugging Face repository.
    #[serde(default)]
    pub hf_repo_id: Option<String>,
    pub downloaded: bool,
    #[serde(default)]
    pub downloading: bool,
    #[serde(default)]
    pub size_mb: Option<f64>,
    #[serde(default)]
    pub loaded: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStatusListResponse {
    pub models: Vec<ModelStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDownloadRequest {
    pub model_name: String,
}

/// `POST /models/migrate`. Progress follows on `/models/migrate/progress`;
/// `moved` and `errors` come only when there was nothing to move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateResult {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub moved: Option<u64>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveDownloadTask {
    pub model_name: String,
    pub status: String,
    pub started_at: String,
    #[serde(default)]
    pub error: Option<String>,
    /// 0 to 100.
    #[serde(default)]
    pub progress: Option<f64>,
    /// Bytes downloaded.
    #[serde(default)]
    pub current: Option<u64>,
    #[serde(default)]
    pub total: Option<u64>,
    /// The file being downloaded.
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveTasksResponse {
    pub downloads: Vec<ActiveDownloadTask>,
}

/// A model as the Hugging Face API lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HuggingFaceModelInfo {
    pub id: String,
    pub author: String,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
    #[serde(default)]
    pub pipeline_tag: Option<String>,
    #[serde(default)]
    pub library_name: Option<String>,
    pub downloads: u64,
    pub likes: u64,
    pub tags: Vec<String>,
    #[serde(rename = "cardData", default)]
    pub card_data: Option<HuggingFaceCardData>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HuggingFaceCardData {
    pub license: Option<String>,
    pub language: Option<Vec<String>>,
    pub pipeline_tag: Option<String>,
}
