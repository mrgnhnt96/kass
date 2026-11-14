//! Capture settings, readiness and history retention.

use super::ApiClient;
use crate::error::Result;
use crate::types::{
    CaptureReadinessResponse, CaptureSettings, CaptureSettingsUpdate, HistoryRetentionDays,
    RetentionPreview, RetentionStatus,
};

impl ApiClient {
    pub fn capture_settings(&self) -> Result<CaptureSettings> {
        self.fetch("GET", "/settings/captures")
    }

    pub fn capture_readiness(&self) -> Result<CaptureReadinessResponse> {
        self.fetch("GET", "/capture/readiness")
    }

    pub fn update_capture_settings(
        &self,
        patch: &CaptureSettingsUpdate,
    ) -> Result<CaptureSettings> {
        self.send("PUT", "/settings/captures", patch)
    }

    pub fn retention_status(&self) -> Result<RetentionStatus> {
        self.fetch("GET", "/settings/captures/retention-status")
    }

    /// What keeping history for `days` would delete.
    pub fn retention_preview(&self, days: HistoryRetentionDays) -> Result<RetentionPreview> {
        self.fetch(
            "GET",
            &format!("/settings/captures/retention-preview?days={}", days.days()),
        )
    }
}
