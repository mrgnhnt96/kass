//! What the shell and the native settings pages show, refreshed in the
//! background: the server's health and settings, model readiness and
//! downloads, the shared Mac's sync, permissions and the updater.

use std::time::Duration;

use gpui_kit::{App, AsyncApp, Context, Task, WeakEntity};
use kass_api::types::{
    ActiveDownloadTask, CaptureReadinessResponse, CaptureSettings, CaptureSettingsUpdate,
    HealthResponse, SyncStatus,
};
use kass_api::{ApiClient, Mac, LOCAL_SERVER_URL};

use super::bridge::{self, Synced};
use crate::dictation::capture::NativeInputDevice;
use crate::host::{AppHandle, Manager};
use crate::{this_mac, updater};

/// How often the server is asked again, as the page's queries did.
const POLL: Duration = Duration::from_secs(5);
/// Microphones and learned examples change rarely: every this many polls.
const SLOW_EVERY: u32 = 6;

#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Connecting,
    Online(HealthResponse),
    Offline,
}

/// The beta `network` sync with another Mac, as the status bar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedSync {
    pub remote: String,
    pub away: bool,
    pub pending: u64,
}

pub struct ShellData {
    /// Requests go to [`Synced::server_url`]; clones share it.
    pub api: ApiClient,
    pub synced: Synced,
    pub health: Health,
    pub settings: Option<CaptureSettings>,
    pub readiness: Option<CaptureReadinessResponse>,
    pub downloads: Vec<ActiveDownloadTask>,
    /// This Mac's own sync status, while another Mac's server is in use.
    pub sync: Option<SyncStatus>,
    pub examples: u64,
    pub devices: Vec<NativeInputDevice>,
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub update: updater::UpdateStatus,
    pub channel: updater::Channel,
    pub version: String,
    _poll: Task<()>,
}

/// One round of answers from the server; `None` where a call failed.
struct Round {
    health: Option<HealthResponse>,
    settings: Option<CaptureSettings>,
    readiness: Option<CaptureReadinessResponse>,
    downloads: Option<Vec<ActiveDownloadTask>>,
    sync: Option<SyncStatus>,
    examples: Option<u64>,
    devices: Option<Vec<NativeInputDevice>>,
}

impl ShellData {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mac = this_mac::get().map(|mac| Mac::new(mac.id.clone(), mac.name.clone()));
        let app = AppHandle;
        let poll = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut tick = 0u32;
            loop {
                let slow = tick.is_multiple_of(SLOW_EVERY);
                tick = tick.wrapping_add(1);
                let Ok((api, remote)) = this.read_with(cx, |this, _| {
                    (this.api.clone(), this.synced.remote_server_url.clone())
                }) else {
                    return;
                };
                let round = cx
                    .background_executor()
                    .spawn(async move { fetch(&api, remote.is_some(), slow) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.apply(round, slow);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        });
        Self {
            api: ApiClient::new(LOCAL_SERVER_URL, mac),
            synced: Synced::default(),
            health: Health::Connecting,
            settings: None,
            readiness: None,
            downloads: Vec::new(),
            sync: None,
            examples: 0,
            devices: Vec::new(),
            accessibility: crate::check_accessibility_permission(),
            input_monitoring: crate::check_input_monitoring_permission(),
            update: updater::update_status(app.state::<updater::UpdaterState>()),
            channel: updater::update_channel(AppHandle),
            version: app.package_info().version,
            _poll: poll,
        }
    }

    fn apply(&mut self, round: Round, slow: bool) {
        self.health = match round.health {
            Some(health) => Health::Online(health),
            None => Health::Offline,
        };
        if round.settings.is_some() {
            self.settings = round.settings;
        }
        if round.readiness.is_some() {
            self.readiness = round.readiness;
        }
        if let Some(downloads) = round.downloads {
            self.downloads = downloads;
        }
        self.sync = round.sync;
        if let Some(examples) = round.examples {
            self.examples = examples;
        }
        if slow {
            if let Some(devices) = round.devices {
                self.devices = devices;
            }
        }
        self.accessibility = crate::check_accessibility_permission();
        self.input_monitoring = crate::check_input_monitoring_permission();
    }

    /// The page synced its preferences: follow its server and theme.
    pub fn set_synced(&mut self, synced: Synced, cx: &mut Context<Self>) {
        if synced == self.synced {
            return;
        }
        let url = if synced.server_url.is_empty() {
            LOCAL_SERVER_URL.to_string()
        } else {
            synced.server_url.clone()
        };
        if url != self.api.base_url() {
            self.api.set_base_url(url);
        }
        if synced.theme != self.synced.theme {
            kass_ui::set_theme_choice(theme_choice(&synced.theme), cx);
        }
        self.synced = synced;
        cx.notify();
    }

    /// The beta `network` sync to show, while another Mac's server is set.
    pub fn shared_sync(&self) -> Option<SharedSync> {
        let remote = self.synced.remote_server_url.clone()?;
        let pending = match &self.sync {
            Some(status) if status.standing_in => status.pending.unwrap_or(0),
            _ => 0,
        };
        Some(SharedSync {
            remote,
            away: self.synced.shared_away,
            pending,
        })
    }

    pub fn beta(&self) -> bool {
        self.channel == updater::Channel::Beta
    }

    /// Change capture settings on the server. On success the new settings
    /// show here and the page refetches its copy.
    pub fn update_settings(
        &mut self,
        patch: CaptureSettingsUpdate,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let api = self.api.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let saved = cx
                .background_executor()
                .spawn(async move { api.update_capture_settings(&patch) })
                .await
                .map_err(|e| e.to_string())?;
            this.update(cx, |this, cx| {
                this.settings = Some(saved);
                cx.notify();
            })
            .map_err(|e| e.to_string())?;
            bridge::invalidate(&[&["settings", "captures"], &["capture-readiness"]]);
            Ok(())
        })
    }

    pub fn refresh_update(&mut self, cx: &mut Context<Self>) {
        let app = AppHandle;
        self.update = updater::update_status(app.state::<updater::UpdaterState>());
        self.channel = updater::update_channel(app);
        cx.notify();
    }
}

fn fetch(api: &ApiClient, remote: bool, slow: bool) -> Round {
    let health = api.health().ok();
    let up = health.is_some();
    Round {
        settings: up.then(|| api.capture_settings().ok()).flatten(),
        readiness: up.then(|| api.capture_readiness().ok()).flatten(),
        downloads: up
            .then(|| api.active_tasks().ok().map(|tasks| tasks.downloads))
            .flatten(),
        // This Mac's server knows whether it's standing in for the other one.
        sync: remote.then(|| api.local_sync_status().ok()).flatten(),
        examples: (up && slow)
            .then(|| api.writing_style(None).ok().map(|s| s.example_count))
            .flatten(),
        devices: slow
            .then(|| crate::dictation::capture::list_input_devices().ok())
            .flatten(),
        health,
    }
}

pub fn theme_choice(theme: &str) -> kass_ui::ThemeChoice {
    match theme {
        "light" => kass_ui::ThemeChoice::Light,
        "dark" => kass_ui::ThemeChoice::Dark,
        _ => kass_ui::ThemeChoice::System,
    }
}

/// Run blocking work off the main thread and come back with its result.
pub fn background<R: Send + 'static>(
    cx: &App,
    work: impl FnOnce() -> R + Send + 'static,
) -> Task<R> {
    cx.background_executor().spawn(async move { work() })
}
