use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WhisperModelSize {
    Base,
    Small,
    Medium,
    Large,
    Turbo,
    #[serde(other)]
    Unknown,
}

impl WhisperModelSize {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Turbo => "turbo",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Qwen3ModelSize {
    #[serde(rename = "0.6B")]
    Size0_6B,
    #[serde(rename = "1.7B")]
    Size1_7B,
    #[serde(rename = "4B")]
    Size4B,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PunctuationStyle {
    Standard,
    Casual,
    Learned,
    #[serde(other)]
    Unknown,
}

/// Days of capture history to keep; `Forever` (0) keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "u16", into = "u16")]
pub enum HistoryRetentionDays {
    Forever,
    Days7,
    Days30,
    Days90,
    Days365,
    /// A value this client doesn't know.
    Other(u16),
}

impl HistoryRetentionDays {
    pub fn days(self) -> u16 {
        self.into()
    }
}

impl From<u16> for HistoryRetentionDays {
    fn from(days: u16) -> Self {
        match days {
            0 => Self::Forever,
            7 => Self::Days7,
            30 => Self::Days30,
            90 => Self::Days90,
            365 => Self::Days365,
            other => Self::Other(other),
        }
    }
}

impl From<HistoryRetentionDays> for u16 {
    fn from(days: HistoryRetentionDays) -> Self {
        match days {
            HistoryRetentionDays::Forever => 0,
            HistoryRetentionDays::Days7 => 7,
            HistoryRetentionDays::Days30 => 30,
            HistoryRetentionDays::Days90 => 90,
            HistoryRetentionDays::Days365 => 365,
            HistoryRetentionDays::Other(days) => days,
        }
    }
}

/// A saved Command Mode instruction, run by saying its name or from ⌘K.
/// A new one has no id until the server gives it one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub instruction: String,
}

/// `GET /settings/captures`: the server's defaults for capturing and cleanup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureSettings {
    pub stt_model: WhisperModelSize,
    pub language: String,
    pub auto_refine: bool,
    pub llm_model: Qwen3ModelSize,
    pub smart_cleanup: bool,
    pub self_correction: bool,
    pub preserve_technical: bool,
    pub punctuation_style: PunctuationStyle,
    pub allow_auto_paste: bool,
    /// Type cleaned text into the app while cleanup is still writing it.
    pub live_text: bool,
    /// Fix the last dictation by voice ("fix that, Morgan not Megan"). Beta.
    pub voice_edits: bool,
    /// Write "!" and drawn-out words ("wayyy") from how something was said.
    pub expressive: bool,
    /// Chime when dictation starts, stops or fails.
    pub sound_cues: bool,
    /// Chime volume, 0 to 1.
    pub sound_cue_volume: f64,
    /// None or empty is the system's default microphone.
    #[serde(default)]
    pub input_device_id: Option<String>,
    pub hotkey_enabled: bool,
    /// keytap key names.
    pub chord_push_to_talk_keys: Vec<String>,
    pub chord_toggle_to_talk_keys: Vec<String>,
    /// Empty turns Command Mode off.
    pub chord_command_keys: Vec<String>,
    pub command_transforms: Vec<Transform>,
    /// Older captures are deleted; what they taught is kept.
    pub history_retention_days: HistoryRetentionDays,
    /// Set by saving `history_retention_days`; nothing is deleted until then.
    pub history_retention_confirmed: bool,
    pub onboarding_completed: bool,
    /// Delete each recording once its transcript is saved.
    pub discard_audio: bool,
    /// Send anonymous daily usage counts.
    pub share_usage: bool,
    /// Start from the cleanup and voice models Kass ships.
    pub shared_adapters: bool,
}

/// `PUT /settings/captures`: only the fields that are set are sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CaptureSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_model: Option<WhisperModelSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_refine: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<Qwen3ModelSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart_cleanup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_correction: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserve_technical: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub punctuation_style: Option<PunctuationStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_auto_paste: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_edits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expressive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_cues: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_cue_volume: Option<f64>,
    /// `Some(None)` goes back to the default microphone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_device_id: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotkey_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chord_push_to_talk_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chord_toggle_to_talk_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chord_command_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_transforms: Option<Vec<Transform>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_retention_days: Option<HistoryRetentionDays>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_retention_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboarding_completed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discard_audio: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_usage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_adapters: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionStatus {
    pub days: HistoryRetentionDays,
    pub confirmed: bool,
    /// Captures the current window would delete once confirmed.
    pub expiring: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPreview {
    pub days: HistoryRetentionDays,
    /// Captures that keeping `days` would delete now.
    pub expiring: u64,
}

/// One row of the dictation readiness checklist. `model_name` is what
/// `POST /models/download` takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelReadiness {
    pub ready: bool,
    pub model_name: String,
    pub display_name: String,
    pub size: String,
    #[serde(default)]
    pub size_mb: Option<u64>,
}

/// The server's half of the readiness check; the app adds its permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureReadinessResponse {
    pub stt: ModelReadiness,
    pub llm: ModelReadiness,
}
