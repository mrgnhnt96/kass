//! What the shell and the native settings pages show, refreshed in the
//! background: the server's health and settings, model readiness and
//! downloads, the shared Mac's sync, permissions and the updater. Also
//! checks whether the other Mac in use is away (beta `network`).

use std::time::{Duration, Instant};

use gpui_kit::{App, AsyncApp, Context, Task, WeakEntity};
use kass_api::types::{
    ActiveDownloadTask, CaptureReadinessResponse, CaptureSettings, CaptureSettingsUpdate,
    HealthResponse, SyncStatus,
};
use kass_api::{ApiClient, Mac, LOCAL_SERVER_URL};

use crate::dictation::capture::NativeInputDevice;
use crate::host::{AppHandle, Manager};
use crate::prefs::{self, Prefs};
use crate::{this_mac, updater};

/// How often the server is asked again, as the page's queries did.
const POLL: Duration = Duration::from_secs(5);
/// How often the other Mac in use is checked, and how long it has to
/// answer. Macs on one network answer in milliseconds.
const AWAY_CHECK: Duration = Duration::from_secs(3);
const AWAY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Checks in a row that must go unanswered before it's called away: one
/// late reply (a busy or paged-out Mac) isn't gone, and switching to this
/// Mac's own, slower models for it costs more than waiting a few seconds.
const AWAY_MISSES: u32 = 3;
/// While it's away, how often to look for it under a changed network name.
const RENAMED_SEARCH: Duration = Duration::from_secs(30);
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
    /// Requests go to [`ShellData::server_url`]; clones share it.
    pub api: ApiClient,
    pub prefs: Prefs,
    /// The other Mac in use can't be reached; this Mac's server stands in.
    pub away: bool,
    /// When the other Mac went away, while it is.
    pub away_since: Option<Instant>,
    /// "Keep waiting" was chosen for this time it's away.
    pub away_dismissed: bool,
    /// "Use this Mac instead" is restarting Kass.
    pub switching_to_this_mac: bool,
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
    /// Why this Mac's server didn't start at launch.
    pub startup_error: Option<String>,
    /// Unfinished onboarding was opened this launch.
    onboarding_checked: bool,
    _poll: Task<()>,
    _away: Task<()>,
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
                let Ok((api, remote)) =
                    this.read_with(cx, |this, _| (this.api.clone(), this.remote()))
                else {
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
        let prefs = prefs::get();
        kass_ui::set_theme_choice(theme_choice(&prefs.theme), cx);
        let channel = updater::update_channel(AppHandle);
        let mut data = Self {
            api: ApiClient::new(LOCAL_SERVER_URL, mac),
            prefs,
            away: prefs::away(),
            away_since: prefs::away().then(Instant::now),
            away_dismissed: false,
            switching_to_this_mac: false,
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
            channel,
            version: app.package_info().version,
            startup_error: None,
            onboarding_checked: false,
            _poll: poll,
            _away: cx.spawn(check_away),
        };
        data.follow_prefs(cx);
        data
    }

    fn apply(&mut self, round: Round, slow: bool) {
        self.health = match round.health {
            Some(health) => Health::Online(health),
            None => Health::Offline,
        };
        if round.settings.is_some() {
            self.settings = round.settings;
            self.open_unfinished_onboarding();
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

    /// Once per launch, when the server says onboarding isn't finished,
    /// open it where it was (such as after the quit Input Monitoring needs).
    fn open_unfinished_onboarding(&mut self) {
        let Some(settings) = &self.settings else {
            return;
        };
        if self.onboarding_checked {
            return;
        }
        self.onboarding_checked = true;
        if !settings.onboarding_completed {
            let _ = crate::open_onboarding(AppHandle, true);
        }
    }

    /// The other Mac in use: only for beta users.
    pub fn remote(&self) -> Option<String> {
        self.prefs.remote(self.beta()).map(str::to_string)
    }

    /// Where requests go: the other Mac's server, or this Mac's while it's
    /// away or none is set.
    pub fn server_url(&self) -> String {
        match self.remote() {
            Some(remote) if !self.away => remote,
            _ => LOCAL_SERVER_URL.to_string(),
        }
    }

    /// The preferences, the channel or the other Mac's presence changed:
    /// follow the server and the theme.
    pub fn follow_prefs(&mut self, cx: &mut Context<Self>) {
        let prefs = prefs::get();
        if prefs.theme != self.prefs.theme {
            kass_ui::set_theme_choice(theme_choice(&prefs.theme), cx);
        }
        self.prefs = prefs;
        self.away = prefs::away();
        if !self.away {
            self.away_since = None;
            self.away_dismissed = false;
        } else if self.away_since.is_none() {
            self.away_since = Some(Instant::now());
        }
        self.channel = updater::update_channel(AppHandle);
        let url = self.server_url();
        if url != self.api.base_url() {
            self.api.set_base_url(url);
        }
        cx.notify();
    }

    /// The beta `network` sync to show, while another Mac's server is set.
    pub fn shared_sync(&self) -> Option<SharedSync> {
        let remote = self.remote()?;
        let pending = match &self.sync {
            Some(status) if status.standing_in => status.pending.unwrap_or(0),
            _ => 0,
        };
        Some(SharedSync {
            remote,
            away: self.away,
            pending,
        })
    }

    pub fn beta(&self) -> bool {
        self.channel == updater::Channel::Beta
    }

    /// Change capture settings on the server. On success the new settings
    /// show here.
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

/// Beta `network`: while using another Mac's Kass, check it every few
/// seconds. When it doesn't answer, this Mac's own server stands in: the
/// requests go there, its models load, and new takes start there. When it's
/// back, everything goes back to it; this Mac's server sends it what was
/// done meanwhile (backend/services/sync).
///
/// Its Kass says which Mac it's on, and that id is remembered. When its
/// network name changes (macOS makes `studio.local` into `studio-2.local`
/// when the name is taken), it's looked for under the names macOS gives
/// and followed to the one with the same id.
async fn check_away(this: WeakEntity<ShellData>, cx: &mut AsyncApp) {
    // This Mac's server may still be starting when the other Mac is first
    // found away, so it's asked to load its models until it has.
    let mut woken = false;
    let mut searched: Option<Instant> = None;
    let mut misses: u32 = 0;
    loop {
        let Ok((remote, api)) = this.read_with(cx, |this, _| (this.remote(), this.api.clone()))
        else {
            return;
        };
        let saved = prefs::get().remote_mac_id;
        let remote_none = remote.is_none();
        let away = match remote {
            None => false,
            Some(remote) => {
                let probe = remote.clone();
                let answer = cx
                    .background_executor()
                    .spawn(async move { kass_api::kass_at(&probe, AWAY_TIMEOUT) })
                    .await;
                let found = match judge(answer, saved.as_deref()) {
                    Answer::Up { remember } => {
                        if let Some(id) = remember {
                            prefs::remember_remote_mac(id);
                        }
                        true
                    }
                    Answer::Away => match saved {
                        Some(id) if searched.is_none_or(|at| at.elapsed() >= RENAMED_SEARCH) => {
                            searched = Some(Instant::now());
                            let moved = cx
                                .background_executor()
                                .spawn(async move {
                                    let urls = kass_api::renamed_addresses(&remote);
                                    kass_api::find_kass(&urls, &id, AWAY_TIMEOUT)
                                })
                                .await;
                            if let Some(url) = &moved {
                                eprintln!(
                                    "[network] The other Mac's name changed; now using {url}"
                                );
                                prefs::follow_remote(url.clone());
                                searched = None;
                            }
                            moved.is_some()
                        }
                        _ => false,
                    },
                };
                if found {
                    woken = false;
                } else if !woken {
                    woken = cx
                        .background_executor()
                        .spawn(async move { api.local_sync_wake().is_ok() })
                        .await;
                }
                let (next, away) = count_miss(misses, found);
                misses = next;
                away
            }
        };
        if remote_none {
            misses = 0;
        }
        if prefs::set_away(away) {
            eprintln!(
                "[network] {}",
                if away {
                    "The other Mac is away; dictating on this one"
                } else {
                    "The other Mac is back"
                }
            );
        }
        cx.background_executor().timer(AWAY_CHECK).await;
    }
}

/// Counts a check: `misses` unanswered ones in a row so far, `found`
/// whether this one was answered. Returns the new count and whether the
/// other Mac is away. An answer clears the count and brings it back at once.
fn count_miss(misses: u32, found: bool) -> (u32, bool) {
    if found {
        return (0, false);
    }
    let misses = misses.saturating_add(1).min(AWAY_MISSES);
    (misses, misses >= AWAY_MISSES)
}

/// What the other Mac's address answering means.
#[derive(Debug, PartialEq)]
enum Answer {
    /// It's there, with its id to remember the first time.
    Up { remember: Option<String> },
    /// Nothing answers as Kass, or another Mac's Kass took the name.
    Away,
}

/// `answer` is [`kass_api::kass_at`]'s, `saved` the id remembered for the
/// other Mac.
fn judge(answer: Option<Option<String>>, saved: Option<&str>) -> Answer {
    match (answer, saved) {
        (None, _) => Answer::Away,
        (Some(Some(id)), Some(saved)) if id != saved => Answer::Away,
        (Some(Some(id)), None) => Answer::Up { remember: Some(id) },
        // An older Kass doesn't say which Mac it's on.
        (Some(_), _) => Answer::Up { remember: None },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(id: &str) -> Option<Option<String>> {
        Some(Some(id.into()))
    }

    #[test]
    fn the_other_macs_id_is_remembered_the_first_time() {
        assert_eq!(
            judge(id("m-1"), None),
            Answer::Up {
                remember: Some("m-1".into())
            }
        );
        assert_eq!(judge(id("m-1"), Some("m-1")), Answer::Up { remember: None });
    }

    #[test]
    fn another_macs_kass_under_its_old_name_is_not_it() {
        assert_eq!(judge(id("m-2"), Some("m-1")), Answer::Away);
    }

    #[test]
    fn one_or_two_misses_are_not_away() {
        assert_eq!(count_miss(0, false), (1, false));
        assert_eq!(count_miss(1, false), (2, false));
    }

    #[test]
    fn the_third_miss_in_a_row_is_away_and_stays() {
        assert_eq!(count_miss(2, false), (3, true));
        assert_eq!(count_miss(3, false), (3, true));
    }

    #[test]
    fn one_answer_clears_the_misses_and_brings_it_back() {
        assert_eq!(count_miss(2, true), (0, false));
        assert_eq!(count_miss(3, true), (0, false));
        // Misses must be consecutive: an answer in between starts over.
        let (m, _) = count_miss(2, true);
        assert_eq!(count_miss(m, false), (1, false));
    }

    #[test]
    fn nothing_answering_is_away() {
        assert_eq!(judge(None, Some("m-1")), Answer::Away);
        assert_eq!(judge(None, None), Answer::Away);
    }

    #[test]
    fn an_older_kass_that_names_no_mac_is_taken_at_its_word() {
        assert_eq!(
            judge(Some(None), Some("m-1")),
            Answer::Up { remember: None }
        );
        assert_eq!(judge(Some(None), None), Answer::Up { remember: None });
    }
}
