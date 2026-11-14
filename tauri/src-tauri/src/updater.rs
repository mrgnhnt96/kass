//! Updates in the background: a newer release is downloaded while Kass
//! runs, then installed when the user clicks Restart or the next time Kass
//! quits. It is never installed while the app and its server are running
//! from the bundle it replaces.
//!
//! Releases publish `latest.json` next to a minisign-signed `.app.tar.gz`
//! (.github/workflows/release.yml); the download is checked against
//! [`PUBLIC_KEY`] before it is kept. This is the format, key and install
//! Tauri's updater used, so copies from before GPUI update to this one and
//! on.
//!
//! Stable copies read the newest public release's `latest.json`. Copies on
//! the beta channel read `beta.json` on the `channels` release instead,
//! which always names the newest release, beta or not, so beta users also
//! get stable releases once they're newer.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::host::{async_runtime, AppHandle, Emitter, Manager, State};

/// After launch, so the first check doesn't compete with starting up.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const STATUS_EVENT: &str = "update:status";
/// Tells every window the channel changed, which turns beta features on or off.
const CHANNEL_EVENT: &str = "update:channel";
const STABLE_ENDPOINT: &str =
    "https://github.com/mrgnhnt96/kass/releases/latest/download/latest.json";
const BETA_ENDPOINT: &str =
    "https://github.com/mrgnhnt96/kass/releases/download/channels/beta.json";
/// The minisign public key releases are signed with, base64-encoded as
/// Tauri's config held it.
const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEY4NjFDQjEzMzc1NTBCMzMKUldRekMxVTNFOHRoK0VLZ3FTRHUvUHJObHhrSXdXUGhmbklvRTRYaStjbjduRnFLYXV0ZjV0VlAK";
/// This Mac's entry in `latest.json`.
const PLATFORM: &str = "darwin-aarch64";
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
    version: String,
    path: PathBuf,
}

/// `latest.json`: the newest release and where each platform's archive is.
#[derive(Deserialize)]
struct Manifest {
    version: String,
    platforms: std::collections::HashMap<String, Platform>,
}

#[derive(Deserialize)]
struct Platform {
    signature: String,
    url: String,
}

/// A release newer than this one.
struct Release {
    version: String,
    signature: String,
    url: String,
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

fn channel_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(CHANNEL_FILE))
}

