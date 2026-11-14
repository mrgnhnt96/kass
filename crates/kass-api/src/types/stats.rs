//! `GET /captures/stats`, for the Captures card and Insights.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UsagePeriod {
    #[serde(rename = "today")]
    Today,
    #[serde(rename = "7d")]
    Days7,
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "all")]
    All,
    #[serde(other)]
    Unknown,
}

impl UsagePeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::All | Self::Unknown => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageBucket {
    Hour,
    Day,
    Week,
    #[serde(other)]
    Unknown,
}

/// Words dictated on one local day ("2026-09-28").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDay {
    pub date: String,
    pub words: u64,
}

/// One period's dictation, for all apps or one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageTotals {
    pub words: u64,
    pub captures: u64,
    pub speaking_ms: u64,
    /// Median pace of captures of 2 s and longer; None when there are none.
    pub pace_wpm: Option<u64>,
    /// Typing time the words would have taken, less the time spent speaking.
    pub time_saved_ms: i64,
    /// Captures with a saved correction.
    pub fixed_captures: u64,
    pub weekdays_dictated: u64,
    pub weekdays_in_period: u64,
    pub weekend_days: Vec<UsageDay>,
    pub hours_dictated: u64,
}

/// One point of the words chart, at a local hour ("2026-09-28T14:00"), day
/// or week start. `words` is None for an hour still to come today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsagePoint {
    pub start: String,
    #[serde(default)]
    pub words: Option<u64>,
    #[serde(default)]
    pub previous_start: Option<String>,
    #[serde(default)]
    pub previous_words: Option<u64>,
}

/// One app's words in the period; a None bundle id is no app recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageApp {
    #[serde(default)]
    pub app_bundle_id: Option<String>,
    pub words: u64,
    pub captures: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLengths {
    pub bin_edges_s: Vec<u64>,
    pub counts: Vec<u64>,
    #[serde(default)]
    pub p25_ms: Option<u64>,
    #[serde(default)]
    pub median_ms: Option<u64>,
    #[serde(default)]
    pub p75_ms: Option<u64>,
}

/// In the Mac's local days. `current` and everything after it follow the
/// app filter; `all_apps` and `apps` are always every app's. `heatmap` is
/// words by weekday (Monday first) and hour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatsResponse {
    pub period: UsagePeriod,
    pub bucket: UsageBucket,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub previous_start: Option<String>,
    #[serde(default)]
    pub previous_end: Option<String>,
    pub typing_wpm: u64,
    pub current: UsageTotals,
    #[serde(default)]
    pub previous: Option<UsageTotals>,
    pub all_apps: UsageTotals,
    pub series: Vec<UsagePoint>,
    pub apps: Vec<UsageApp>,
    pub heatmap: Vec<Vec<u64>>,
    pub lengths: UsageLengths,
}
