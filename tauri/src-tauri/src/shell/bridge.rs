//! How the native shell and the main window's page keep each other in step
//! while both draw parts of the window (app/src/lib/nativeShell.ts is the
//! page's side).
//!
//! The page tells the shell where its router is (`shell_location`). The
//! shell asks the page to navigate (`shell:navigate`) and to refetch what
//! native settings changed (`shell:invalidate`). The preferences both use
//! are the app's own (src/prefs.rs).

use serde::Deserialize;
use serde_json::json;

use crate::host::emit_value;
use crate::MAIN_WINDOW_LABEL;

/// Where the page's router is.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Location {
    pub path: String,
    pub search: String,
}

/// Show `path` (with `search`, like `?model=whisper-turbo`). The page's
/// router follows and reports back with `shell_location`.
pub fn navigate(path: &str, search: &str) {
    emit_value(
        "shell:navigate",
        json!({ "path": path, "search": search }),
        Some(MAIN_WINDOW_LABEL.to_string()),
    );
}

/// Tell the page that what it fetched under these query keys changed.
pub fn invalidate(keys: &[&[&str]]) {
    emit_value(
        "shell:invalidate",
        json!({ "keys": keys }),
        Some(MAIN_WINDOW_LABEL.to_string()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_where_the_page_is() {
        let location: Location = serde_json::from_value(json!({ "path": "/models" })).unwrap();
        assert_eq!(location.path, "/models");
        assert_eq!(location.search, "");
    }
}
