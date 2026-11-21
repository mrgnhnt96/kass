//! The server itself: health, its folders, and syncing with another Mac.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// `healthy` when it's up.
    pub status: String,
    /// Whether a Whisper model is loaded.
    pub model_loaded: bool,
    /// Whether the configured Whisper model is downloaded.
    #[serde(default)]
    pub model_downloaded: Option<bool>,
    #[serde(default)]
    pub model_size: Option<String>,
    pub gpu_available: bool,
    #[serde(default)]
    pub gpu_type: Option<String>,
    #[serde(default)]
    pub backend_type: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// Server process start, Unix seconds.
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// Peak resident memory of the server process.
    #[serde(default)]
    pub peak_memory_mb: Option<u64>,
    /// The id of the Mac the server runs on, which stays when its network
    /// name changes. None from an older Kass.
    #[serde(default)]
    pub mac_id: Option<String>,
}

/// `GET /health/filesystem`: whether the server's folders are there and writable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilesystemHealthResponse {
    pub healthy: bool,
    #[serde(default)]
    pub disk_free_mb: Option<f64>,
    #[serde(default)]
    pub disk_total_mb: Option<f64>,
    pub directories: Vec<DirectoryCheck>,
}

impl FilesystemHealthResponse {
    /// The captures folder, as the Captures settings page shows it.
    pub fn captures_dir(&self) -> Option<&str> {
        self.directories
            .iter()
            .map(|dir| dir.path.as_str())
            .find(|path| path.contains("captures"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryCheck {
    pub path: String,
    pub exists: bool,
    pub writable: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// `GET /sync/status` on this Mac's server, while using another Mac's
/// (beta `network`). Only while it `standing_in` are the rest set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub standing_in: bool,
    /// Changes made here that the other Mac doesn't have yet.
    #[serde(default)]
    pub pending: Option<u64>,
    /// Models this Mac is downloading, to stand in without waiting.
    #[serde(default)]
    pub downloading: Vec<String>,
}

/// A response that's only a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub message: String,
}
