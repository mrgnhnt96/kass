//! `kass://` links: open a screen in the main window from outside the
//! app, e.g. `kass://captures?capture=<id>` from a review doc.
//!
//! macOS delivers the link through GPUI's `on_open_urls` (the scheme is
//! registered by `CFBundleURLTypes` in Info.plist, so only an installed
//! build receives them). A link can arrive before the app has started, when
//! it is what launched Kass, so the latest one is kept until the shell takes
//! it.

use std::sync::Mutex;

use gpui_kit::App;

use crate::host::{self, AppHandle, Manager};
use crate::shell;

/// `herga` is Kass's old name; links made before the rename still open.
const SCHEMES: [&str; 2] = ["kass", "herga"];
const MAIN_WINDOW_LABEL: &str = "main";

#[derive(Default)]
pub struct DeepLinkState {
    pending: Mutex<Option<String>>,
}

/// The in-app route for a `kass://` link: `kass://captures?capture=x`
/// is `/captures?capture=x`. None for other schemes.
pub fn route(url: &str) -> Option<String> {
    let rest = SCHEMES
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme)?.strip_prefix("://"))?;
    Some(format!("/{}", rest.trim_start_matches('/')))
}

/// Show the main window on the link's screen.
pub fn open(app: &AppHandle, url: &str) {
    let Some(route) = route(url) else { return };
    *app.state::<DeepLinkState>().pending.lock().unwrap() = Some(route);
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    host::with_app(deliver);
}

/// Show the screen of a link that's waiting, once.
pub fn deliver(cx: &mut App) {
    let Some(route) = AppHandle
        .try_state::<DeepLinkState>()
        .and_then(|state| state.pending.lock().unwrap().take())
    else {
        return;
    };
    let (path, search) = match route.split_once('?') {
        Some((path, search)) => (path.to_string(), format!("?{search}")),
        None => (route, String::new()),
    };
    shell::navigate(&path, &search, cx);
}

#[cfg(test)]
mod tests {
    use super::route;

    #[test]
    fn maps_a_link_to_its_route() {
        assert_eq!(
            route("kass://captures?capture=abc").as_deref(),
            Some("/captures?capture=abc")
        );
        assert_eq!(
            route("kass:///settings/dictation").as_deref(),
            Some("/settings/dictation")
        );
        assert_eq!(route("kass://").as_deref(), Some("/"));
        assert_eq!(route("https://example.com"), None);
        assert_eq!(route("kassx://captures"), None);
        assert_eq!(
            route("herga://captures?capture=abc").as_deref(),
            Some("/captures?capture=abc")
        );
    }
}