fn channel(app: &AppHandle) -> Channel {
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
pub fn beta_features(app: &AppHandle) -> bool {
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

fn set_status(app: &AppHandle, status: UpdateStatus) {
    let state = app.state::<UpdaterState>();
    *state.status.lock().unwrap() = status.clone();
    let _ = app.emit(STATUS_EVENT, status);
}

/// Check now and then every few hours, downloading any newer release.
/// Development builds never update themselves.
pub fn start(app: AppHandle) {
    BETA_FEATURES.store(beta_features(&app), Ordering::Relaxed);
    if cfg!(debug_assertions) {
        return;
    }
    async_runtime::spawn(async move {
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
async fn check(app: &AppHandle) -> Result<UpdateStatus, String> {
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

fn parse_version(text: &str) -> Result<semver::Version, String> {
    semver::Version::parse(text.trim_start_matches('v')).map_err(|e| format!("version {text}: {e}"))
}

/// The release `manifest` offers, if it is newer than `current`.
fn newer_release(manifest: Manifest, current: &str) -> Result<Option<Release>, String> {
    let version = parse_version(&manifest.version)?;
    if version <= parse_version(current)? {
        return Ok(None);
    }
    let platform = manifest
        .platforms
        .get(&format!("{PLATFORM}-app"))
        .or_else(|| manifest.platforms.get(PLATFORM))
        .ok_or_else(|| format!("release {version} has nothing for {PLATFORM}"))?;
    Ok(Some(Release {
        version: version.to_string(),
        signature: platform.signature.clone(),
        url: platform.url.clone(),
    }))
}

fn base64_text(text: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Check `bytes` against the release's minisign signature.
fn verify(bytes: &[u8], signature: &str, public_key: &str) -> Result<(), String> {
    let key = minisign_verify::PublicKey::decode(&base64_text(public_key)?)
        .map_err(|e| format!("public key: {e}"))?;
    let signature = minisign_verify::Signature::decode(&base64_text(signature)?)
        .map_err(|e| format!("signature: {e}"))?;
    key.verify(bytes, &signature, true)
        .map_err(|e| format!("the download's signature doesn't match: {e}"))
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("kass/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10 * 60))
        .build()
        .map_err(|e| e.to_string())
}

async fn check_and_download(app: &AppHandle) -> Result<(), String> {
    let checked_channel = channel(app);
    let endpoint = match checked_channel {
        Channel::Stable => STABLE_ENDPOINT,
        Channel::Beta => BETA_ENDPOINT,
    };
    let client = client()?;
    let manifest: Manifest = client
        .get(endpoint)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let Some(release) = newer_release(manifest, env!("CARGO_PKG_VERSION"))? else {
        return Ok(());
    };
    let version = release.version.clone();
    {
        let pending = app.state::<UpdaterState>();
        let pending = pending.pending.lock().unwrap();
        if pending.as_ref().is_some_and(|p| p.version == version) {
            return Ok(());
        }
    }
    set_status(
        app,
        UpdateStatus::Downloading {
            version: version.clone(),
        },
    );
    let bytes = client
        .get(&release.url)
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    verify(&bytes, &release.signature, PUBLIC_KEY)?;
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
    *app.state::<UpdaterState>().pending.lock().unwrap() = Some(Pending { version, path });
    set_status(
        app,
        UpdateStatus::Ready {
            version: release.version,
        },
    );
    Ok(())
}

/// The running `.app`: the executable is `Kass.app/Contents/MacOS/kass`.
fn running_bundle() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    exe.ancestors()
        .nth(3)
        .filter(|bundle| bundle.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} isn't inside an app bundle", exe.display()))
}

/// A new, empty folder in the temp dir.
fn temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Unpack the archive's `Kass.app` into `into`, dropping the top folder.
fn unpack(archive: &[u8], into: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(archive)));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let inner: PathBuf = entry
            .path()
            .map_err(|e| e.to_string())?
            .iter()
            .skip(1)
            .collect();
        if inner.as_os_str().is_empty() {
            continue;
        }
        let target = into.join(&inner);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Put the unpacked app at `bundle`. When the folder it's in can't be
/// written, macOS asks for an administrator's password, as Tauri's
/// updater did.
fn replace_bundle(bundle: &Path, new_app: &Path) -> Result<(), String> {
    let backup = temp_dir("kass-current-app")?;
    match std::fs::rename(bundle, backup.join("current_app")) {
        Ok(()) => {
            if let Err(e) = std::fs::rename(new_app, bundle) {
                // Put the old one back rather than leave nothing there.
                let _ = std::fs::rename(backup.join("current_app"), bundle);
                return Err(e.to_string());
            }
            let _ = std::fs::remove_dir_all(&backup);
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let _ = std::fs::remove_dir_all(&backup);
            let script = format!(
                "do shell script \"rm -rf '{}' && mv -f '{}' '{}'\" with administrator privileges",
                bundle.display(),
                new_app.display(),
                bundle.display()
            );
            let status = std::process::Command::new("/usr/bin/osascript")
                .args(["-e", &script])
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("the update wasn't allowed to replace Kass".into());
            }
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&backup);
            return Err(e.to_string());
        }
    }
    // So Finder and Launch Services see the new version.
    let _ = std::process::Command::new("/usr/bin/touch")
        .arg(bundle)
        .status();
    Ok(())
}

fn install(archive: &[u8]) -> Result<(), String> {
    let bundle = running_bundle()?;
    let staging = temp_dir("kass-updated-app")?;
    let installed = unpack(archive, &staging).and_then(|()| replace_bundle(&bundle, &staging));
    let _ = std::fs::remove_dir_all(&staging);
    installed
}

/// Swap in the downloaded version, if there is one. Call only once
/// nothing runs from the bundle anymore (the server is stopped).
pub fn install_pending(app: &AppHandle) -> Result<bool, String> {
    let Some(pending) = app.state::<UpdaterState>().pending.lock().unwrap().take() else {
        return Ok(false);
    };
    let bytes = std::fs::read(&pending.path).map_err(|e| e.to_string())?;
    let installed = install(&bytes);
    let _ = std::fs::remove_file(&pending.path);
    installed.map(|_| true)
}

/// Drop a downloaded beta, so leaving the beta channel doesn't install one.
fn discard_pending_beta(app: &AppHandle) {
    let state = app.state::<UpdaterState>();
    let mut pending = state.pending.lock().unwrap();
    if pending.as_ref().is_some_and(|p| p.version.contains('-')) {
        if let Some(p) = pending.take() {
            let _ = std::fs::remove_file(&p.path);
        }
        drop(pending);
        set_status(app, UpdateStatus::Current);
    }
}

pub fn update_channel(app: AppHandle) -> Channel {
    channel(&app)
}

/// Switch channels and check again right away. Leaving beta keeps the
/// beta that's installed; Kass moves on once a public release is newer.
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
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateStatus, String> {
    if cfg!(debug_assertions) {
        return Err("Development builds don't update themselves.".into());
    }
    check(&app).await
}

pub fn update_status(state: State<'_, UpdaterState>) -> UpdateStatus {
    state.status.lock().unwrap().clone()
}

/// Relaunch into the downloaded version: quitting installs it (see the
/// quit handler in `run`) once the server has stopped.
pub async fn restart_to_update(state: State<'_, crate::ServerState>) -> Result<(), String> {
    crate::restart_app(state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "version": version,
            "notes": "",
            "pub_date": "2026-11-01T00:00:00Z",
            "platforms": { "darwin-aarch64": { "signature": "sig", "url": "https://example.com/Kass.app.tar.gz" } }
        }))
        .unwrap()
    }

    #[test]
    fn offers_only_newer_releases() {
        assert!(newer_release(manifest("0.10.0-beta.7"), "0.10.0-beta.7")
            .unwrap()
            .is_none());
        assert!(newer_release(manifest("0.9.0"), "0.10.0-beta.7")
            .unwrap()
            .is_none());
        let release = newer_release(manifest("v0.10.0"), "0.10.0-beta.7")
            .unwrap()
            .unwrap();
        assert_eq!(release.version, "0.10.0");
        assert_eq!(release.url, "https://example.com/Kass.app.tar.gz");
        assert!(newer_release(manifest("0.10.0-beta.8"), "0.10.0-beta.7")
            .unwrap()
            .is_some());
    }

    #[test]
    fn rejects_a_bad_signature() {
        // A signature from another key, in the format latest.json carries.
        let error = verify(b"payload", "bm90IGEgc2lnbmF0dXJl", PUBLIC_KEY).unwrap_err();
        assert!(error.starts_with("signature"), "{error}");
        assert!(base64_text(PUBLIC_KEY)
            .unwrap()
            .contains("minisign public key"));
    }

    #[test]
    fn unpacks_without_the_top_folder() {
        let mut tar = tar::Builder::new(Vec::new());
        let body = b"#!/bin/sh\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "Kass.app/Contents/MacOS/kass", &body[..])
            .unwrap();
        let tar = tar.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        let archive = gz.finish().unwrap();
        let dir = temp_dir("kass-updater-test").unwrap();
        unpack(&archive, &dir).unwrap();
        assert!(dir.join("Contents/MacOS/kass").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
