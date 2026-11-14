//! Words dictation should get right, each in one or more places
//! (docs/plans/DICTIONARIES.md).

use serde::{Deserialize, Serialize};

/// Where an entry applies. Most specific wins: app, then style, then everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictionaryScope {
    Global,
    Style,
    App,
    #[serde(other)]
    Unknown,
}

/// One place an entry applies: everywhere, a writing style (`scope_id` its
/// id) or an app (its bundle id).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryPlace {
    pub scope: DictionaryScope,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DictionaryPlaceInput {
    pub scope: DictionaryScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
}

impl DictionaryPlaceInput {
    pub fn global() -> Self {
        Self {
            scope: DictionaryScope::Global,
            scope_id: None,
            app_name: None,
        }
    }

    pub fn style(style_id: impl Into<String>) -> Self {
        Self {
            scope: DictionaryScope::Style,
            scope_id: Some(style_id.into()),
            app_name: None,
        }
    }

    pub fn app(bundle_id: impl Into<String>, app_name: Option<String>) -> Self {
        Self {
            scope: DictionaryScope::App,
            scope_id: Some(bundle_id.into()),
            app_name,
        }
    }
}

impl From<&DictionaryPlace> for DictionaryPlaceInput {
    fn from(place: &DictionaryPlace) -> Self {
        Self {
            scope: place.scope,
            scope_id: place.scope_id.clone(),
            app_name: place.app_name.clone(),
        }
    }
}

/// Who added an entry: the user, or Kass from a word they spelled aloud or
/// corrected. Editing makes it the user's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictionarySource {
    #[default]
    User,
    SpokenFix,
    Correction,
    #[serde(other)]
    Unknown,
}

/// A word dictation should get right. A spelling has only `written`; a
/// replacement also has `spoken`, what the user says to get `written`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub id: String,
    pub written: String,
    #[serde(default)]
    pub spoken: Option<String>,
    pub places: Vec<DictionaryPlace>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Off: only fixed where spelled exactly, never swapped in for a word
    /// that sounds like it.
    #[serde(default = "yes")]
    pub match_sound: bool,
    #[serde(default)]
    pub source: DictionarySource,
    /// A phrase: `written` is inserted exactly, line breaks and all, where
    /// `spoken` is said.
    #[serde(default)]
    pub phrase: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryListResponse {
    /// Newest first.
    pub entries: Vec<DictionaryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DictionaryEntryCreate {
    pub written: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spoken: Option<String>,
    /// At least one; with `Global`, the server keeps only that.
    pub places: Vec<DictionaryPlaceInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_sound: Option<bool>,
    /// Needs `spoken`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phrase: Option<bool>,
}

/// `PATCH /dictionary/{id}`: only the fields that are set are sent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DictionaryEntryUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<String>,
    /// `Some(None)` makes a replacement a plain spelling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spoken: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub places: Option<Vec<DictionaryPlaceInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_sound: Option<bool>,
}

/// One place of an entry that applies in an app; `overridden` when a more
/// specific entry wins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDictionaryEntry {
    pub id: String,
    pub written: String,
    #[serde(default)]
    pub spoken: Option<String>,
    pub scope: DictionaryScope,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default = "yes")]
    pub match_sound: bool,
    #[serde(default)]
    pub phrase: bool,
    #[serde(default)]
    pub overridden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDictionaryResponse {
    /// Most specific first: the app's own, its style's, then everywhere's.
    pub entries: Vec<ResolvedDictionaryEntry>,
    /// Terms given to speech-to-text as a hint.
    pub prompt_terms: Vec<String>,
    /// Terms that did not fit in the speech-to-text prompt.
    pub dropped_terms: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deleted {
    pub deleted: bool,
}

fn yes() -> bool {
    true
}
