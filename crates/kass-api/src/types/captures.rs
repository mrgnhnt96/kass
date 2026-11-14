use serde::{Deserialize, Serialize};

use super::settings::{PunctuationStyle, Qwen3ModelSize, WhisperModelSize};

/// `Command`: a Command Mode rewrite of selected text (docs/plans/COMMAND_MODE.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSource {
    Dictation,
    Recording,
    File,
    Command,
    #[serde(other)]
    Unknown,
}

impl CaptureSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dictation => "dictation",
            Self::Recording => "recording",
            Self::File => "file",
            Self::Command => "command",
            Self::Unknown => "unknown",
        }
    }
}

/// The accessibility-focused element when dictation started (from the
/// app's `dictate:start` payload, not the server).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusSnapshot {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub role: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementFlags {
    pub smart_cleanup: bool,
    pub self_correction: bool,
    pub preserve_technical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub punctuation_style: Option<PunctuationStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capitalize_first: Option<bool>,
    /// The writing style whose habits, examples and rules cleanup used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewOutcome {
    /// The cleanup was kept.
    Review,
    /// The transcript was used instead.
    Reject,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewReason {
    Answered,
    Dropped,
    Negation,
    Number,
    Person,
    Technical,
    #[serde(other)]
    Unknown,
}

/// Why a capture's cleanup is flagged for the user to check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementReview {
    pub outcome: ReviewOutcome,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub reasons: Vec<ReviewReason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub id: String,
    pub audio_path: String,
    pub source: CaptureSource,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    pub transcript_raw: String,
    #[serde(default)]
    pub transcript_refined: Option<String>,
    #[serde(default)]
    pub stt_model: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub refinement_flags: Option<RefinementFlags>,
    #[serde(default)]
    pub refinement_review: Option<RefinementReview>,
    /// The app dictated into, when the capture came from the shortcut.
    #[serde(default)]
    pub app_bundle_id: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
    /// Command captures: the selected text, the instruction that ran and
    /// the transform's name. The rewrite is `transcript_refined`.
    #[serde(default)]
    pub command_selection: Option<String>,
    #[serde(default)]
    pub command_instruction: Option<String>,
    #[serde(default)]
    pub command_transform: Option<String>,
    /// The writing style the capture was cleaned up with.
    #[serde(default)]
    pub style_id: Option<String>,
    /// The recording was deleted once the transcript was saved.
    #[serde(default)]
    pub audio_deleted: bool,
    /// The Mac that dictated it, for a Kass shared over the network.
    #[serde(default)]
    pub mac_id: Option<String>,
    #[serde(default)]
    pub mac_name: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureListResponse {
    pub items: Vec<CaptureResponse>,
    pub total: u64,
}

/// `POST /captures`: the capture, and the server's settings when it was
/// made, so the caller knows whether to refine and paste.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureCreateResponse {
    #[serde(flatten)]
    pub capture: CaptureResponse,
    pub auto_refine: bool,
    pub allow_auto_paste: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CaptureRefineRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<RefinementFlags>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_size: Option<Qwen3ModelSize>,
}

/// The form fields of `POST /captures` besides the file. No source sends `file`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureCreateOptions {
    pub source: Option<CaptureSource>,
    pub language: Option<String>,
    pub stt_model: Option<WhisperModelSize>,
}

/// One app in the Captures app list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureAppCount {
    pub app_bundle_id: String,
    #[serde(default)]
    pub app_name: Option<String>,
    pub count: u64,
    #[serde(default)]
    pub last_captured_at: Option<String>,
    /// The style the app's dictation uses; the default until the user chooses.
    #[serde(default)]
    pub style_id: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub suggested_style_id: Option<String>,
}

/// `GET /captures/apps`. `unknown_count` is the captures with no app:
/// uploads, and dictation from before apps were saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureAppsResponse {
    pub total: u64,
    pub unknown_count: u64,
    pub apps: Vec<CaptureAppCount>,
}

/// Which captures to list: every app's, one app's, or those with no app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CaptureAppFilter {
    #[default]
    All,
    App(String),
    Unknown,
}

impl CaptureAppFilter {
    pub(crate) fn query_pair(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::All => None,
            Self::App(bundle_id) => Some(("app_bundle_id", bundle_id)),
            Self::Unknown => Some(("unknown_app", "true")),
        }
    }
}

/// An audio file for `POST /captures`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureUpload {
    pub file_name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl CaptureUpload {
    pub fn wav(file_name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            file_name: file_name.into(),
            content_type: "audio/wav".into(),
            bytes,
        }
    }
}
