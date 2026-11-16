//! Settings › Dictation: what's still missing before dictation works, the
//! global shortcut and its chords, the microphone, sound cues and paste.

use gpui_kit::assets::IconName;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{ActiveDownloadTask, CaptureSettingsUpdate, ModelReadiness};
use kass_i18n::{parse_markup, tr};
use kass_ui::{
    metrics, select, select_state, show_toast, theme, Button, SelectOption, SelectOptions,
    SettingRow, SettingSection, Toast, Toggle,
};

use super::chord::{chord_keys, default_chord, open_chord_picker};
use super::hud_preview::HudPreview;
use super::toast_error;
use crate::shell::data::{self, ShellData};

const D: &str = "settings-captures-dictation";
const READY: &str = "captures-readiness";

/// The microphone choice that follows the system's default.
const DEFAULT_MIC: &str = "default";

fn t(id: impl AsRef<str>) -> String {
    kass_i18n::t(id.as_ref())
}

/// Which chord a picker sets.
#[derive(Clone, Copy)]
enum Chord {
    PushToTalk,
    Toggle,
}

/// A permission the page can send you to System Settings for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Permission {
    InputMonitoring,
    Accessibility,
}

impl Permission {
    fn key(self) -> &'static str {
        match self {
            Self::InputMonitoring => "input-monitoring",
            Self::Accessibility => "accessibility",
        }
    }

    fn check(self) -> bool {
        match self {
            Self::InputMonitoring => crate::check_input_monitoring_permission(),
            Self::Accessibility => crate::check_accessibility_permission(),
        }
    }

    fn open_settings(self) {
        let opened = match self {
            Self::InputMonitoring => crate::open_input_monitoring_settings(),
            Self::Accessibility => crate::open_accessibility_settings(crate::host::AppHandle),
        };
        if let Err(e) = opened {
            eprintln!("[settings] {e}");
        }
    }
}

/// The models still to download, in the toast's words.
pub fn missing_models(stt: Option<&ModelReadiness>, llm: Option<&ModelReadiness>) -> Vec<String> {
    [stt, llm]
        .into_iter()
        .flatten()
        .filter(|m| !m.ready)
        .map(|m| m.display_name.clone())
        .collect()
}

/// "1.5 GB" from a size in MB, as the checklist shows it.
fn gigabytes(mb: u64) -> String {
    format!("{:.1} GB", mb as f64 / 1000.)
}

/// How far along a download is, 0 to 100, when the server says.
pub fn percent(task: &ActiveDownloadTask) -> Option<u32> {
    if let Some(progress) = task.progress {
        return Some(progress.clamp(0., 100.).round() as u32);
    }
    match (task.current, task.total) {
        (Some(current), Some(total)) if total > 0 => {
            Some((current as f64 / total as f64 * 100.).round() as u32)
        }
        _ => None,
    }
}

pub struct DictationPage {
    data: Entity<ShellData>,
    mic: Entity<SelectState<SelectOptions<String>>>,
    /// The microphones in the dropdown, to tell when the list changes.
    mics: Vec<(String, String)>,
    volume: Entity<SliderState>,
    preview: Entity<HudPreview>,
    /// The volume last saved, to keep the slider on it.
    saved_volume: f64,
    /// A download you asked for is on its way to the server.
    starting_download: bool,
    /// A permission rechecked from its notice, and whether it's still missing.
    checking: Option<Permission>,
    still_missing: Vec<Permission>,
    _subscriptions: Vec<Subscription>,
}

