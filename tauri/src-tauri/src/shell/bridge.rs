//! How the native shell and the main window's page keep each other in step
//! while both draw parts of the window (app/src/lib/nativeShell.ts is the
//! page's side).
//!
//! The page tells the shell where its router is (`shell_location`) and the
//! preferences it still owns (`shell_sync`). The shell asks the page to
//! navigate (`shell:navigate`), to change one of those preferences
//! (`shell:command`, answered with `shell_command_result`) and to refetch
//! what native settings changed (`shell:invalidate`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::host::emit_value;
use crate::MAIN_WINDOW_LABEL;

/// Where the page's router is.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Location {
    pub path: String,
    pub search: String,
}

/// The preferences the page keeps (app/src/stores/serverStore.ts and the
/// theme in uiStore.ts).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Synced {
    /// `system`, `light` or `dark`.
    pub theme: String,
    pub share_on_network: bool,
    /// The other Mac whose Kass this one uses (beta `network`).
    pub remote_server_url: Option<String>,
    /// Where requests go: the other Mac's server, or this Mac's while it's away.
    pub server_url: String,
    /// The other Mac can't be reached, so this Mac's server stands in.
    pub shared_away: bool,
}

/// Something only the page can do, because it owns the preference.
pub enum Command {
    SetTheme(&'static str),
    /// Beta updates on or off, then the restart that needs.
    SetBeta(bool),
    /// Share this Mac's server, then restart it.
    SetShareOnNetwork(bool),
    /// Use another Mac's server (or this Mac's again), then restart.
    UseRemote(Option<String>),
    /// Start onboarding again from the first step.
    OpenOnboarding,
}

impl Command {
    fn payload(&self) -> Value {
        match self {
            Self::SetTheme(theme) => json!({ "kind": "setTheme", "theme": theme }),
            Self::SetBeta(beta) => json!({ "kind": "setBeta", "beta": beta }),
            Self::SetShareOnNetwork(share) => {
                json!({ "kind": "setShareOnNetwork", "share": share })
            }
            Self::UseRemote(url) => json!({ "kind": "useRemote", "url": url }),
            Self::OpenOnboarding => json!({ "kind": "openOnboarding" }),
        }
    }
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<(), String>>>>;

fn pending() -> &'static Pending {
    static PENDING: std::sync::OnceLock<Pending> = std::sync::OnceLock::new();
    PENDING.get_or_init(Default::default)
}

/// Ask the page to run `command`; resolves once it has, with its error.
pub async fn send(command: Command) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    pending().lock().unwrap().insert(id, tx);
    let mut payload = command.payload();
    payload["id"] = json!(id);
    emit_value(
        "shell:command",
        payload,
        Some(MAIN_WINDOW_LABEL.to_string()),
    );
    rx.await
        .unwrap_or_else(|_| Err("The page didn't answer".to_string()))
}

/// The page finished command `id`.
pub fn answered(id: u64, error: Option<String>) {
    if let Some(tx) = pending().lock().unwrap().remove(&id) {
        let _ = tx.send(error.map_or(Ok(()), Err));
    }
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
    fn reads_what_the_page_syncs() {
        let synced: Synced = serde_json::from_value(json!({
            "theme": "dark",
            "shareOnNetwork": true,
            "remoteServerUrl": null,
            "serverUrl": "http://127.0.0.1:17493",
            "sharedAway": false,
        }))
        .unwrap();
        assert_eq!(synced.theme, "dark");
        assert!(synced.share_on_network);
        assert_eq!(synced.remote_server_url, None);
        assert_eq!(synced.server_url, "http://127.0.0.1:17493");
    }

    #[test]
    fn commands_carry_their_kind() {
        assert_eq!(
            Command::UseRemote(Some("http://studio.local:17493".into())).payload(),
            json!({ "kind": "useRemote", "url": "http://studio.local:17493" })
        );
        assert_eq!(Command::UseRemote(None).payload()["url"], Value::Null);
        assert_eq!(Command::SetBeta(false).payload()["beta"], json!(false));
    }

    #[test]
    fn an_answer_resolves_its_command() {
        let (tx, mut rx) = oneshot::channel();
        pending().lock().unwrap().insert(9_999, tx);
        answered(9_999, Some("nope".into()));
        assert_eq!(rx.try_recv().unwrap(), Err("nope".to_string()));
    }
}
