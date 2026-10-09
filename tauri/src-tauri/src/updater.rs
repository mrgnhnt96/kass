//! Updates in the background: a newer release is downloaded while Kass
//! runs, then installed when the user clicks Restart or the next time Kass
//! quits. It is never installed while the app and its server are running
//! from the bundle it replaces.
//!
//! Releases publish `latest.json` next to a signed `.app.tar.gz`
//! (.github/workflows/release.yml); the plugin checks the signature against
//! the public key in tauri.conf.json.
//!
//! Stable copies read the newest public release's `latest.json` (the
//! endpoint in tauri.conf.json). Copies on the beta channel read
//! `beta.json` on the `channels` release instead, which always names the
//! newest release, beta or not, so beta users also get stable releases
//! once they're newer.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{command, AppHandle, Emitter, Manager, Runtime, State, Url};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Notify;

/// After launch, so the first check doesn't compete with starting up.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const STATUS_EVENT: &str = "update:status";
/// Tells every window the channel changed, which turns beta features on or off.
const CHANNEL_EVENT: &str = "update:channel";
const BETA_ENDPOINT: &str =
    "https://github.com/mrgnhnt96/kass/releases/download/channels/beta.json";
/// Present (containing `beta`) while this copy is on the beta channel. It
/// sits in the app data dir, where the server reads it too (backend/beta.py).
const CHANNEL_FILE: &str = "update-channel";

/// Which releases this copy updates to. The beta channel also turns on
/// beta features: they ship in every release, but only show for beta users.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Public releases only.
    Stable,
    /// `-beta` releases too, as soon as they're tagged.
    Beta,
}

#[derive(Clone, Serialize, PartialEq, Eq, Debug)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// This is the latest version, or nothing has been found yet.
    Current,
    Downloading {
        version: String,
    },
    /// Downloaded and verified; installs on restart or quit.
    Ready {
        version: String,
    },
}

struct Pending {
    update: Update,
    path: PathBuf,
}

pub struct UpdaterState {
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<Pending>>,
    /// Checks right away instead of waiting out the interval.
    wake: Notify,
    /// Held for a whole check, so a check the user asks for and the
    /// background one never download the same release twice.
    checking: tokio::sync::Mutex<()>,
}

impl Default for UpdaterState {
    fn default() -> Self {
        Self {
            status: Mutex::new(UpdateStatus::Current),
            pending: Mutex::new(None),
            wake: Notify::new(),
            checking: tokio::sync::Mutex::new(()),
        }
    }
}

fn channel_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(CHANNEL_FILE))
}

fn channel<R: Runtime>(app: &AppHandle<R>) -> Channel {
    let beta = channel_path(app)
        .and_then(|path| std::fs::read_to_string(path).map_err(|e| e.to_string()))
        .is_ok_and(|text| text.trim() == "beta");
    if beta {
        Channel::Beta
    } else {
        Channel::Stable
    }
}

/// Whether beta features are on: this copy is on the beta channel.
pub fn beta_features<R: Runtime>(app: &AppHandle<R>) -> bool {
    channel(app) == Channel::Beta
}

/// [`beta_features`], kept in memory for code with no `AppHandle`, such as
/// text insertion. Set at launch and on every channel switch.
static BETA_FEATURES: AtomicBool = AtomicBool::new(false);

/// Whether beta features are on, for code with no `AppHandle`.
#[allow(dead_code)] // No beta features right now.
pub fn beta_features_on() -> bool {
    BETA_FEATURES.load(Ordering::Relaxed)
}

fn set_status<R: Runtime>(app: &AppHandle<R>, status: UpdateStatus) {
    let state = app.state::<UpdaterState>();
    *state.status.lock().unwrap() = status.clone();
    let _ = app.emit(STATUS_EVENT, status);
}

/// Check now and then every few hours, downloading any newer release.
/// Development builds never update themselves.
pub fn start<R: Runtime>(app: AppHandle<R>) {
    BETA_FEATURES.store(beta_features(&app), Ordering::Relaxed);
    if cfg!(debug_assertions) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let _ = check(&app).await;
            let state = app.state::<UpdaterState>();
            tokio::select! {
                _ = tokio::time::sleep(CHECK_INTERVAL) => {}
                _ = state.wake.notified() => {}
            }
        }
    });
}

