//! The app's own preferences: the theme and the beta `network` settings
//! (share this Mac's server, use another Mac's). Saved in app data as
//! `prefs.json`. The main window's page kept them in its localStorage
//! before; it hands them over once with `prefs_adopt` and follows
//! `prefs:changed` from then on.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::host::{self, AppHandle, Emitter, Manager};
use crate::updater::{self, Channel};
use crate::ServerState;

/// Sent to every window when a preference or [`away`] changes, with
/// [`page_view`].
pub const CHANGED_EVENT: &str = "prefs:changed";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    /// `system`, `light` or `dark`.
    pub theme: String,
    /// Beta `network`: run this Mac's server for other Macs too.
    pub share_on_network: bool,
    /// Beta `network`: the other Mac whose Kass this one uses.
    pub remote_server_url: Option<String>,
    /// Set once the page's old values were taken, or anything was changed
    /// here: the page's copy counts for nothing after that.
    pub adopted: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            share_on_network: false,
            remote_server_url: None,
            adopted: false,
        }
    }
}

impl Prefs {
    /// The other Mac in use: only for beta users.
    pub fn remote(&self, beta: bool) -> Option<&str> {
        self.remote_server_url
            .as_deref()
            .filter(|url| beta && !url.is_empty())
    }

    /// Whether this Mac's server listens for other Macs: beta users who
    /// asked for it and aren't using another Mac's.
    pub fn shares(&self, beta: bool) -> bool {
        beta && self.share_on_network && self.remote(beta).is_none()
    }

    /// The page's values, when nothing has been adopted or changed yet.
    fn adopt(&self, page: PagePrefs) -> Option<Self> {
        if self.adopted {
            return None;
        }
        Some(Self {
            theme: match page.theme.as_str() {
                "light" | "dark" => page.theme,
                _ => "system".into(),
            },
            share_on_network: page.share_on_network,
            remote_server_url: page.remote_server_url.filter(|url| !url.is_empty()),
            adopted: true,
        })
    }
}

/// What the page kept in its localStorage (app/src/stores).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PagePrefs {
    pub theme: String,
    pub share_on_network: bool,
    pub remote_server_url: Option<String>,
}

static PREFS: Mutex<Option<Prefs>> = Mutex::new(None);

/// The other Mac in use can't be reached, so this Mac's server stands in
/// (shell/data.rs checks). Not saved.
static AWAY: AtomicBool = AtomicBool::new(false);

fn path() -> Option<PathBuf> {
    AppHandle
        .path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("prefs.json"))
}

fn load() -> Prefs {
    path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(prefs: &Prefs) {
    let Some(path) = path() else {
        return;
    };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(prefs)?)?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        eprintln!("[prefs] save failed: {e}");
    }
}

pub fn get() -> Prefs {
    PREFS.lock().unwrap().get_or_insert_with(load).clone()
}

fn replace(next: Prefs) {
    {
        let mut prefs = PREFS.lock().unwrap();
        if prefs.as_ref() == Some(&next) {
            return;
        }
        *prefs = Some(next.clone());
    }
    save(&next);
    changed();
}

/// Change the preferences, save them and tell everyone.
pub fn change(f: impl FnOnce(&mut Prefs)) {
    let mut next = get();
    f(&mut next);
    next.adopted = true;
    replace(next);
}

/// Take the page's old values the first time it asks; answer with what
/// counts now, as [`page_view`].
pub fn adopt(page: PagePrefs) -> serde_json::Value {
    if let Some(next) = get().adopt(page) {
        replace(next);
    }
    page_view()
}

pub fn away() -> bool {
    AWAY.load(Ordering::Relaxed)
}

/// The other Mac went away or came back; whether that changed anything.
pub fn set_away(away: bool) -> bool {
    if AWAY.swap(away, Ordering::Relaxed) == away {
        return false;
    }
    crate::dictation::dictation_set_away(
        AppHandle.state::<crate::dictation::DictationState>(),
        away,
    );
    changed();
    true
}

/// The preferences as the page's stores hold them.
pub fn page_view() -> serde_json::Value {
    let prefs = get();
    json!({
        "theme": prefs.theme,
        "shareOnNetwork": prefs.share_on_network,
        "remoteServerUrl": prefs.remote_server_url,
        "sharedAway": away(),
    })
}

