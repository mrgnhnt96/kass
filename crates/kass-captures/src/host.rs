//! What the Captures page asks of the app around it: the things only the
//! app can do (fix text where it was typed, look up app icons, open other
//! screens). Which Mac this is comes from the API client, and which others
//! are connected from the page's own event stream. Every method has a
//! default that does nothing, so the page runs on its own, as in the example.

use gpui_kit::{App, Image, Task, Window};
use kass_api::types::{CaptureAppFilter, UsagePeriod};
use std::sync::Arc;

/// A screen the page links to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Finishing setup: models and permissions.
    Setup,
    /// Writing Style settings, on a style when one is given.
    WritingStyle(Option<String>),
    CommandMode,
    /// The dictionary's phrases.
    DictionaryPhrases,
    DictationSettings,
    /// Insights, showing these apps over this period.
    Insights {
        filter: CaptureAppFilter,
        period: UsagePeriod,
    },
}

pub trait CapturesHost {
    /// Fixes the text where Kass typed it, if it still reads `before` there.
    /// `Some(app)` when it changed it (an empty name when the app is
    /// unknown); `None` when it didn't, or holds it until the app is back.
    fn apply_correction(
        &self,
        _capture_id: String,
        _before: String,
        _after: String,
        _cx: &mut App,
    ) -> Task<Option<String>> {
        Task::ready(None)
    }

    /// The icon of an installed app, by bundle id.
    fn app_icon(&self, _bundle_id: &str) -> Option<Arc<Image>> {
        None
    }

    fn navigate(&self, _route: Route, _window: &mut Window, _cx: &mut App) {}
}

/// The host when the page runs on its own.
pub struct NoHost;

impl CapturesHost for NoHost {}
