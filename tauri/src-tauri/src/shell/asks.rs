//! What the main window asks once per launch, over whatever screen shows:
//! to move Kass into Applications when it runs from anywhere else, and how
//! long to keep history before retention first deletes anything
//! (docs/plans/HISTORY_RETENTION.md). One dialog at a time.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{CaptureSettingsUpdate, HistoryRetentionDays};
use kass_ui::{
    close_dialog, dialog_footer, dialog_header, metrics, open_dialog, select, select_state, theme,
    Button, SelectOption, SelectOptions,
};

use super::bridge;
use super::data::{self, Health, ShellData};
use crate::app_location::{self, AppLocation};
use crate::host::{self, AppHandle, Manager};
use crate::settings::general::{retention_label, KEEP, RETENTION};
use crate::ServerState;

/// Asked this launch already: reopening the window doesn't ask again.
static LOCATION_ASKED: AtomicBool = AtomicBool::new(false);
static RETENTION_ASKED: AtomicBool = AtomicBool::new(false);

fn t(id: impl AsRef<str>) -> String {
    kass_i18n::t(id.as_ref())
}

pub struct Asks {
    data: Entity<ShellData>,
    /// The retention status is being fetched.
    checking: bool,
    _subscription: Subscription,
}

impl Asks {
    pub fn new(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription =
            cx.observe_in(&data, window, |this, _, window, cx| this.check(window, cx));
        Self {
            data,
            checking: false,
            _subscription: subscription,
        }
    }

    /// Runs as the shell's data refreshes, so a question waits for the
    /// server and for the dialog before it to close.
    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if !LOCATION_ASKED.swap(true, Ordering::AcqRel) {
            let location = app_location::location();
            if location.needs_move {
                let view = cx.new(|_| LocationAsk::new(location));
                open_dialog(window, cx, move |_, _| view.clone().into_any_element());
                return;
            }
        }
        if RETENTION_ASKED.load(Ordering::Acquire) || self.checking {
            return;
        }
        let data = self.data.read(cx);
        if !matches!(data.health, Health::Online(_)) {
            return;
        }
        self.checking = true;
        let api = data.api.clone();
        let status = data::background(cx, move || api.retention_status());
        cx.spawn_in(window, async move |this, cx| {
            let status = status.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.checking = false;
                // Asked again on the next refresh.
                let Ok(status) = status else { return };
                if window.has_active_dialog(cx) {
                    return;
                }
                RETENTION_ASKED.store(true, Ordering::Release);
                // With nothing old to delete the server confirms by itself.
                if status.confirmed || status.expiring == 0 {
                    return;
                }
                let data = this.data.clone();
                let view = cx.new(|cx| RetentionAsk::new(data, status, window, cx));
                open_dialog(window, cx, move |_, _| view.clone().into_any_element());
            });
        })
        .detach();
    }
}

/// Kass runs outside `/Applications`, where macOS permissions don't work
/// reliably: it can be missing from the Input Monitoring list, or its switch
/// is on but does nothing. Moving copies the app, puts the old copies in the
/// Trash and relaunches.
struct LocationAsk {
    location: AppLocation,
    moving: bool,
    error: Option<String>,
}

impl LocationAsk {
    fn new(location: AppLocation) -> Self {
        Self {
            location,
            moving: false,
            error: None,
        }
    }

    fn move_app(&mut self, cx: &mut Context<Self>) {
        self.moving = true;
        self.error = None;
        cx.notify();
        let view = cx.entity().downgrade();
        host::async_runtime::spawn(async move {
            // Kass quits and reopens from Applications when this works.
            let state = AppHandle.state::<ServerState>();
            if let Err(error) = crate::move_to_applications(state).await {
                host::with_app(move |cx| {
                    let _ = view.update(cx, |ask, cx| {
                        ask.moving = false;
                        ask.error = Some(error);
                        cx.notify();
                    });
                });
            }
        });
    }
}

impl Render for LocationAsk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        let path = self.location.path.clone().unwrap_or_default();
        let note = |text: String, color: Hsla| {
            div()
                .text_size(metrics::TEXT_SM)
                .line_height(metrics::LINE_SM)
                .text_color(color)
                .child(text)
        };
        let moving = self.moving;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(dialog_header(
                t("app-location-title"),
                Some(kass_i18n::tr!("app-location-description", path = path).into()),
                cx,
            ))
            .when(self.location.replaces_existing, |body| {
                body.child(note(t("app-location-replaces"), theme.muted_foreground))
            })
            .when_some(self.error.clone(), |body, error| {
                body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(note(error, theme.destructive))
                        .child(note(t("app-location-manual"), theme.muted_foreground)),
                )
            })
            .child(
                dialog_footer()
                    .child(
                        Button::new("app-location-later")
                            .ghost()
                            .label(t("app-location-later"))
                            .disabled(moving)
                            .on_click(|_, window, cx| close_dialog(window, cx)),
                    )
                    .child(
                        Button::new("app-location-move")
                            .label(if moving {
                                t("app-location-moving")
                            } else {
                                t("app-location-move")
                            })
                            .disabled(moving)
                            .on_click(cx.listener(|ask, _, _, cx| ask.move_app(cx))),
                    ),
            )
    }
}