fn changed() {
    let _ = AppHandle.emit(CHANGED_EVENT, page_view());
    host::with_app(|cx| {
        if let Some(data) = crate::shell::try_shell_data(cx) {
            data.update(cx, |data, cx| data.follow_prefs(cx));
        }
    });
}

pub fn set_theme(theme: &str) {
    let theme = match theme {
        "light" | "dark" => theme,
        _ => "system",
    };
    change(|prefs| prefs.theme = theme.to_string());
}

fn beta() -> bool {
    updater::beta_features(&AppHandle)
}

/// Beta updates on or off. Leaving beta stops `network`: back onto this
/// Mac's own server, with the restart that needs.
pub async fn set_beta(on: bool) -> Result<(), String> {
    let before = get();
    let (was_remote, was_sharing) = (before.remote(beta()).is_some(), before.shares(beta()));
    updater::set_update_channel(AppHandle, if on { Channel::Beta } else { Channel::Stable })?;
    changed();
    if !on && was_remote {
        crate::restart_app(AppHandle.state::<ServerState>()).await
    } else if !on && was_sharing {
        crate::restart_server(AppHandle, AppHandle.state::<ServerState>(), None)
            .await
            .map(drop)
    } else {
        Ok(())
    }
}

/// Share this Mac's server with other Macs, or stop, and restart it.
pub async fn set_share_on_network(share: bool) -> Result<(), String> {
    change(|prefs| prefs.share_on_network = share);
    let result = crate::restart_server(AppHandle, AppHandle.state::<ServerState>(), None).await;
    if result.is_err() {
        change(|prefs| prefs.share_on_network = !share);
    }
    result.map(drop)
}

/// Use another Mac's server (or this Mac's again with `None`), then
/// restart the app onto it.
pub async fn use_remote(url: Option<String>) -> Result<(), String> {
    let previous = get().remote_server_url;
    change(|prefs| prefs.remote_server_url = url);
    let result = crate::restart_app(AppHandle.state::<ServerState>()).await;
    if result.is_err() {
        change(|prefs| prefs.remote_server_url = previous);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(theme: &str, share: bool, remote: Option<&str>) -> PagePrefs {
        PagePrefs {
            theme: theme.into(),
            share_on_network: share,
            remote_server_url: remote.map(Into::into),
        }
    }

    #[test]
    fn takes_the_pages_values_once() {
        let taken = Prefs::default()
            .adopt(page("dark", true, Some("http://studio.local:17493")))
            .unwrap();
        assert_eq!(taken.theme, "dark");
        assert!(taken.share_on_network);
        assert_eq!(
            taken.remote_server_url.as_deref(),
            Some("http://studio.local:17493")
        );
        assert!(taken.adopted);
        assert_eq!(taken.adopt(page("light", false, None)), None);
    }

    #[test]
    fn a_change_here_wins_over_the_page() {
        let changed = Prefs {
            theme: "light".into(),
            adopted: true,
            ..Prefs::default()
        };
        assert_eq!(changed.adopt(page("dark", false, None)), None);
    }

    #[test]
    fn unknown_or_empty_page_values_fall_back() {
        let taken = Prefs::default().adopt(page("", false, Some(""))).unwrap();
        assert_eq!(taken.theme, "system");
        assert_eq!(taken.remote_server_url, None);
    }

    #[test]
    fn network_is_for_beta_users_only() {
        let prefs = Prefs {
            share_on_network: true,
            remote_server_url: Some("http://studio.local:17493".into()),
            ..Prefs::default()
        };
        assert_eq!(prefs.remote(false), None);
        assert_eq!(prefs.remote(true), Some("http://studio.local:17493"));
        // Using another Mac's server, this one doesn't share its own.
        assert!(!prefs.shares(true));
        let sharing = Prefs {
            remote_server_url: None,
            ..prefs
        };
        assert!(sharing.shares(true));
        assert!(!sharing.shares(false));
    }

    #[test]
    fn reads_what_older_files_lack() {
        let prefs: Prefs = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(prefs.theme, "dark");
        assert!(!prefs.share_on_network);
        assert!(!prefs.adopted);
    }
}
