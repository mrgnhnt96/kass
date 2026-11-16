//! Settings › General: the app (version, updates, the local server, restart,
//! launch at login, theme), other Macs (beta), storage and privacy.

use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::InputState;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{CaptureSettings, CaptureSettingsUpdate, HistoryRetentionDays};
use kass_ui::{
    metrics, open_confirm, select, select_state, show_toast, text_input, theme, Button, Confirm,
    SelectOption, SelectOptions, SettingRow, SettingSection, Toast, ToastKind, Toggle,
};

use super::toast_error;
use crate::host::{self, AppHandle, Manager};
use crate::login_item::{self, Status as LoginStatus};
use crate::prefs;
use crate::shell::bridge;
use crate::shell::data::{self, Health, ShellData};
use crate::shell::status;
use crate::updater::{self, UpdateStatus};
use crate::ServerState;

const G: &str = "settings-general";
const STORE: &str = "settings-captures-storage";
pub(crate) const KEEP: &str = "settings-captures-storage-retention";

/// How long to wait for another Mac's Kass to answer before giving up.
const REACH_TIMEOUT: Duration = Duration::from_secs(5);

const THEMES: [&str; 3] = ["system", "light", "dark"];

/// The history choices, shortest first, then forever.
pub(crate) const RETENTION: [(u16, &str); 5] = [
    (7, "d7"),
    (30, "d30"),
    (90, "d90"),
    (365, "d365"),
    (0, "forever"),
];

fn t(id: impl AsRef<str>) -> String {
    kass_i18n::t(id.as_ref())
}

pub(crate) fn retention_label(days: u16) -> String {
    let key = RETENTION
        .iter()
        .find(|(d, _)| *d == days)
        .map_or("d30", |(_, key)| key);
    t(format!("{KEEP}-{key}"))
}

/// Whether keeping `next` days of history deletes captures `current` keeps.
pub fn is_shorter(next: u16, current: u16) -> bool {
    next != 0 && (current == 0 || next < current)
}

/// The version row's text, from the update state.
fn version_description(version: &str, update: &UpdateStatus, up_to_date: bool) -> String {
    match update {
        UpdateStatus::Ready { version: latest } => {
            kass_i18n::tr!(
                &format!("{G}-version-ready"),
                current = version,
                latest = latest.as_str()
            )
        }
        UpdateStatus::Downloading { version: latest } => kass_i18n::tr!(
            &format!("{G}-version-downloading"),
            current = version,
            latest = latest.as_str()
        ),
        UpdateStatus::Current if up_to_date => {
            kass_i18n::tr!(&format!("{G}-version-up-to-date"), version = version)
        }
        UpdateStatus::Current => kass_i18n::tr!(&format!("{G}-version-current"), version = version),
    }
}

