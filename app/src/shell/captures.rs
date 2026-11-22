//! What the native Captures page needs from the app: installed apps' icons,
//! the screens it links to, and fixing a correction where Kass typed it.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Image, Task, Window};
use kass_captures::{CapturesHost, Route};

use super::insights::Icons;
use crate::dictation::corrections;
use crate::host::{async_runtime, AppHandle};

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
        Route::Insights => ("/insights", String::new()),
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
        assert_eq!(link(&Route::Insights), ("/insights", String::new()));
    }
}
