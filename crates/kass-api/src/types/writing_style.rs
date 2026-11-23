//! Writing styles, the apps assigned to them (docs/plans/PER_APP_STYLE.md),
//! and what each has learned.

use serde::{Deserialize, Serialize};

use super::settings::PunctuationStyle;

/// Stable codes for learned punctuation habits; the app words them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritingStyleHabit {
    BoundaryPeriod,
    BoundaryComma,
    BoundaryNone,
    LowercaseStart,
    DropIntroComma,
    DropConjunctionComma,
    DropFinalPeriod,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritingStyleStatus {
    pub ready: bool,
    pub runs: u64,
    #[serde(default)]
    pub last_run_at: Option<String>,
    pub example_count: u64,
    pub habits: Vec<WritingStyleHabit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonalExampleSource {
    Correction,
    Calibration,
    #[serde(other)]
    Unknown,
}

/// One "when I say this, I mean this" example cleanup learns from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalExample {
    pub id: String,
    pub source: PersonalExampleSource,
    pub said: String,
    pub meant: String,
    #[serde(default)]
    pub created_at: Option<String>,
    /// The app a correction was made in.
    #[serde(default)]
    pub app_bundle_id: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
}

/// A named writing style that apps are assigned to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritingStyle {
    pub id: String,
    pub name: String,
    pub position: i64,
    /// The style of every app the user hasn't assigned.
    pub is_default: bool,
    pub punctuation_style: PunctuationStyle,
    pub preserve_technical: bool,
    /// How the user says they write in this style's apps; cleanup follows it.
    #[serde(default)]
    pub description: String,
}

/// An app and the style its dictation uses; `confirmed` once the user chose
/// it. `corrections` counts its captures whose corrections teach that style;
/// `suggested_style_id` is what most apps of its App Store category use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledApp {
    pub bundle_id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub style_id: String,
    pub confirmed: bool,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub corrections: u64,
    #[serde(default)]
    pub suggested_style_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritingStylesResponse {
    pub styles: Vec<WritingStyle>,
    pub apps: Vec<StyledApp>,
    pub max_styles: u64,
    /// Memory one style's cached prompt takes in the cleanup model, estimated.
    #[serde(default)]
    pub cache_mb_per_style: Option<u64>,
}

impl WritingStylesResponse {
    /// The style every app the user hasn't assigned uses.
    pub fn default_style(&self) -> Option<&WritingStyle> {
        self.styles
            .iter()
            .find(|style| style.is_default)
            .or(self.styles.first())
    }

    pub fn style(&self, id: &str) -> Option<&WritingStyle> {
        self.styles.iter().find(|style| style.id == id)
    }

    pub fn app(&self, bundle_id: &str) -> Option<&StyledApp> {
        self.apps.iter().find(|app| app.bundle_id == bundle_id)
    }

    /// The style a new app is shown in until it's confirmed: what most apps
    /// of its App Store category use, else its own, else the default.
    pub fn suggested_style(&self, app: &StyledApp) -> Option<&WritingStyle> {
        app.suggested_style_id
            .as_deref()
            .and_then(|id| self.style(id))
            .or_else(|| self.style(&app.style_id))
            .or_else(|| self.default_style())
    }

    /// Whether another style can be added.
    pub fn has_room(&self) -> bool {
        (self.styles.len() as u64) < self.max_styles
    }
}

/// What moving an app to a style takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovePlan {
    /// Already there and confirmed: nothing to do.
    Stay,
    /// Assign it, bringing any corrections along.
    Assign,
    /// It has corrections teaching another style: ask whether they come too.
    Ask,
}

impl StyledApp {
    pub fn plan_move(&self, to: &str) -> MovePlan {
        if self.style_id == to {
            if self.confirmed {
                MovePlan::Stay
            } else {
                MovePlan::Assign
            }
        } else if self.corrections > 0 {
            MovePlan::Ask
        } else {
            MovePlan::Assign
        }
    }
}

/// `PATCH /writing-styles/{id}`: only the fields that are set are sent.
/// `is_default: Some(true)` makes the style the default; false means nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WritingStyleUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub punctuation_style: Option<PunctuationStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserve_technical: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

/// What happens to an app's corrections when it moves to another style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MovedCorrections {
    #[default]
    Bring,
    Leave,
}

/// An app to keep in the style it already uses (`POST /writing-styles/apps/confirm`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppToConfirm {
    pub bundle_id: String,
    pub app_name: Option<String>,
}

impl From<&StyledApp> for AppToConfirm {
    fn from(app: &StyledApp) -> Self {
        Self {
            bundle_id: app.bundle_id.clone(),
            app_name: app.name.clone(),
        }
    }
}

/// One rule summarized from the user's older examples.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionNote {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionNotesStatus {
    pub notes: Vec<CorrectionNote>,
    /// Examples that left the prompt and have not been summarized yet.
    pub pending: u64,
    #[serde(default)]
    pub last_run: Option<String>,
    pub outcome: String,
}