pub struct GeneralPage {
    data: Entity<ShellData>,
    theme: Entity<SelectState<SelectOptions<String>>>,
    retention: Entity<SelectState<SelectOptions<u16>>>,
    remote: Entity<InputState>,
    /// A check you started is running.
    checking: bool,
    /// A check you started found nothing newer.
    up_to_date: bool,
    updating: bool,
    restarting: bool,
    beta_busy: bool,
    login: LoginStatus,
    login_busy: bool,
    sharing_busy: bool,
    connecting: bool,
    /// This Mac's address on the network, once asked for.
    address: Option<String>,
    captures_dir: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl GeneralPage {
    pub fn new(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current_theme = data.read(cx).prefs.theme.clone();
        let themes = THEMES
            .iter()
            .map(|choice| {
                SelectOption::new(
                    choice.to_string(),
                    t(format!("settings-theme-options-{choice}")),
                )
            })
            .collect();
        let theme = cx.new(|cx| select_state(themes, Some(&current_theme), window, cx));

        let days = Self::days(&data, cx);
        let choices = RETENTION
            .iter()
            .map(|&(days, key)| SelectOption::new(days, t(format!("{KEEP}-{key}"))))
            .collect();
        let retention = cx.new(|cx| select_state(choices, Some(&days), window, cx));

        let remote = cx.new(|cx| InputState::new(window, cx).placeholder("studio.local"));

        let subscriptions = vec![
            cx.subscribe(&theme, |this, _, event: &SelectEvent<_>, cx| {
                if let SelectEvent::Confirm(Some(choice)) = event {
                    this.set_theme(choice.clone(), cx);
                }
            }),
            cx.subscribe_in(
                &retention,
                window,
                |this, _, event: &SelectEvent<_>, window, cx| {
                    if let SelectEvent::Confirm(Some(days)) = event {
                        this.choose_retention(*days, window, cx);
                    }
                },
            ),
            cx.observe_in(&data, window, |this, _, window, cx| {
                this.sync_selects(window, cx);
                if this.address.is_none() && this.data.read(cx).prefs.share_on_network {
                    this.load_address(cx);
                }
                cx.notify();
            }),
            // Launch at login can change in System Settings while Kass is behind.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.login = login_item::launch_at_login_status();
                    cx.notify();
                }
            }),
        ];

        let mut page = Self {
            data,
            theme,
            retention,
            remote,
            checking: false,
            up_to_date: false,
            updating: false,
            restarting: false,
            beta_busy: false,
            login: login_item::launch_at_login_status(),
            login_busy: false,
            sharing_busy: false,
            connecting: false,
            address: None,
            captures_dir: None,
            _subscriptions: subscriptions,
        };
        page.load_captures_dir(cx);
        if page.data.read(cx).prefs.share_on_network {
            page.load_address(cx);
        }
        page
    }

    fn days(data: &Entity<ShellData>, cx: &App) -> u16 {
        data.read(cx)
            .settings
            .as_ref()
            .map_or(30, |s| s.history_retention_days.days())
    }

    /// Keep the dropdowns on what's saved, whoever changed it.
    fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme_choice = self.data.read(cx).prefs.theme.clone();
        let days = Self::days(&self.data, cx);
        self.theme.update(cx, |state, cx| {
            if state.selected_value() != Some(&theme_choice) {
                state.set_selected_value(&theme_choice, window, cx);
            }
        });
        self.retention.update(cx, |state, cx| {
            if state.selected_value() != Some(&days) {
                state.set_selected_value(&days, window, cx);
            }
        });
    }

    fn load_captures_dir(&mut self, cx: &mut Context<Self>) {
        let api = self.data.read(cx).api.clone();
        let task = data::background(cx, move || api.filesystem_health().ok());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let dir = task
                .await
                .and_then(|h| h.captures_dir().map(str::to_string));
            let _ = this.update(cx, |this, cx| {
                this.captures_dir = dir;
                cx.notify();
            });
        })
        .detach();
    }

    fn load_address(&mut self, cx: &mut Context<Self>) {
        let task = data::background(cx, || crate::network_address().ok());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let address = task.await;
            let _ = this.update(cx, |this, cx| {
                this.address = address;
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, patch: CaptureSettingsUpdate, window: &mut Window, cx: &mut Context<Self>) {
        let task = self
            .data
            .update(cx, |data, cx| data.update_settings(patch, cx));
        // Like the page, a failed save just leaves what the server kept.
        cx.spawn_in(window, async move |_, _| {
            if let Err(e) = task.await {
                eprintln!("[settings] save failed: {e}");
            }
        })
        .detach();
    }

    fn toggle(
        &self,
        id: &'static str,
        checked: bool,
        disabled: bool,
        patch: fn(bool) -> CaptureSettingsUpdate,
        cx: &mut Context<Self>,
    ) -> Toggle {
        let this = cx.entity().downgrade();
        Toggle::new(id)
            .checked(checked)
            .disabled(disabled)
            .on_change(move |value, window, cx| {
                let value = *value;
                let _ = this.update(cx, |this, cx| this.save(patch(value), window, cx));
            })
    }

    /// Run `command` (src/prefs.rs); `busy` is set until it's done.
    fn command(
        &mut self,
        command: impl Future<Output = Result<(), String>> + Send + 'static,
        busy: fn(&mut Self) -> &mut bool,
        failed: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        *busy(self) = true;
        cx.notify();
        let command = host::async_runtime::spawn(command);
        cx.spawn_in(window, async move |this, cx| {
            let result = command.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                *busy(this) = false;
                if let Err(e) = result {
                    toast_error(t(failed), &e, window, cx);
                }
                this.data.update(cx, |data, cx| data.refresh_update(cx));
                cx.notify();
            });
        })
        .detach();
    }

    fn set_theme(&mut self, choice: String, cx: &mut Context<Self>) {
        let Some(&choice) = THEMES.iter().find(|c| **c == choice) else {
            return;
        };
        // Show it now; every window follows once it's saved.
        kass_ui::set_theme_choice(data::theme_choice(choice), cx);
        prefs::set_theme(choice);
    }

    fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.checking = true;
        self.up_to_date = false;
        cx.notify();
        let check = host::async_runtime::spawn(updater::check_for_updates(AppHandle));
        cx.spawn_in(window, async move |this, cx| {
            let result = check.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.checking = false;
                match result {
                    Ok(status) => this.up_to_date = status == UpdateStatus::Current,
                    Err(e) => toast_error(t(format!("{G}-version-check-failed")), &e, window, cx),
                }
                this.data.update(cx, |data, cx| data.refresh_update(cx));
                cx.notify();
            });
        })
        .detach();
    }

    fn restart_to_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.updating = true;
        cx.notify();
        let restart = host::async_runtime::spawn(async {
            updater::restart_to_update(AppHandle.state::<ServerState>()).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = restart.await.unwrap_or_else(|e| Err(e.to_string()));
            if let Err(e) = result {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.updating = false;
                    toast_error(t(format!("{G}-restart-failed")), &e, window, cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restarting = true;
        cx.notify();
        let restart = host::async_runtime::spawn(async {
            crate::restart_app(AppHandle.state::<ServerState>()).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = restart.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.restarting = false;
                if let Err(e) = result {
                    toast_error(t(format!("{G}-restart-failed")), &e, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_launch_at_login(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.login_busy = true;
        cx.notify();
        let task = data::background(cx, move || login_item::set_launch_at_login(on));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.login_busy = false;
                match result {
                    Ok(status) => this.login = status,
                    Err(e) => toast_error(t(format!("{G}-launch-at-login-failed")), &e, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keep less history: ask first when that deletes captures.
    fn choose_retention(&mut self, days: u16, window: &mut Window, cx: &mut Context<Self>) {
        let current = Self::days(&self.data, cx);
        if days == current {
            return;
        }
        let patch = CaptureSettingsUpdate {
            history_retention_days: Some(HistoryRetentionDays::from(days)),
            ..Default::default()
        };
        if !is_shorter(days, current) {
            self.save(patch, window, cx);
            return;
        }
        // The dropdown shows what's kept until you confirm.
        self.sync_selects(window, cx);
        let api = self.data.read(cx).api.clone();
        let preview = data::background(cx, move || {
            api.retention_preview(HistoryRetentionDays::from(days))
                .ok()
                .map(|p| p.expiring)
        });
        cx.spawn_in(window, async move |this, cx| {
            let expiring = preview.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if expiring == Some(0) {
                    this.save(patch, window, cx);
                    return;
                }
                let window_label = retention_label(days);
                let (description, confirm) = match expiring {
                    Some(count) => (
                        kass_i18n::tr!(
                            &format!("{KEEP}-confirm-description"),
                            count = count,
                            window = window_label.as_str()
                        ),
                        kass_i18n::tr!(&format!("{KEEP}-confirm-delete"), count = count),
                    ),
                    None => (
                        kass_i18n::tr!(
                            &format!("{KEEP}-confirm-unknown"),
                            window = window_label.as_str()
                        ),
                        t(format!("{KEEP}-confirm-delete-unknown")),
                    ),
                };
                let page = cx.entity().downgrade();
                open_confirm(
                    window,
                    cx,
                    Confirm {
                        title: kass_i18n::tr!(
                            &format!("{KEEP}-confirm-title"),
                            window = window_label.as_str()
                        )
                        .into(),
                        description: Some(description.into()),
                        confirm: confirm.into(),
                        cancel: t("common-cancel").into(),
                        destructive: true,
                        on_confirm: Rc::new(move |window, cx| {
                            let patch = CaptureSettingsUpdate {
                                history_retention_days: Some(HistoryRetentionDays::from(days)),
                                ..Default::default()
                            };
                            let _ = page.update(cx, |page, cx| {
                                page.save(patch, window, cx);
                                bridge::invalidate(&[&["captures"]]);
                            });
                        }),
                    },
                );
            });
        })
        .detach();
    }

    /// Use the other Mac at what's typed, once it answers as Kass.
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.remote.read(cx).value().to_string();
        let Some(url) = kass_api::normalize_server_address(&typed) else {
            show_toast(
                window,
                cx,
                Toast::new(t(format!("{G}-network-remote-invalid"))).kind(ToastKind::Destructive),
            );
            return;
        };
        self.connecting = true;
        cx.notify();
        let probe = url.clone();
        let reach = data::background(cx, move || kass_api::answers_as_kass(&probe, REACH_TIMEOUT));
        cx.spawn_in(window, async move |this, cx| {
            let reached = reach.await;
            if !reached {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.connecting = false;
                    show_toast(
                        window,
                        cx,
                        Toast::new(kass_i18n::tr!(
                            &format!("{G}-network-remote-unreachable"),
                            url = url.as_str()
                        ))
                        .description(t(format!("{G}-network-remote-unreachable-hint")))
                        .kind(ToastKind::Destructive),
                    );
                    cx.notify();
                });
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.command(
                    prefs::use_remote(Some(url)),
                    |this| &mut this.connecting,
                    "settings-general-restart-failed",
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn app_section(&self, cx: &mut Context<Self>) -> SettingSection {
        let data = self.data.read(cx);
        let version = data.version.clone();
        let update = data.update.clone();
        let beta = data.beta();
        let health = data.health.clone();
        let theme_t = theme(cx);

        let version_action = match &update {
            UpdateStatus::Ready { .. } => Button::new("restart-to-update")
                .small()
                .label(t(if self.updating {
                    format!("{G}-version-restarting")
                } else {
                    format!("{G}-version-restart")
                }))
                .disabled(self.updating)
                .on_click(cx.listener(|this, _, window, cx| this.restart_to_update(window, cx))),
            _ => Button::new("check-updates")
                .outline()
                .small()
                .label(t(if self.checking {
                    format!("{G}-version-checking")
                } else {
                    format!("{G}-version-check")
                }))
                .disabled(self.checking || matches!(update, UpdateStatus::Downloading { .. }))
                .on_click(cx.listener(|this, _, window, cx| this.check_for_updates(window, cx))),
        };

        let beta_description = if !beta && version.contains("-beta.") {
            kass_i18n::tr!(
                &format!("{G}-beta-updates-leaving"),
                version = version.as_str()
            )
        } else {
            t(format!("{G}-beta-updates-description"))
        };

        let (pill_label, pill_fg, pill_bg, dot) = match &health {
            Health::Connecting => (
                t(format!("{G}-connection-connecting")),
                theme_t.muted_foreground,
                theme_t.muted,
                false,
            ),
            Health::Offline => (
                t(format!("{G}-connection-offline")),
                theme_t.destructive,
                theme_t.destructive.opacity(0.1),
                true,
            ),
            Health::Online(_) => (
                t(format!("{G}-connection-online")),
                theme_t.success,
                theme_t.success.opacity(0.1),
                true,
            ),
        };
        let pill = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .py(px(4.))
            .rounded_full()
            .bg(pill_bg)
            .text_color(pill_fg)
            .font_family(theme_t.mono_font.clone())
            .text_size(metrics::TEXT_XS)
            .when(dot, |p| {
                p.child(div().size(px(6.)).rounded_full().bg(pill_fg))
            })
            .child(pill_label);

        let stats = match &health {
            Health::Online(health) => status::server_stats(health, status::unix_now()),
            _ => Vec::new(),
        };
        let (card, border, muted) = (theme_t.card, theme_t.border, theme_t.muted_foreground);
        let mono = theme_t.mono_font.clone();
        let stat_rows = stats
            .chunks(3)
            .map(|row| {
                div()
                    .flex()
                    .gap(px(8.))
                    .children(row.iter().map(|(label, value)| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(12.))
                            .py(px(10.))
                            .rounded(metrics::RADIUS_LG)
                            .border_1()
                            .border_color(border)
                            .bg(card)
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(10.))
                                    .text_color(muted)
                                    .child(label.to_uppercase()),
                            )
                            .child(div().text_size(px(13.)).truncate().child(value.clone()))
                    }))
                    .children((row.len()..3).map(|_| div().flex_1()))
            })
            .collect::<Vec<_>>();

        let login_action = match self.login {
            LoginStatus::RequiresApproval => Button::new("open-login-items")
                .outline()
                .small()
                .label(t(format!("{G}-launch-at-login-open-login-items")))
                .on_click(|_, window, cx| {
                    if let Err(e) = login_item::open_login_items_settings() {
                        toast_error(t(format!("{G}-launch-at-login-failed")), &e, window, cx);
                    }
                })
                .into_any_element(),
            status => Toggle::new("launch-at-login")
                .checked(status == LoginStatus::Enabled)
                .disabled(self.login_busy || status == LoginStatus::Unavailable)
                .on_change(cx.listener(|this, on: &bool, window, cx| {
                    this.set_launch_at_login(*on, window, cx)
                }))
                .into_any_element(),
        };
        let login_description = match self.login {
            LoginStatus::Unavailable => t(format!("{G}-launch-at-login-unavailable")),
            LoginStatus::RequiresApproval => t(format!("{G}-launch-at-login-requires-approval")),
            _ => t(format!("{G}-launch-at-login-description")),
        };

        SettingSection::new(t(format!("{G}-section-app")))
            .child(
                SettingRow::new(t(format!("{G}-version-title")))
                    .description(version_description(&version, &update, self.up_to_date))
                    .action(version_action),
            )
            .child(
                SettingRow::new(t(format!("{G}-beta-updates-title")))
                    .description(beta_description)
                    .action(
                        Toggle::new("beta-updates")
                            .checked(beta)
                            .disabled(self.beta_busy)
                            .on_change(cx.listener(|this, on: &bool, window, cx| {
                                this.command(
                                    prefs::set_beta(*on),
                                    |this| &mut this.beta_busy,
                                    "settings-general-beta-updates-failed",
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(
                SettingRow::new(t(format!("{G}-server-title")))
                    .description(t(format!("{G}-server-description")))
                    .action(pill)
                    .when(!stat_rows.is_empty(), |row| {
                        row.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .pt(px(12.))
                                .children(stat_rows),
                        )
                    }),
            )
            .child(
                SettingRow::new(t(format!("{G}-restart-title")))
                    .description(t(format!("{G}-restart-description")))
                    .action(
                        Button::new("restart-kass")
                            .outline()
                            .small()
                            .label(t(if self.restarting {
                                format!("{G}-restart-busy")
                            } else {
                                format!("{G}-restart-action")
                            }))
                            .disabled(self.restarting)
                            .on_click(cx.listener(|this, _, window, cx| this.restart(window, cx))),
                    ),
            )
            .child(
                SettingRow::new(t(format!("{G}-launch-at-login-title")))
                    .description(login_description)
                    .action(login_action),
            )
            .child(
                SettingRow::new(t("settings-theme-label"))
                    .description(t("settings-theme-description"))
                    .action(div().w(px(160.)).child(select(&self.theme, cx))),
            )
    }

    fn network_section(&self, cx: &mut Context<Self>) -> Option<SettingSection> {
        let data = self.data.read(cx);
        if !data.beta() {
            return None;
        }
        let share_on_network = data.prefs.share_on_network;
        let remote = data.remote();

        let share_row = remote.is_none().then(|| {
            let description = match (&self.address, share_on_network) {
                (Some(address), true) => {
                    kass_i18n::tr!(&format!("{G}-network-share-on"), address = address.as_str())
                }
                _ => t(format!("{G}-network-share-description")),
            };
            SettingRow::new(t(format!("{G}-network-share-title")))
                .description(description)
                .action(
                    Toggle::new("share-on-network")
                        .checked(share_on_network)
                        .disabled(self.sharing_busy)
                        .on_change(cx.listener(|this, on: &bool, window, cx| {
                            this.command(
                                prefs::set_share_on_network(*on),
                                |this| &mut this.sharing_busy,
                                "settings-general-network-share-failed",
                                window,
                                cx,
                            )
                        })),
                )
        });

        let remote_row = match remote {
            Some(url) => SettingRow::new(t(format!("{G}-network-remote-title")))
                .description(kass_i18n::tr!(
                    &format!("{G}-network-remote-using"),
                    url = url.as_str()
                ))
                .action(
                    Button::new("use-this-mac")
                        .outline()
                        .small()
                        .label(t(format!("{G}-network-remote-stop")))
                        .disabled(self.connecting)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.command(
                                prefs::use_remote(None),
                                |this| &mut this.connecting,
                                "settings-general-restart-failed",
                                window,
                                cx,
                            )
                        })),
                ),
            None => SettingRow::new(t(format!("{G}-network-remote-title")))
                .description(t(format!("{G}-network-remote-description")))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .pt(px(12.))
                        .child(div().flex_1().child(text_input(&self.remote, cx)))
                        .child(
                            Button::new("connect-remote")
                                .outline()
                                .label(t(if self.connecting {
                                    format!("{G}-network-remote-connecting")
                                } else {
                                    format!("{G}-network-remote-connect")
                                }))
                                .disabled(self.connecting)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.connect(window, cx)),
                                ),
                        ),
                ),
        };

        Some(
            SettingSection::new(t(format!("{G}-network-title")))
                .description(t(format!("{G}-network-description")))
                .children(share_row)
                .child(remote_row),
        )
    }

    fn storage_section(
        &self,
        settings: Option<&CaptureSettings>,
        cx: &mut Context<Self>,
    ) -> SettingSection {
        let loaded = settings.is_some();
        let discard = settings.is_some_and(|s| s.discard_audio);
        let mono = theme(cx).mono_font.clone();
        let folder = self.captures_dir.clone();
        SettingSection::new(t(format!("{STORE}-title")))
            .description(t(format!("{STORE}-description")))
            .child(
                SettingRow::new(t(format!("{KEEP}-title")))
                    .description(t(format!("{KEEP}-description")))
                    .action(div().w(px(160.)).child(select(&self.retention, cx))),
            )
            .child(
                SettingRow::new(t(format!("{STORE}-recordings-title")))
                    .description(t(format!("{STORE}-recordings-description")))
                    .action(self.toggle(
                        "discard-audio",
                        discard,
                        !loaded,
                        |v| CaptureSettingsUpdate {
                            discard_audio: Some(v),
                            ..Default::default()
                        },
                        cx,
                    )),
            )
            .child(
                SettingRow::new(t(format!("{STORE}-folder-title")))
                    .description(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(t(format!("{STORE}-folder-description")))
                            .when_some(folder.clone(), |d, path| {
                                d.child(div().font_family(mono).text_size(px(11.)).child(path))
                            }),
                    )
                    .action(
                        Button::new("open-captures-folder")
                            .outline()
                            .small()
                            .label(t(format!("{STORE}-folder-open")))
                            .disabled(folder.is_none())
                            .on_click(move |_, window, cx| {
                                if let Some(path) = &folder {
                                    if let Err(e) = host::open_external(path) {
                                        toast_error(
                                            t(format!("{STORE}-folder-title")),
                                            &e.to_string(),
                                            window,
                                            cx,
                                        );
                                    }
                                }
                            }),
                    ),
            )
    }
}

impl Render for GeneralPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.data.read(cx).settings.clone();
        let loaded = settings.is_some();
        let share_usage = settings.as_ref().is_none_or(|s| s.share_usage);
        let app = self.app_section(cx);
        let network = self.network_section(cx);
        let storage = self.storage_section(settings.as_ref(), cx);
        let privacy = SettingSection::new(t(format!("{G}-section-privacy"))).child(
            SettingRow::new(t(format!("{G}-share-usage-title")))
                .description(t(format!("{G}-share-usage-description")))
                .action(self.toggle(
                    "share-usage",
                    share_usage,
                    !loaded,
                    |v| CaptureSettingsUpdate {
                        share_usage: Some(v),
                        ..Default::default()
                    },
                    cx,
                )),
        );
        div()
            .flex()
            .flex_col()
            .child(app)
            .children(network)
            .child(storage)
            .child(privacy)
    }
}

#[cfg(test)]
mod tests {
    use super::{is_shorter, version_description};
    use crate::updater::UpdateStatus;

    #[test]
    fn shorter_history_asks_first() {
        assert!(is_shorter(7, 30));
        assert!(is_shorter(365, 0));
        assert!(!is_shorter(0, 30));
        assert!(!is_shorter(90, 30));
        assert!(!is_shorter(30, 30));
    }

    #[test]
    fn version_says_what_the_updater_has() {
        assert_eq!(
            version_description("0.9.0", &UpdateStatus::Current, false),
            "Kass 0.9.0"
        );
        assert_eq!(
            version_description("0.9.0", &UpdateStatus::Current, true),
            "Kass 0.9.0. You're up to date."
        );
        assert_eq!(
            version_description(
                "0.9.0",
                &UpdateStatus::Ready {
                    version: "0.9.1".into()
                },
                false
            ),
            "Kass 0.9.0. 0.9.1 is downloaded and installs when Kass restarts."
        );
    }
}
