//! What the native Captures page needs from the app: installed apps' icons,
//! the screens it links to, fixing a correction where Kass typed it, and
//! dictating a phrase.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Image, Task, Window};
use kass_api::types::CaptureAppFilter;
use kass_captures::{CapturesHost, Route};
use kass_dictionary::PhraseDictation;

use super::insights::Icons;
use crate::dictation::corrections;
use crate::host::{async_runtime, AppHandle};
use crate::settings::dictionary::HostDictation;

/// Where the page is in the main window.
const PATH: &str = "/captures";

pub struct Host {
    pub icons: Rc<Icons>,
}

impl CapturesHost for Host {
    fn apply_correction(
        &self,
        capture_id: String,
        before: String,
        after: String,
        cx: &mut App,
    ) -> Task<Option<String>> {
        let applied = async_runtime::spawn(corrections::apply_correction(
            AppHandle, capture_id, before, after,
        ));
        cx.background_executor()
            .spawn(async move { applied.await.ok().flatten() })
    }

    fn app_icon(&self, bundle_id: &str) -> Option<Arc<Image>> {
        self.icons.get(bundle_id)
    }

    fn navigate(&self, route: Route, _window: &mut Window, cx: &mut App) {
        let (path, search) = link(&route);
        super::navigate(path, &search, cx);
    }

    fn phrase_dictation(&self) -> Option<Rc<dyn PhraseDictation>> {
        Some(Rc::new(HostDictation::on(PATH)))
    }
}

/// Where a route goes: its path and search.
fn link(route: &Route) -> (&'static str, String) {
    match route {
        Route::Setup => ("/setup", String::new()),
        Route::WritingStyle(style) => (
            "/settings/writing-style",
            style
                .as_ref()
                .map(|id| format!("?style={id}"))
                .unwrap_or_default(),
        ),
        Route::CommandMode => ("/settings/command-mode", String::new()),
        Route::DictionaryPhrases => ("/settings/dictionary", "?kind=phrases".into()),
        Route::DictationSettings => ("/settings/dictation", String::new()),
        Route::Insights { filter, period } => {
            let apps = match filter {
                CaptureAppFilter::All => "apps=all".to_string(),
                CaptureAppFilter::Unknown => "apps=unknown".to_string(),
                CaptureAppFilter::App(bundle_id) => format!("app={bundle_id}"),
            };
            ("/insights", format!("?{apps}&period={}", period.as_str()))
        }
    }
}

/// The capture a link asks for, from a search like `?capture=<id>`.
pub fn capture_in(search: &str) -> Option<String> {
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("capture="))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kass_api::types::UsagePeriod;

    #[test]
    fn capture_from_search() {
        assert_eq!(capture_in("?capture=abc"), Some("abc".into()));
        assert_eq!(capture_in("?x=1&capture=abc"), Some("abc".into()));
        assert_eq!(capture_in("?capture="), None);
        assert_eq!(capture_in(""), None);
    }

    #[test]
    fn routes_go_where_the_page_linked() {
        assert_eq!(link(&Route::Setup), ("/setup", String::new()));
        assert_eq!(
            link(&Route::WritingStyle(Some("s1".into()))),
            ("/settings/writing-style", "?style=s1".into())
        );
        assert_eq!(
            link(&Route::WritingStyle(None)),
            ("/settings/writing-style", String::new())
        );
        assert_eq!(
            link(&Route::DictionaryPhrases),
            ("/settings/dictionary", "?kind=phrases".into())
        );
    }

    #[test]
    fn insights_opens_on_the_listed_apps_and_week() {
        let insights = |filter| {
            link(&Route::Insights {
                filter,
                period: UsagePeriod::Days7,
            })
        };
        assert_eq!(
            insights(CaptureAppFilter::App("com.tinyspeck.slackmacgap".into())),
            (
                "/insights",
                "?app=com.tinyspeck.slackmacgap&period=7d".into()
            )
        );
        assert_eq!(
            insights(CaptureAppFilter::All),
            ("/insights", "?apps=all&period=7d".into())
        );
        assert_eq!(
            insights(CaptureAppFilter::Unknown),
            ("/insights", "?apps=unknown&period=7d".into())
        );
    }
}
