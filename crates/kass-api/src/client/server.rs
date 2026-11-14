//! Health, background tasks, Command Mode, and syncing with another Mac.

use serde::Deserialize;
use serde_json::json;

use super::ApiClient;
use crate::address::LOCAL_SERVER_URL;
use crate::error::Result;
use crate::types::{
    ActiveTasksResponse, CaptureResponse, FilesystemHealthResponse, HealthResponse, Message,
    SyncStatus,
};

impl ApiClient {
    pub fn health(&self) -> Result<HealthResponse> {
        self.fetch("GET", "/health")
    }

    /// Whether this is an internal build of the server (`GET /internal`).
    pub fn internal(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct Internal {
            internal: bool,
        }
        self.fetch::<Internal>("GET", "/internal")
            .map(|body| body.internal)
    }

    pub fn filesystem_health(&self) -> Result<FilesystemHealthResponse> {
        self.fetch("GET", "/health/filesystem")
    }

    /// Rewrite `selection` by a spoken or typed instruction (Command Mode
    /// without a recording).
    pub fn run_command(&self, selection: &str, instruction: &str) -> Result<CaptureResponse> {
        self.send(
            "POST",
            "/commands/run",
            &json!({ "selection": selection, "instruction": instruction }),
        )
    }

    pub fn active_tasks(&self) -> Result<ActiveTasksResponse> {
        self.fetch("GET", "/tasks/active")
    }

    pub fn clear_all_tasks(&self) -> Result<Message> {
        self.fetch("POST", "/tasks/clear")
    }

    /// Tell this Mac's own server, whatever server the client uses, that
    /// the other Mac is back so it hands over what it did meanwhile.
    pub fn local_sync_wake(&self) -> Result<()> {
        self.call_url("POST", &format!("{LOCAL_SERVER_URL}/sync/wake"), None)
            .map(drop)
    }

    /// Whether this Mac's own server is standing in for the other Mac.
    pub fn local_sync_status(&self) -> Result<SyncStatus> {
        super::decode(self.call_url("GET", &format!("{LOCAL_SERVER_URL}/sync/status"), None)?)
    }
}
