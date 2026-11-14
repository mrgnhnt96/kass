//! Reports on captures and the learning that comes from them.

use super::ApiClient;
use crate::encode::component;
use crate::error::Result;
use crate::types::{CaptureFeedbackCreate, CaptureFeedbackResponse, CorrectionLearningStatus};

impl ApiClient {
    pub fn report_capture_output(
        &self,
        capture_id: &str,
        report: &CaptureFeedbackCreate,
    ) -> Result<CaptureFeedbackResponse> {
        self.send(
            "POST",
            &format!("/captures/{}/feedback", component(capture_id)),
            report,
        )
    }

    /// Withdraws one report and everything it taught.
    pub fn withdraw_capture_report(&self, capture_id: &str, report_id: &str) -> Result<()> {
        let path = format!(
            "/captures/{}/feedback/{}",
            component(capture_id),
            component(report_id)
        );
        self.run("DELETE", &path)
    }

    pub fn list_capture_feedback(&self, capture_id: &str) -> Result<Vec<CaptureFeedbackResponse>> {
        self.fetch(
            "GET",
            &format!("/captures/{}/feedback", component(capture_id)),
        )
    }

    pub fn export_capture_feedback(&self) -> Result<Vec<CaptureFeedbackResponse>> {
        self.fetch("GET", "/capture/feedback/export")
    }

    pub fn correction_learning_status(&self) -> Result<CorrectionLearningStatus> {
        self.fetch("GET", "/capture/learning")
    }

    pub fn run_correction_learning(&self) -> Result<CorrectionLearningStatus> {
        self.fetch("POST", "/capture/learning/run")
    }

    /// Hold off model learning while the user records.
    pub fn pause_learning_for_recording(&self) -> Result<()> {
        self.run("POST", "/capture/learning/activity")
    }

    pub fn cancel_model_learning(&self) -> Result<()> {
        self.run("POST", "/capture/learning/cancel")
    }

    pub fn rollback_correction_learning(&self) -> Result<CorrectionLearningStatus> {
        self.fetch("POST", "/capture/learning/rollback")
    }

    pub fn download_voice_sounds(&self) -> Result<()> {
        self.run("POST", "/capture/learning/voice/sounds")
    }
}
