//! Reports on captures and what correction learning made of them
//! (docs/plans/CORRECTION_LEARNING.md). The learning status is built from
//! plain dicts on the server, so every field has a default.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::captures::CaptureResponse;

/// How a report was made; a redictation teaches correction learning only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFeedbackSource {
    #[default]
    Manual,
    VoiceFix,
    Redictation,
    #[serde(other)]
    Unknown,
}

/// Which text a report corrects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackTarget {
    Raw,
    Refined,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaptureFeedbackCreate {
    pub target: FeedbackTarget,
    pub expected_text: String,
    pub notes: String,
    pub snapshot: CaptureResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<CaptureFeedbackSource>,
    /// A report of the same capture and target this one amends; it is replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureFeedbackResponse {
    pub id: String,
    pub capture_id: String,
    pub target: FeedbackTarget,
    pub expected_text: String,
    pub notes: String,
    pub snapshot: CaptureResponse,
    pub source: CaptureFeedbackSource,
    #[serde(default)]
    pub replaces: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOutcome {
    #[default]
    Waiting,
    Updated,
    NoChange,
    RolledBack,
    #[serde(other)]
    Unknown,
}

/// `GET /capture/learning`: the correction rules, and the personal models
/// trained from corrections and the user's voice (`model`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorrectionLearningStatus {
    pub model: Option<ModelLearningStatus>,
    pub evaluated_report_ids: Vec<String>,
    pub revision: i64,
    pub active_rules: u64,
    pub last_run: Option<String>,
    pub outcome: LearningOutcome,
    pub can_rollback: bool,
    pub metrics: Option<RuleMetrics>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleMetrics {
    pub examples: u64,
    pub candidates: u64,
    pub accepted: u64,
    pub withdrawn: u64,
    pub median_rule_ms: f64,
    pub latency_passed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelLearningStatus {
    pub phase: String,
    pub revision: i64,
    pub running: bool,
    pub active_adapter: Option<String>,
    pub speech_model: Option<String>,
    pub can_rollback: bool,
    /// Examples per split (`train`, `validation`, `test`, `audio_test`, `speech_test`).
    pub counts: BTreeMap<String, u64>,
    pub last_run: Option<String>,
    pub error: Option<String>,
    pub metrics: Option<ModelMetrics>,
    pub voice: Option<VoiceTrainingStatus>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelMetrics {
    pub adapter: Option<AdapterMetrics>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdapterMetrics {
    pub passed: bool,
    pub reasons: Vec<String>,
    pub baseline_errors: f64,
    pub candidate_errors: f64,
}

/// Mistakes per group of test recordings (voice_training/gate.py).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceErrors {
    pub clean: f64,
    pub noisy: f64,
    pub correction: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceTrainingStatus {
    pub active: Option<String>,
    pub active_since: Option<String>,
    pub can_undo: bool,
    pub min_train: u64,
    pub min_test: u64,
    pub bank: VoiceBank,
    pub sounds: VoiceSounds,
    pub metrics: Option<VoiceMetrics>,
    pub training: Option<VoiceTraining>,
}

/// The recordings voice training has.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceBank {
    pub takes: u64,
    pub train: u64,
    pub test: u64,
    pub minutes: f64,
    pub room_minutes: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SoundsState {
    #[default]
    Missing,
    Downloading,
    Ready,
    Failed,
    #[serde(other)]
    Unknown,
}

/// The background sounds voice training mixes in, downloaded once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSounds {
    pub state: SoundsState,
    pub fraction: f64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceMetrics {
    pub passed: bool,
    pub reasons: Vec<String>,
    pub takes: u64,
    pub base: Option<VoiceErrors>,
    pub production: Option<VoiceErrors>,
    pub candidate: Option<VoiceErrors>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceTraining {
    pub updates: u64,
    pub seconds: f64,
    pub train_takes: u64,
}