/// Check once, downloading any newer release, and return where that leaves
/// things. A check already running finishes first.
async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<UpdateStatus, String> {
    let state = app.state::<UpdaterState>();
    let _checking = state.checking.lock().await;
    if let Err(error) = check_and_download(app).await {
        eprintln!("Updater: {error}");
        // What was downloaded before stays ready; otherwise try again later.
        if state.pending.lock().unwrap().is_none() {
            set_status(app, UpdateStatus::Current);
        }
        return Err(error);
    }
    let status = state.status.lock().unwrap().clone();
    Ok(status)
}

async fn check_and_download<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let checked_channel = channel(app);
    let updater = match checked_channel {
        Channel::Stable => app.updater(),
        Channel::Beta => app
            .updater_builder()
            .endpoints(vec![Url::parse(BETA_ENDPOINT).expect("valid beta endpoint")])
            .and_then(|builder| builder.build()),
    }
    .map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let version = update.version.clone();
    {
        let pending = app.state::<UpdaterState>();
        let pending = pending.pending.lock().unwrap();
        if pending
            .as_ref()
            .is_some_and(|p| p.update.version == version)
        {
            return Ok(());
        }
    }
    set_status(
        app,
        UpdateStatus::Downloading {
            version: version.clone(),
        },
    );
    // Verified against the public key before it's returned.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("update.app.tar.gz");
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    drop(bytes);
    // The user switched channels while this downloaded; the next check
    // (woken by the switch) finds what the new channel offers. Any earlier
    // download went to the same file, so it's gone too.
    if channel(app) != checked_channel {
        let _ = std::fs::remove_file(&path);
        *app.state::<UpdaterState>().pending.lock().unwrap() = None;
        set_status(app, UpdateStatus::Current);
        return Ok(());
    }
    *app.state::<UpdaterState>().pending.lock().unwrap() = Some(Pending { update, path });
    set_status(app, UpdateStatus::Ready { version });
    Ok(())
}

/// Swap in the downloaded version, if there is one. Call only once
/// nothing runs from the bundle anymore (the server is stopped).
pub fn install_pending<R: Runtime>(app: &AppHandle<R>) -> Result<bool, String> {
    let Some(pending) = app.state::<UpdaterState>().pending.lock().unwrap().take() else {
        return Ok(false);
    };
    let bytes = std::fs::read(&pending.path).map_err(|e| e.to_string())?;
    let installed = pending.update.install(bytes).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&pending.path);
    installed.map(|_| true)
}

/// Drop a downloaded beta, so leaving the beta channel doesn't install one.
fn discard_pending_beta<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<UpdaterState>();
    let mut pending = state.pending.lock().unwrap();
    if pending
        .as_ref()
        .is_some_and(|p| p.update.version.contains('-'))
    {
        if let Some(p) = pending.take() {
            let _ = std::fs::remove_file(&p.path);
        }
        drop(pending);
        set_status(app, UpdateStatus::Current);
    }
}

#[command]
pub fn update_channel(app: AppHandle) -> Channel {
    channel(&app)
}

/// Switch channels and check again right away. Leaving beta keeps the
/// beta that's installed; Kass moves on once a public release is newer.
#[command]
pub fn set_update_channel(app: AppHandle, channel: Channel) -> Result<Channel, String> {
    let path = channel_path(&app)?;
    match channel {
        Channel::Beta => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, "beta").map_err(|e| e.to_string())?;
        }
        Channel::Stable => {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            discard_pending_beta(&app);
        }
    }
    BETA_FEATURES.store(channel == Channel::Beta, Ordering::Relaxed);
    app.state::<UpdaterState>().wake.notify_one();
    let _ = app.emit(CHANNEL_EVENT, channel);
    Ok(channel)
}

/// Check now, for the user's "Check for updates", and wait for any newer
/// release to download. `Current` means this is the latest version.
#[command]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateStatus, String> {
    if cfg!(debug_assertions) {
        return Err("Development builds don't update themselves.".into());
    }
    check(&app).await
}

#[command]
pub fn update_status(state: State<'_, UpdaterState>) -> UpdateStatus {
    state.status.lock().unwrap().clone()
}

/// Relaunch into the downloaded version: quitting installs it (see the
/// `RunEvent::Exit` handler) once the server has stopped.
#[command]
pub async fn restart_to_update(
    app: AppHandle,
    state: State<'_, crate::ServerState>,
) -> Result<(), String> {
    crate::restart_app(app, state).await
}