impl DictationPage {
    pub fn new(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mics = Self::mic_list(&data, cx);
        let chosen = Self::chosen_mic(&data, &mics, cx);
        let options = Self::mic_options(&mics);
        let mic = cx.new(|cx| select_state(options, Some(&chosen), window, cx));

        let saved_volume = Self::saved_volume(&data, cx);
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.05)
                .default_value(saved_volume as f32)
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &mic,
                window,
                |this, _, event: &SelectEvent<_>, window, cx| {
                    if let SelectEvent::Confirm(Some(id)) = event {
                        let id: &String = id;
                        let input_device_id = (id != DEFAULT_MIC).then(|| id.clone());
                        this.save(
                            CaptureSettingsUpdate {
                                input_device_id: Some(input_device_id),
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    }
                },
            ),
            cx.subscribe_in(
                &volume,
                window,
                |this, _, event: &SliderEvent, window, cx| {
                    if let SliderEvent::Release(value) = event {
                        this.set_volume(value.start(), window, cx);
                    }
                },
            ),
            cx.observe_in(&data, window, |this, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
        ];

        Self {
            data,
            mic,
            mics,
            volume,
            preview: cx.new(HudPreview::new),
            saved_volume,
            starting_download: false,
            checking: None,
            still_missing: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn mic_list(data: &Entity<ShellData>, cx: &App) -> Vec<(String, String)> {
        data.read(cx)
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect()
    }

    fn mic_options(mics: &[(String, String)]) -> SelectOptions<String> {
        std::iter::once(SelectOption::new(
            DEFAULT_MIC.to_string(),
            t(format!("{D}-input-device-default")),
        ))
        .chain(
            mics.iter()
                .map(|(id, name)| SelectOption::new(id.clone(), name.clone())),
        )
        .collect()
    }

    /// The saved microphone, or the default when it's gone.
    fn chosen_mic(data: &Entity<ShellData>, mics: &[(String, String)], cx: &App) -> String {
        data.read(cx)
            .settings
            .as_ref()
            .and_then(|s| s.input_device_id.clone())
            .filter(|id| mics.iter().any(|(m, _)| m == id))
            .unwrap_or_else(|| DEFAULT_MIC.to_string())
    }

    fn saved_volume(data: &Entity<ShellData>, cx: &App) -> f64 {
        data.read(cx)
            .settings
            .as_ref()
            .map_or(0.5, |s| s.sound_cue_volume)
    }

    /// Keep the dropdown and slider on what's saved, whoever changed it.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mics = Self::mic_list(&self.data, cx);
        if mics != self.mics {
            let options = Self::mic_options(&mics);
            self.mic
                .update(cx, |state, cx| state.set_items(options, window, cx));
            self.mics = mics;
        }
        let chosen = Self::chosen_mic(&self.data, &self.mics, cx);
        self.mic.update(cx, |state, cx| {
            if state.selected_value() != Some(&chosen) {
                state.set_selected_value(&chosen, window, cx);
            }
        });
        let saved = Self::saved_volume(&self.data, cx);
        if saved != self.saved_volume {
            self.saved_volume = saved;
            self.volume
                .update(cx, |state, cx| state.set_value(saved as f32, window, cx));
        }
    }

    fn save(&mut self, patch: CaptureSettingsUpdate, window: &mut Window, cx: &mut Context<Self>) {
        let task = self
            .data
            .update(cx, |data, cx| data.update_settings(patch, cx));
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

    /// Save the volume let go on, and play the start chime at it.
    fn set_volume(&mut self, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = ((value as f64) * 20.).round() / 20.;
        if volume == self.saved_volume {
            return;
        }
        self.saved_volume = volume;
        self.save(
            CaptureSettingsUpdate {
                sound_cue_volume: Some(volume),
                ..Default::default()
            },
            window,
            cx,
        );
        crate::sound_cues::preview_sound_cue(volume as f32);
    }

    /// Turn the shortcut on or off. On while a model is missing, say the
    /// shortcut won't work until it's downloaded.
    fn set_hotkey(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.save(
            CaptureSettingsUpdate {
                hotkey_enabled: Some(on),
                ..Default::default()
            },
            window,
            cx,
        );
        if !on {
            return;
        }
        let data = self.data.read(cx);
        let auto_refine = data.settings.as_ref().is_none_or(|s| s.auto_refine);
        let Some(readiness) = data.readiness.as_ref() else {
            return;
        };
        let llm = auto_refine.then_some(&readiness.llm);
        let names = missing_models(Some(&readiness.stt), llm);
        if names.is_empty() {
            return;
        }
        let description = tr!(
            "captures-toast-shortcut-not-armed-description",
            names = names.join(" and "),
            count = names.len()
        );
        show_toast(
            window,
            cx,
            Toast::new(t("captures-toast-shortcut-not-armed")).description(description),
        );
    }

    fn change_chord(&mut self, chord: Chord, window: &mut Window, cx: &mut Context<Self>) {
        let (key, initial) = match chord {
            Chord::PushToTalk => ("ptt", self.chord(Chord::PushToTalk, cx)),
            Chord::Toggle => ("toggle", self.chord(Chord::Toggle, cx)),
        };
        let this = cx.entity().downgrade();
        open_chord_picker(
            t(format!("{D}-chord-picker-{key}-title")),
            t(format!("{D}-chord-picker-{key}-description")),
            initial,
            move |keys, window, cx| {
                let patch = match chord {
                    Chord::PushToTalk => CaptureSettingsUpdate {
                        chord_push_to_talk_keys: Some(keys),
                        ..Default::default()
                    },
                    Chord::Toggle => CaptureSettingsUpdate {
                        chord_toggle_to_talk_keys: Some(keys),
                        ..Default::default()
                    },
                };
                let _ = this.update(cx, |this, cx| this.save(patch, window, cx));
            },
            window,
            cx,
        );
    }

    fn chord(&self, chord: Chord, cx: &App) -> Vec<String> {
        let settings = self.data.read(cx).settings.as_ref();
        match chord {
            Chord::PushToTalk => settings
                .map(|s| s.chord_push_to_talk_keys.clone())
                .unwrap_or_else(|| default_chord(false)),
            Chord::Toggle => settings
                .map(|s| s.chord_toggle_to_talk_keys.clone())
                .unwrap_or_else(|| default_chord(true)),
        }
    }

    fn download(&mut self, model: ModelReadiness, window: &mut Window, cx: &mut Context<Self>) {
        self.starting_download = true;
        cx.notify();
        let api = self.data.read(cx).api.clone();
        let name = model.model_name.clone();
        let task = data::background(cx, move || api.trigger_model_download(&name));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting_download = false;
                match result {
                    Ok(_) => show_toast(
                        window,
                        cx,
                        Toast::new(t(format!("{READY}-download-started"))).description(tr!(
                            &format!("{READY}-download-started-description"),
                            name = model.display_name.as_str()
                        )),
                    ),
                    Err(e) => toast_error(
                        t(format!("{READY}-download-failed")),
                        &e.to_string(),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn recheck(&mut self, permission: Permission, window: &mut Window, cx: &mut Context<Self>) {
        self.checking = Some(permission);
        self.still_missing.retain(|p| *p != permission);
        cx.notify();
        let task = data::background(cx, move || permission.check());
        cx.spawn_in(window, async move |this, cx| {
            let granted = task.await;
            let _ = this.update(cx, |this, cx| {
                this.checking = None;
                if !granted {
                    this.still_missing.push(permission);
                }
                this.data.update(cx, |data, cx| {
                    match permission {
                        Permission::InputMonitoring => data.input_monitoring = granted,
                        Permission::Accessibility => data.accessibility = granted,
                    }
                    cx.notify();
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn readiness(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let data = self.data.read(cx);
        let auto_refine = data.settings.as_ref().is_none_or(|s| s.auto_refine);
        let readiness = data.readiness.clone();
        let stt_ready = readiness.as_ref().is_some_and(|r| r.stt.ready);
        let llm_ready = readiness.as_ref().is_some_and(|r| r.llm.ready);
        let (input_monitoring, accessibility) = (data.input_monitoring, data.accessibility);
        if stt_ready && (!auto_refine || llm_ready) && input_monitoring && accessibility {
            return None;
        }
        let downloads = data.downloads.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(readiness) = readiness {
            rows.push(self.model_row("stt", readiness.stt, &downloads, cx));
            if auto_refine {
                rows.push(self.model_row("llm", readiness.llm, &downloads, cx));
            }
        }
        rows.push(self.permission_row(
            Permission::InputMonitoring,
            input_monitoring,
            IconName::Keyboard,
            cx,
        ));
        rows.push(self.permission_row(
            Permission::Accessibility,
            accessibility,
            IconName::Accessibility,
            cx,
        ));
        let t_ = theme(cx);
        Some(
            div()
                .mb(px(28.))
                .p_4()
                .rounded(metrics::RADIUS_LG)
                .border_1()
                .border_color(t_.warning.opacity(0.3))
                .bg(t_.warning.opacity(0.06))
                .child(
                    div()
                        .mb_3()
                        .text_size(metrics::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .child(t(format!("{READY}-title"))),
                )
                .child(div().flex().flex_col().gap(px(10.)).children(rows))
                .into_any_element(),
        )
    }

    fn model_row(
        &self,
        gate: &'static str,
        model: ModelReadiness,
        downloads: &[ActiveDownloadTask],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let description = if model.ready {
            t(format!("{READY}-{gate}-ready"))
        } else if let Some(mb) = model.size_mb {
            tr!(
                &format!("{READY}-{gate}-missing-with-size"),
                size = gigabytes(mb)
            )
        } else {
            t(format!("{READY}-{gate}-missing"))
        };
        let task = downloads
            .iter()
            .find(|d| d.model_name == model.model_name && d.status == "downloading");
        let downloading = !model.ready && task.is_some();
        let label = match task.and_then(percent) {
            Some(pct) if downloading => tr!(&format!("{READY}-downloading-percent"), pct = pct),
            _ if downloading => t(format!("{READY}-downloading")),
            _ => t(format!("{READY}-download-button")),
        };
        let this = cx.entity().downgrade();
        let ready = model.ready;
        let title = tr!(
            &format!("{READY}-{gate}-label"),
            name = model.display_name.as_str()
        );
        let action = Button::new(SharedString::from(format!("download-{gate}")))
            .small()
            .icon(if downloading {
                IconName::LoaderCircle
            } else {
                IconName::Download
            })
            .label(label)
            .disabled(downloading || self.starting_download)
            .on_click(move |_, window, cx| {
                let model = model.clone();
                let _ = this.update(cx, |this, cx| this.download(model, window, cx));
            });
        checklist_row(
            IconName::Cpu,
            title,
            description,
            ready,
            action.into_any_element(),
            cx,
        )
    }

    fn permission_row(
        &self,
        permission: Permission,
        granted: bool,
        icon: IconName,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = permission.key();
        let description = if granted {
            t(format!("{READY}-{key}-ready"))
        } else {
            t(format!("{READY}-{key}-missing"))
        };
        let action = Button::new(SharedString::from(format!("open-{key}")))
            .small()
            .icon(IconName::ExternalLink)
            .label(t(format!("{READY}-{key}-open-settings")))
            .on_click(move |_, _, _| permission.open_settings());
        checklist_row(
            icon,
            t(format!("{READY}-{key}-label")),
            description,
            granted,
            action.into_any_element(),
            cx,
        )
    }

    /// The warning under a setting that needs a permission macOS hasn't given.
    fn notice(&self, permission: Permission, cx: &mut Context<Self>) -> AnyElement {
        let t_ = theme(cx);
        let key = permission.key();
        let p = format!("captures-permissions-{key}");
        let checking = self.checking == Some(permission);
        let still_missing = !checking && self.still_missing.contains(&permission);
        let this = cx.entity().downgrade();
        div()
            .mt_3()
            .flex()
            .items_start()
            .gap_3()
            .px(px(14.))
            .py_3()
            .rounded(metrics::RADIUS_LG)
            .border_1()
            .border_color(t_.warning.opacity(0.3))
            .bg(t_.warning.opacity(0.06))
            .child(icon(IconName::TriangleAlert, 16., t_.warning).mt(px(2.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(metrics::TEXT_BASE)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t_.foreground)
                            .child(t(format!("{p}-title"))),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_XS)
                            .line_height(px(19.5))
                            .text_color(t_.muted_foreground)
                            .child(marked_path(
                                &t(format!("{p}-body")),
                                t_.foreground.opacity(0.85),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pt_2()
                            .child(
                                Button::new(SharedString::from(format!("{key}-open")))
                                    .small()
                                    .icon(IconName::ExternalLink)
                                    .label(t(format!("{p}-open-settings")))
                                    .on_click(move |_, _, _| permission.open_settings()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("{key}-recheck")))
                                    .small()
                                    .outline()
                                    .disabled(checking)
                                    .label(if checking {
                                        t(format!("{p}-rechecking"))
                                    } else {
                                        t(format!("{p}-recheck"))
                                    })
                                    .on_click(move |_, window, cx| {
                                        let _ = this.update(cx, |this, cx| {
                                            this.recheck(permission, window, cx)
                                        });
                                    }),
                            ),
                    )
                    .when(still_missing, |body| {
                        body.child(
                            div()
                                .pt_1()
                                .text_size(metrics::TEXT_XS)
                                .text_color(t_.warning)
                                .child(t(format!("{p}-still-missing"))),
                        )
                    }),
            )
            .into_any_element()
    }

    fn chord_action(
        &self,
        id: &'static str,
        chord: Chord,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let keys = self.chord(chord, cx);
        let this = cx.entity().downgrade();
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(chord_keys(&keys, false, cx))
            .child(
                Button::new(id)
                    .outline()
                    .small()
                    .ml(px(6.))
                    .disabled(disabled)
                    .label(t(format!("{D}-push-to-talk-change")))
                    .on_click(move |_, window, cx| {
                        let _ = this.update(cx, |this, cx| this.change_chord(chord, window, cx));
                    }),
            )
    }
}

/// A setting's title, description and control without the row's padding,
/// for rows with a notice under them.
fn row_layout(title: String, description: String, control: impl IntoElement, cx: &App) -> Div {
    let t_ = theme(cx);
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(32.))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(metrics::TEXT_SM)
                        .line_height(metrics::TEXT_SM)
                        .text_color(t_.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(metrics::TEXT_XS)
                        .line_height(px(19.5))
                        .text_color(t_.muted_foreground)
                        .child(description),
                ),
        )
        .child(div().flex_none().child(control))
}

fn icon(name: IconName, size: f32, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .flex_shrink_0()
        .size(px(size))
        .text_color(color)
}

/// Text with its `<path>…</path>` run drawn in `color`.
fn marked_path(text: &str, color: Hsla) -> StyledText {
    let mut plain = String::new();
    let mut highlights = Vec::new();
    for run in parse_markup(text, &["path"]) {
        let start = plain.len();
        plain.push_str(&run.text);
        if run.tag.is_some() {
            highlights.push((
                start..plain.len(),
                HighlightStyle {
                    color: Some(color),
                    font_weight: Some(FontWeight::MEDIUM),
                    ..Default::default()
                },
            ));
        }
    }
    StyledText::new(plain).with_highlights(highlights)
}

/// One line of the "before you can dictate" list.
fn checklist_row(
    kind: IconName,
    title: String,
    description: String,
    ready: bool,
    action: AnyElement,
    cx: &App,
) -> AnyElement {
    let t_ = theme(cx);
    div()
        .flex()
        .items_start()
        .gap_3()
        .p(px(14.))
        .rounded(metrics::RADIUS_LG)
        .border_1()
        .when(ready, |row| {
            row.border_color(t_.accent.opacity(0.2))
                .bg(t_.accent.opacity(0.05))
        })
        .when(!ready, |row| {
            row.border_color(t_.border).bg(t_.muted.opacity(0.2))
        })
        .child(div().mt(px(2.)).child(if ready {
            icon(IconName::CircleCheck, 20., t_.accent)
        } else {
            icon(IconName::Circle, 20., t_.muted_foreground.opacity(0.5))
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(kind, 14., t_.muted_foreground))
                        .child(
                            div()
                                .text_size(metrics::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t_.foreground)
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .text_size(metrics::TEXT_XS)
                        .line_height(px(19.5))
                        .text_color(t_.muted_foreground)
                        .child(description),
                )
                .when(!ready, |body| body.child(div().pt(px(6.)).child(action))),
        )
        .into_any_element()
}

impl Render for DictationPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (hotkey, auto_paste, voice_edits, sound_cues, input_monitoring, accessibility) = {
            let data = self.data.read(cx);
            let s = data.settings.as_ref();
            (
                s.is_some_and(|s| s.hotkey_enabled),
                s.is_none_or(|s| s.allow_auto_paste),
                s.is_none_or(|s| s.voice_edits),
                s.is_none_or(|s| s.sound_cues),
                data.input_monitoring,
                data.accessibility,
            )
        };
        self.preview
            .update(cx, |preview, _| preview.set_enabled(hotkey));
        let this = cx.entity().downgrade();
        let hotkey_toggle =
            Toggle::new("hotkey-enabled")
                .checked(hotkey)
                .on_change(move |on, window, cx| {
                    let on = *on;
                    let _ = this.update(cx, |this, cx| this.set_hotkey(on, window, cx));
                });

        let shortcut = SettingSection::new(t(format!("{D}-section-shortcut")))
            .child(
                div()
                    .py(px(14.))
                    .child(row_layout(
                        t(format!("{D}-global-shortcut-title")),
                        t(format!("{D}-global-shortcut-description")),
                        hotkey_toggle,
                        cx,
                    ))
                    .when(hotkey && !input_monitoring, |row| {
                        row.child(self.notice(Permission::InputMonitoring, cx))
                    }),
            )
            .child(
                SettingRow::new(t(format!("{D}-push-to-talk-title")))
                    .description(t(format!("{D}-push-to-talk-description")))
                    .action(self.chord_action("change-ptt", Chord::PushToTalk, !hotkey, cx)),
            )
            .child(
                SettingRow::new(t(format!("{D}-toggle-title")))
                    .description(t(format!("{D}-toggle-description")))
                    .action(self.chord_action("change-toggle", Chord::Toggle, !hotkey, cx)),
            );

        let input = SettingSection::new(t(format!("{D}-section-input")))
            .child(
                SettingRow::new(t(format!("{D}-input-device-title")))
                    .description(t(format!("{D}-input-device-description")))
                    .action(div().w(px(240.)).child(select(&self.mic, cx))),
            )
            .child(
                SettingRow::new(t(format!("{D}-preview-title")))
                    .description(t(format!("{D}-preview-description")))
                    .action(self.preview.clone()),
            )
            .child(
                SettingRow::new(t(format!("{D}-sound-cues-title")))
                    .description(t(format!("{D}-sound-cues-description")))
                    .action(self.toggle(
                        "sound-cues",
                        sound_cues,
                        false,
                        |v| CaptureSettingsUpdate {
                            sound_cues: Some(v),
                            ..Default::default()
                        },
                        cx,
                    )),
            )
            .child(
                SettingRow::new(t(format!("{D}-sound-cue-volume-title")))
                    .description(t(format!("{D}-sound-cue-volume-description")))
                    .action(
                        div()
                            .w(px(240.))
                            .child(Slider::new(&self.volume).disabled(!sound_cues)),
                    ),
            );

        let output = SettingSection::new(t(format!("{D}-section-output")))
            .child(
                div()
                    .py(px(14.))
                    .child(row_layout(
                        t(format!("{D}-auto-paste-title")),
                        t(format!("{D}-auto-paste-description")),
                        self.toggle(
                            "auto-paste",
                            auto_paste,
                            !hotkey,
                            |v| CaptureSettingsUpdate {
                                allow_auto_paste: Some(v),
                                ..Default::default()
                            },
                            cx,
                        ),
                        cx,
                    ))
                    .when(!accessibility, |row| {
                        row.child(self.notice(Permission::Accessibility, cx))
                    }),
            )
            .child(
                SettingRow::new(t(format!("{D}-voice-edits-title")))
                    .description(t(format!("{D}-voice-edits-description")))
                    .action(self.toggle(
                        "voice-edits",
                        voice_edits,
                        !hotkey,
                        |v| CaptureSettingsUpdate {
                            voice_edits: Some(v),
                            ..Default::default()
                        },
                        cx,
                    )),
            );

        div()
            .flex()
            .flex_col()
            .children(self.readiness(cx))
            .child(shortcut)
            .child(input)
            .child(output)
    }
}

#[cfg(test)]
mod tests {
    use super::{gigabytes, missing_models, percent};
    use kass_api::types::{ActiveDownloadTask, ModelReadiness};

    fn model(name: &str, ready: bool) -> ModelReadiness {
        ModelReadiness {
            ready,
            model_name: name.to_lowercase(),
            display_name: name.into(),
            size: String::new(),
            size_mb: None,
        }
    }

    #[test]
    fn names_the_models_still_missing() {
        let stt = model("Whisper", false);
        let llm = model("Qwen", true);
        assert_eq!(missing_models(Some(&stt), Some(&llm)), vec!["Whisper"]);
        assert_eq!(missing_models(Some(&stt), None), vec!["Whisper"]);
        let llm = model("Qwen", false);
        assert_eq!(
            missing_models(Some(&stt), Some(&llm)),
            vec!["Whisper", "Qwen"]
        );
    }

    #[test]
    fn sizes_in_gigabytes() {
        assert_eq!(gigabytes(1500), "1.5 GB");
        assert_eq!(gigabytes(800), "0.8 GB");
    }

    #[test]
    fn download_percent() {
        let mut task = ActiveDownloadTask {
            model_name: "m".into(),
            status: "downloading".into(),
            started_at: String::new(),
            error: None,
            progress: Some(42.4),
            current: None,
            total: None,
            filename: None,
        };
        assert_eq!(percent(&task), Some(42));
        task.progress = None;
        task.current = Some(50);
        task.total = Some(200);
        assert_eq!(percent(&task), Some(25));
        task.total = None;
        assert_eq!(percent(&task), None);
    }
}