/// History retention is about to delete captures for the first time. Any
/// choice saves a window, and the server deletes nothing until one is
/// saved; "Not now" saves nothing, so it asks again next launch.
struct RetentionAsk {
    data: Entity<ShellData>,
    choice: Entity<SelectState<SelectOptions<u16>>>,
    days: u16,
    /// Captures `days` would delete; `None` while counting.
    count: Option<u64>,
    _subscription: Subscription,
}

impl RetentionAsk {
    fn new(
        data: Entity<ShellData>,
        status: kass_api::types::RetentionStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let days = match status.days.days() {
            0 => 30,
            days => days,
        };
        let choices = RETENTION
            .iter()
            .filter(|(days, _)| *days != 0)
            .map(|&(days, key)| SelectOption::new(days, t(format!("{KEEP}-{key}"))))
            .collect();
        let choice = cx.new(|cx| select_state(choices, Some(&days), window, cx));
        let subscription = cx.subscribe(&choice, |ask, _, event: &SelectEvent<_>, cx| {
            if let SelectEvent::Confirm(Some(days)) = event {
                ask.pick(*days, cx);
            }
        });
        Self {
            data,
            choice,
            days,
            count: Some(status.expiring),
            _subscription: subscription,
        }
    }

    fn pick(&mut self, days: u16, cx: &mut Context<Self>) {
        if days == self.days {
            return;
        }
        self.days = days;
        self.count = None;
        cx.notify();
        let api = self.data.read(cx).api.clone();
        let preview = data::background(cx, move || {
            api.retention_preview(HistoryRetentionDays::from(days))
        });
        cx.spawn(async move |ask, cx| {
            let preview = preview.await;
            let _ = ask.update(cx, |ask, cx| {
                // Only the latest pick's count counts.
                if ask.days == days {
                    ask.count = preview.ok().map(|preview| preview.expiring);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save(&mut self, days: u16, window: &mut Window, cx: &mut Context<Self>) {
        let patch = CaptureSettingsUpdate {
            history_retention_days: Some(HistoryRetentionDays::from(days)),
            ..Default::default()
        };
        let task = self
            .data
            .update(cx, |data, cx| data.update_settings(patch, cx));
        cx.spawn(async move |_, _| match task.await {
            Ok(()) => bridge::invalidate(&[&["captures"]]),
            Err(e) => eprintln!("[retention] save failed: {e}"),
        })
        .detach();
        close_dialog(window, cx);
    }
}

impl Render for RetentionAsk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let days = self.days;
        let window_label = retention_label(days);
        let description = match self.count {
            None => t(format!("{KEEP}-ask-counting")),
            Some(count) => kass_i18n::tr!(
                &format!("{KEEP}-ask-description"),
                count = count,
                window = window_label.as_str()
            ),
        };
        let keep = kass_i18n::tr!(
            &format!("{KEEP}-ask-keep"),
            count = self.count.unwrap_or(0),
            window = window_label.as_str()
        );
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(dialog_header(
                t(format!("{KEEP}-ask-title")),
                Some(description.into()),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .text_size(metrics::TEXT_SM)
                            .child(t(format!("{KEEP}-ask-choose"))),
                    )
                    .child(div().w(px(160.)).child(select(&self.choice, cx))),
            )
            .child(
                dialog_footer()
                    .child(
                        Button::new("retention-later")
                            .ghost()
                            .label(t(format!("{KEEP}-ask-later")))
                            .on_click(|_, window, cx| close_dialog(window, cx)),
                    )
                    .child(
                        Button::new("retention-forever")
                            .outline()
                            .label(t(format!("{KEEP}-ask-forever")))
                            .on_click(cx.listener(|ask, _, window, cx| ask.save(0, window, cx))),
                    )
                    .child(
                        Button::new("retention-keep")
                            .destructive()
                            .label(keep)
                            .disabled(self.count.is_none())
                            .on_click(
                                cx.listener(move |ask, _, window, cx| ask.save(days, window, cx)),
                            ),
                    ),
            )
    }
}
