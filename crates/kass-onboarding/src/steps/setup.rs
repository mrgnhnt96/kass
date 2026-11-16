//! The steps that get Kass ready: the welcome, the downloads, the
//! permissions, the microphone, the keys, and the end.

use crate::downloads::{ModelProgress, ModelState, Summary};
use crate::flow::{Step, size_label};
use crate::host::{LoginStatus, MicPermission};
use crate::motion::{self, Frame};
use crate::page::Onboarding;
use crate::poster::{
    self, CapSize, HeadlineSize, Kind, Poster, actions, button, headline, keycaps, lead, panel,
    progress_bar, success_card,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::select::SelectEvent;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::CaptureSettingsUpdate;
use kass_i18n::{t, tr};
use kass_ui::{SelectOption, SelectOptions, Toggle, metrics, select, select_state};

/// A line of words that wraps between them.
fn words(text: &str) -> impl Iterator<Item = Div> + '_ {
    text.split_whitespace()
        .map(|word| div().flex_none().child(word.to_string()))
}

impl Onboarding {
    pub(crate) fn render_welcome(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        // The filler is struck out after the line lands, the way Kass cleans it.
        let size = px(50.);
        let line = poster::display()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(size * 0.26)
            .max_w(px(760.))
            .text_size(size)
            .line_height(size * 1.08)
            .child(motion::strike(
                t("onboarding-welcome-filler-a"),
                poster.fg,
                px(5.),
                frame,
                self.entered,
                900.,
            ))
            .children(words(&t("onboarding-welcome-kept-a")).collect::<Vec<_>>())
            .child(motion::strike(
                t("onboarding-welcome-filler-b"),
                poster.fg,
                px(5.),
                frame,
                self.entered,
                1250.,
            ))
            .children(words(&t("onboarding-welcome-kept-b")).collect::<Vec<_>>());
        vec![
            headline(t("onboarding-welcome-title"), HeadlineSize::Md).into_any_element(),
            line.into_any_element(),
            lead(t("onboarding-welcome-body")).into_any_element(),
            actions()
                .child(button(
                    "start",
                    t("onboarding-welcome-start"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
                .into_any_element(),
        ]
    }

    pub(crate) fn render_download(
        &mut self,
        poster: Poster,
        summary: &Summary,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        if let Some(failed) = &summary.failed {
            return self.render_failed(poster, failed, summary.failed_is_speech, cx);
        }
        let mut rows = div()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(640.))
            .child(model_row(
                t("onboarding-download-speech"),
                t("onboarding-download-speech-role"),
                &summary.speech,
                poster,
            ));
        if let Some(cleanup) = &summary.cleanup {
            rows = rows.child(model_row(
                t("onboarding-download-cleanup"),
                t("onboarding-download-cleanup-role"),
                cleanup,
                poster,
            ));
        }
        let short = summary.disk_short_mb > 0.;
        // Laid out either way, so the buttons below stay put.
        let disk = panel()
            .when(!short, |p| p.opacity(0.))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(t("onboarding-download-disk-title")),
            )
            .child(div().text_size(px(13.)).opacity(0.85).child(tr!(
                "onboarding-download-disk-body",
                size = size_label(summary.disk_short_mb)
            )));
        let action = if short {
            button(
                "check-disk",
                t("onboarding-download-check-again"),
                Kind::Primary,
                poster,
                cx.listener(|page, _, _, cx| page.check_disk(cx)),
            )
        } else if self.progress.downloads_started || summary.ready {
            let label = if summary.ready {
                t("onboarding-next")
            } else {
                t("onboarding-download-continue")
            };
            button(
                "next",
                label,
                Kind::Primary,
                poster,
                cx.listener(|page, _, window, cx| page.next(window, cx)),
            )
        } else {
            button(
                "start-downloads",
                t("onboarding-download-start"),
                Kind::Primary,
                poster,
                cx.listener(|page, _, _, cx| page.start_downloads(cx)),
            )
        };
        vec![
            headline(t("onboarding-download-title"), HeadlineSize::Lg).into_any_element(),
            lead(t("onboarding-download-body")).into_any_element(),
            rows.into_any_element(),
            disk.into_any_element(),
            actions().child(action).into_any_element(),
        ]
    }

    fn render_failed(
        &mut self,
        poster: Poster,
        failed: &ModelProgress,
        is_speech: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let model = failed
            .model
            .as_ref()
            .map(|m| m.display_name.clone())
            .unwrap_or_default();
        let details = failed.error.clone().map(|error| {
            let open = self.failure_details;
            div()
                .flex()
                .flex_col()
                .gap_2()
                .max_w(px(560.))
                .h(px(84.))
                .text_size(px(13.))
                .child(
                    div()
                        .id("failure-details")
                        .cursor_pointer()
                        .underline()
                        .child(t("onboarding-failed-details"))
                        .on_click(cx.listener(|page, _, _, cx| {
                            page.failure_details = !page.failure_details;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .font_family(metrics::FONT_MONO)
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .max_h(px(54.))
                        .overflow_hidden()
                        .opacity(if open { 0.8 } else { 0. })
                        .child(error),
                )
        });
        let mut row = actions().child(
            button(
                "retry",
                t("onboarding-failed-retry"),
                Kind::Primary,
                poster,
                cx.listener(|page, _, _, cx| page.retry_download(cx)),
            )
            .child(poster::icon(IconName::RotateCcw, px(16.), poster.bg)),
        );
        if is_speech {
            row = row.child(button(
                "smaller",
                t("onboarding-failed-smaller"),
                Kind::Outline,
                poster,
                cx.listener(|page, _, _, cx| page.use_smaller_speech_model(cx)),
            ));
        }
        let mut pieces = vec![
            headline(t("onboarding-failed-title"), HeadlineSize::Lg).into_any_element(),
            lead(tr!(
                "onboarding-failed-body",
                model = model,
                percent = failed.percent.round() as i64
            ))
            .into_any_element(),
            div()
                .max_w(px(560.))
                .child(progress_bar(failed.percent, true, poster))
                .into_any_element(),
        ];
        pieces.extend(details.map(IntoElement::into_any_element));
        pieces.push(row.into_any_element());
        pieces
    }

    pub(crate) fn render_input_monitoring(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut pieces = vec![
            headline(t("onboarding-input-monitoring-title"), HeadlineSize::Lg).into_any_element(),
            lead(t("onboarding-input-monitoring-body")).into_any_element(),
        ];
        if !self.input_monitoring {
            // macOS makes the user quit Kass before it applies: say so first.
            // Onboarding reopens here afterward.
            pieces.push(
                panel()
                    .flex_row()
                    .items_start()
                    .gap_3()
                    .child(div().mt(px(2.)).child(poster::icon(
                        IconName::RotateCcw,
                        px(18.),
                        poster.fg,
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(t("onboarding-input-monitoring-quit-title")),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .opacity(0.8)
                                    .child(t("onboarding-input-monitoring-quit-body")),
                            ),
                    )
                    .into_any_element(),
            );
            pieces.push(
                actions()
                    .child(button(
                        "open-settings",
                        t("onboarding-open-settings"),
                        Kind::Primary,
                        poster,
                        cx.listener(|page, _, _, _| page.host.open_input_monitoring_settings()),
                    ))
                    .child(skip_button(poster, cx))
                    .into_any_element(),
            );
            return pieces;
        }
        pieces.push(
            success_card(
                t("onboarding-input-monitoring-on-title"),
                t("onboarding-input-monitoring-on-detail"),
                frame,
                self.entered,
            )
            .into_any_element(),
        );
        // One room for whichever row shows, so nothing below moves.
        let row = if !self.trying {
            actions()
                .child(button(
                    "try",
                    t("onboarding-try-it-now"),
                    Kind::Success,
                    poster,
                    cx.listener(|page, _, _, cx| {
                        page.trying = true;
                        page.pressed_once = false;
                        cx.notify();
                    }),
                ))
                .child(button(
                    "next",
                    t("onboarding-next"),
                    Kind::Ghost,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
        } else if self.pressed_once {
            div()
                .flex()
                .items_center()
                .gap_5()
                .child(motion::pop(
                    poster::display()
                        .text_size(px(26.))
                        .text_color(rgb(poster::BRIGHT_GREEN))
                        .child(t("onboarding-input-monitoring-it-works")),
                    frame,
                    self.pressed_at,
                    0.,
                ))
                .child(button(
                    "next",
                    t("onboarding-next"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .child(div().child(t("onboarding-hold")))
                .child(keycaps(
                    &self.push_keys(),
                    self.held.is_some(),
                    CapSize::Md,
                    true,
                    poster,
                    frame,
                    self.held_at,
                ))
                .child(
                    div()
                        .text_size(px(13.))
                        .opacity(0.8)
                        .child(t("onboarding-input-monitoring-hold-hint")),
                )
        };
        pieces.push(div().min_h(px(64.)).child(row).into_any_element());
        pieces
    }

    pub(crate) fn render_accessibility(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut pieces = vec![
            headline(t("onboarding-accessibility-title"), HeadlineSize::Lg).into_any_element(),
            lead(t("onboarding-accessibility-body")).into_any_element(),
        ];
        if self.accessibility {
            pieces.push(
                success_card(
                    t("onboarding-accessibility-on-title"),
                    t("onboarding-accessibility-on-detail"),
                    frame,
                    self.entered,
                )
                .into_any_element(),
            );
            pieces.push(actions().child(next_button(poster, cx)).into_any_element());
        } else {
            pieces.push(
                actions()
                    .child(button(
                        "open-settings",
                        t("onboarding-open-settings"),
                        Kind::Primary,
                        poster,
                        cx.listener(|page, _, _, _| page.host.open_accessibility_settings()),
                    ))
                    .child(skip_button(poster, cx))
                    .into_any_element(),
            );
        }
        pieces
    }

    pub(crate) fn render_microphone(
        &mut self,
        poster: Poster,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let title = headline(t("onboarding-microphone-title"), HeadlineSize::Lg).into_any_element();
        let permission = self.mic.permission;
        if matches!(
            permission,
            Some(MicPermission::Denied | MicPermission::Restricted)
        ) {
            return vec![
                title,
                lead(t("onboarding-microphone-denied-body")).into_any_element(),
                panel()
                    .child(
                        div()
                            .font_family(metrics::FONT_MONO)
                            .text_size(px(12.))
                            .opacity(0.85)
                            .child(t("onboarding-microphone-path")),
                    )
                    .into_any_element(),
                actions()
                    .child(button(
                        "check-again",
                        t("onboarding-microphone-check-again"),
                        Kind::Primary,
                        poster,
                        cx.listener(|page, _, _, cx| {
                            let permission = page.host.microphone_permission();
                            page.mic.permission = Some(permission);
                            if permission == MicPermission::Granted {
                                page.devices = page.host.input_devices();
                                page.start_mic();
                            }
                            cx.notify();
                        }),
                    ))
                    .child(skip_button(poster, cx))
                    .into_any_element(),
            ];
        }
        if !(self.mic.previewing && permission == Some(MicPermission::Granted)) {
            return vec![
                title,
                lead(t("onboarding-microphone-ask-body")).into_any_element(),
                actions()
                    .child(button(
                        "allow",
                        t("onboarding-microphone-allow"),
                        Kind::Primary,
                        poster,
                        cx.listener(|page, _, _, cx| {
                            page.start_mic();
                            cx.notify();
                        }),
                    ))
                    .into_any_element(),
            ];
        }
        self.sync_mic_select(window, cx);
        let picker = self
            .mic_select
            .as_ref()
            .map(|s| div().flex_1().child(select(&s.state, cx)));
        vec![
            title,
            lead(t("onboarding-microphone-body")).into_any_element(),
            level_meter(self.mic.db, poster).into_any_element(),
            poster::status_line(self.mic.error.clone().map(Into::into)).into_any_element(),
            div()
                .flex()
                .items_center()
                .gap_3()
                .max_w(px(520.))
                .text_size(px(13.))
                .child(div().opacity(0.8).child(t("onboarding-microphone-pick")))
                .children(picker)
                .into_any_element(),
            actions()
                .child(button(
                    "sounds-good",
                    t("onboarding-microphone-sounds-good"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
                .into_any_element(),
        ]
    }

    /// The microphone picker, made again when the devices or the choice change.
    fn sync_mic_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self
            .settings
            .as_ref()
            .and_then(|s| s.input_device_id.clone())
            .filter(|id| self.devices.iter().any(|(each, _)| each == id));
        let key = (chosen.clone(), self.devices.clone());
        if self.mic_select.as_ref().is_some_and(|s| s.key == key) {
            return;
        }
        let mut options: Vec<SelectOption<String>> = vec![SelectOption::new(
            "default".to_string(),
            t("onboarding-microphone-system-default"),
        )];
        options.extend(
            self.devices
                .iter()
                .map(|(id, name)| SelectOption::new(id.clone(), name.clone())),
        );
        let options: SelectOptions<String> = options.into_iter().collect();
        let selected = chosen.unwrap_or_else(|| "default".to_string());
        let state = cx.new(|cx| select_state(options, Some(&selected), window, cx));
        let subscription = cx.subscribe_in(&state, window, |page, _, event, _, cx| {
            if let SelectEvent::Confirm(Some(device)) = event {
                let device = (device != "default").then(|| device.clone());
                if let Some(settings) = page.settings.as_mut() {
                    settings.input_device_id = device.clone();
                }
                page.update_settings(
                    CaptureSettingsUpdate {
                        input_device_id: Some(device),
                        ..Default::default()
                    },
                    cx,
                );
                page.host.mic_preview_stop();
                page.start_mic();
                cx.notify();
            }
        });
        self.mic_select = Some(crate::page::MicSelect {
            key,
            state,
            _subscription: subscription,
        });
    }

    pub(crate) fn render_keys(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let down = matches!(
            self.held.as_deref(),
            Some("push_to_talk" | "toggle_to_talk")
        );
        let (label, since) = if down {
            (t("onboarding-keys-listening"), self.held_at)
        } else if self.pressed_once {
            (t("onboarding-keys-nice"), self.held_at)
        } else {
            (t("onboarding-keys-try-it"), self.entered)
        };
        let row = div()
            .flex()
            .items_center()
            .gap(px(28.))
            .child(keycaps(
                &self.push_keys(),
                down,
                CapSize::Xl,
                false,
                poster,
                frame,
                self.held_at,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(motion::pop(
                        poster::display().text_size(px(26.)).child(label),
                        frame,
                        since,
                        0.,
                    ))
                    .child(
                        div()
                            .max_w(px(340.))
                            .text_size(px(13.))
                            .opacity(0.8)
                            .child(t("onboarding-keys-hint")),
                    ),
            );
        let mut pieces = vec![
            headline(t("onboarding-keys-title"), HeadlineSize::Lg).into_any_element(),
            row.into_any_element(),
        ];
        if !self.input_monitoring {
            pieces.push(
                panel()
                    .child(
                        div()
                            .text_size(px(13.))
                            .child(t("onboarding-keys-needs-input-monitoring")),
                    )
                    .into_any_element(),
            );
        }
        if self.login != LoginStatus::Unavailable {
            pieces.push(self.login_row(cx).into_any_element());
        }
        let action = if self.pressed_once {
            next_button(poster, cx)
        } else {
            button(
                "skip",
                t("onboarding-skip"),
                Kind::Ghost,
                poster,
                cx.listener(|page, _, window, cx| page.next(window, cx)),
            )
        };
        pieces.push(actions().child(action).into_any_element());
        pieces
    }

    fn login_row(&mut self, cx: &mut Context<Self>) -> Div {
        let enabled = self.login == LoginStatus::Enabled;
        let toggle = div()
            .id("login")
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .h(px(36.))
            .rounded_full()
            .px(px(14.))
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(white())
            .cursor_pointer()
            .bg(rgb(if enabled { 0x1E7A3C } else { crate::flow::INK }))
            .child(if enabled {
                format!("✓ {}", t("onboarding-keys-login-enabled"))
            } else if self.login == LoginStatus::RequiresApproval {
                t("onboarding-keys-login-approve")
            } else {
                t("onboarding-keys-login-enable")
            })
            .on_click(cx.listener(move |page, _, _, cx| {
                page.login = page.host.set_launch_at_login(!enabled);
                cx.notify();
            }));
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .max_w(px(520.))
            .rounded(px(16.))
            .border_1()
            .border_color(black().opacity(0.2))
            .bg(white().opacity(0.35))
            .px(px(14.))
            .py_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(t("onboarding-keys-login-title")),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .opacity(0.75)
                            .child(t("onboarding-keys-login-detail")),
                    ),
            )
            .child(toggle)
    }

    /// A spoken step while the models are still coming: what it waits on.
    pub(crate) fn render_locked(
        &mut self,
        poster: Poster,
        summary: &Summary,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let waiting = match &summary.cleanup {
            Some(cleanup) if summary.speech.state == ModelState::Ready => cleanup,
            _ => &summary.speech,
        };
        let failed = summary.failed.is_some();
        let name = waiting
            .model
            .as_ref()
            .map(|m| m.display_name.clone())
            .unwrap_or_default();
        let body = if failed {
            t("onboarding-locked-failed-body")
        } else {
            tr!("onboarding-locked-body", model = name.clone())
        };
        let progress = panel()
            .max_w(px(560.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_weight(FontWeight::MEDIUM)
                    .child(poster::icon(IconName::Lock, px(16.), poster.fg))
                    .child(t("onboarding-locked-cant-hear")),
            )
            .child(progress_bar(waiting.percent, failed, poster))
            .child(
                div()
                    .font_family(metrics::FONT_MONO)
                    .text_size(px(12.))
                    .opacity(0.8)
                    .child(format!("{name} · {}%", waiting.percent.round() as i64)),
            );
        let mut row = actions();
        if failed {
            row = row.child(button(
                "see-why",
                t("onboarding-locked-see-why"),
                Kind::Outline,
                poster,
                cx.listener(|page, _, window, cx| page.go(Step::Download, window, cx)),
            ));
        }
        vec![
            headline(t("onboarding-locked-title"), HeadlineSize::Lg).into_any_element(),
            lead(body).into_any_element(),
            progress.into_any_element(),
            poster::speak_row(
                "dictate",
                &self.push_keys(),
                false,
                true,
                poster,
                frame,
                self.entered,
                |_, _, _| {},
            )
            .into_any_element(),
            row.into_any_element(),
        ]
    }

    pub(crate) fn render_done(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let chords = self.chords();
        let rows = [
            (chords.push.clone(), t("onboarding-done-push")),
            (chords.toggle.clone(), t("onboarding-done-toggle")),
            (chords.command.clone(), t("onboarding-done-command")),
        ];
        let table =
            div()
                .flex()
                .flex_col()
                .max_w(px(560.))
                .children(rows.into_iter().filter(|(keys, _)| !keys.is_empty()).map(
                    |(keys, text)| {
                        div()
                            .flex()
                            .items_center()
                            .border_t_1()
                            .border_color(poster.fg.opacity(0.25))
                            .py(px(10.))
                            .child(div().w(px(170.)).flex_none().child(keycaps(
                                &keys,
                                false,
                                CapSize::Sm,
                                false,
                                poster,
                                frame,
                                self.entered,
                            )))
                            .child(div().flex_1().text_size(px(14.)).child(text))
                    },
                ))
                .child(
                    div()
                        .pt_2()
                        .text_size(px(12.))
                        .opacity(0.75)
                        .child(t("onboarding-done-change")),
                );
        let share = self.settings.as_ref().is_some_and(|s| s.share_usage);
        let share_card = div()
            .flex()
            .items_start()
            .gap_3()
            .max_w(px(560.))
            .rounded(px(16.))
            .bg(white().opacity(0.12))
            .px_4()
            .py_3()
            .child(
                Toggle::new("share-usage")
                    .checked(share)
                    .on_change(cx.listener(|page, on: &bool, _, cx| {
                        let on = *on;
                        if let Some(settings) = page.settings.as_mut() {
                            settings.share_usage = on;
                        }
                        page.update_settings(
                            CaptureSettingsUpdate {
                                share_usage: Some(on),
                                ..Default::default()
                            },
                            cx,
                        );
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(t("onboarding-done-share-title")),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .opacity(0.75)
                            .child(t("onboarding-done-share-body")),
                    ),
            );
        let size = poster::headline_size(HeadlineSize::Xl);
        vec![
            poster::display()
                .text_size(size)
                .line_height(size * 1.04)
                .child(motion::rise_letters(
                    &t("onboarding-done-title"),
                    frame,
                    self.entered,
                    0.,
                    size,
                ))
                .into_any_element(),
            lead(t("onboarding-done-body")).into_any_element(),
            table.into_any_element(),
            share_card.into_any_element(),
            actions()
                .child(button(
                    "start-using",
                    t("onboarding-done-start"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, _, cx| page.finish(None, cx)),
                ))
                .child(button(
                    "show-me",
                    t("onboarding-done-show-me"),
                    Kind::Outline,
                    poster,
                    cx.listener(|page, _, _, cx| {
                        page.finish(Some("/settings/features".into()), cx)
                    }),
                ))
                .into_any_element(),
        ]
    }
}

fn next_button(poster: Poster, cx: &mut Context<Onboarding>) -> Stateful<Div> {
    button(
        "next",
        t("onboarding-next"),
        Kind::Primary,
        poster,
        cx.listener(|page, _, window, cx| page.next(window, cx)),
    )
}

fn skip_button(poster: Poster, cx: &mut Context<Onboarding>) -> Stateful<Div> {
    button(
        "skip",
        t("onboarding-skip-for-now"),
        Kind::Ghost,
        poster,
        cx.listener(|page, _, window, cx| page.next(window, cx)),
    )
}

fn model_row(label: String, role: String, progress: &ModelProgress, poster: Poster) -> Div {
    let status = match progress.state {
        ModelState::Ready => t("onboarding-download-ready"),
        ModelState::Failed => t("onboarding-download-stopped"),
        ModelState::Downloading => format!("{}%", progress.percent.round() as i64),
        ModelState::Next => t("onboarding-download-next"),
        ModelState::Waiting => t("onboarding-download-waiting"),
    };
    let size = size_label(progress.model.as_ref().and_then(|m| m.size_mb).unwrap_or(0) as f64);
    let detail = if size.is_empty() {
        format!("· {role}")
    } else {
        format!("· {role} · {size}")
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(div().font_weight(FontWeight::MEDIUM).child(label))
                        .child(div().opacity(0.75).child(detail)),
                )
                .child(
                    div()
                        .font_family(metrics::FONT_MONO)
                        .text_size(px(12.))
                        .child(status),
                ),
        )
        .child(progress_bar(
            progress.percent,
            progress.state == ModelState::Failed,
            poster,
        ))
}

/// Level bars for a dBFS reading: quiet rooms sit near -60, speech near -20.
fn level_meter(db: f64, poster: Poster) -> Div {
    let level = ((db + 60.) / 50.).clamp(0., 1.) as f32;
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(64.))
        .children((0..30).map(|i| {
            let i = i as f32;
            let shape = 0.35 + 0.65 * ((i * 0.9).sin() * (i * 0.31).cos()).abs();
            let at = i / 30.;
            div()
                .w(px(7.))
                .h(px((shape * level * 60.).max(6.)))
                .rounded(px(4.))
                .bg(poster.fg)
                .opacity(if at < level { 1. } else { 0.3 })
        }))
}
