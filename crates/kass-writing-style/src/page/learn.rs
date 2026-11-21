//! Teaching the chosen style, what it learns from (examples, notes, the
//! corrections export, forgetting it all), and its recent corrections.

use super::style::destructive_outline;
use super::{WritingStylePage, toast_error};
use crate::model::{
    Day, app_label, apps_in, day_of, habit_key, parse_time, recent_corrections, short_date,
};
use crate::widgets::{app_tile, icon, mono_label, pill};
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    PersonalExampleSource, PunctuationStyle, WritingStyleHabit, WritingStyleUpdate,
    WritingStylesResponse,
};
use kass_i18n::tr;
use kass_ui::{
    Button, Confirm, SettingRow, SettingSection, dialog_header, metrics, open_confirm, theme,
};
use std::path::PathBuf;
use std::rc::Rc;

/// The learned habits as chips, each explained on hover.
pub(crate) fn habit_chips(habits: &[WritingStyleHabit], cx: &App) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap(px(6.))
        .children(habits.iter().filter_map(|habit| {
            let key = habit_key(*habit)?;
            let explained: SharedString = tr!(&format!("writing-style-habits-{key}")).into();
            Some(
                div()
                    .id(SharedString::from(format!("habit-{key}")))
                    .tooltip(move |window, cx| Tooltip::new(explained.clone()).build(window, cx))
                    .child(pill(tr!(&format!("writing-style-habit-chips-{key}")), cx)),
            )
        }))
}

/// Where the corrections export opens first.
fn downloads() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Downloads"))
        .unwrap_or_else(std::env::temp_dir)
}

impl WritingStylePage {
    pub(super) fn render_calibration(
        &mut self,
        data: &WritingStylesResponse,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let name = self.style_name();
        let apps: Vec<String> = self
            .style_id()
            .map(|id| apps_in(data, &id).into_iter().map(app_label).collect())
            .unwrap_or_default();
        let runs = self.status.as_ref().map_or(0, |status| status.runs);
        let last_run = self
            .status
            .as_ref()
            .and_then(|status| status.last_run_at.as_deref())
            .and_then(parse_time)
            .map(|time| {
                tr!(
                    "writing-style-settings-calibrate-last-run",
                    date = short_date(&time),
                    count = runs
                )
            });
        let habits = self
            .status
            .as_ref()
            .map(|status| status.habits.clone())
            .unwrap_or_default();

        let description = if apps.is_empty() {
            tr!("writing-style-settings-calibrate-description")
        } else {
            tr!(
                "writing-style-settings-calibrate-description-apps",
                apps = apps.join(", ")
            )
        };

        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .mb(px(28.))
            .p(px(20.))
            .rounded(px(10.))
            .border_1()
            .border_color(t.accent.opacity(0.2))
            .bg(t.accent.opacity(0.04))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.foreground)
                                    .child(tr!(
                                        "writing-style-settings-calibrate-title",
                                        style = name.clone()
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(metrics::TEXT_XS)
                                    .line_height(metrics::LINE_XS)
                                    .text_color(t.muted_foreground)
                                    .child(description),
                            ),
                    )
                    .child(
                        Button::new("teach-style")
                            .label(if runs > 0 {
                                tr!("writing-style-settings-calibrate-again")
                            } else {
                                tr!("writing-style-settings-calibrate-start")
                            })
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(
                                cx.listener(|page, _, window, cx| page.open_teach(window, cx)),
                            ),
                    ),
            )
            .when(runs > 0, |el| {
                el.child(
                    div()
                        .flex()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_CAPTION)
                        .text_color(t.muted_foreground)
                        .child(
                            tr!(
                                "writing-style-settings-calibrate-habits-label",
                                style = name.clone()
                            )
                            .to_uppercase(),
                        )
                        .when_some(last_run, |el, last| el.child(format!(" · {last}"))),
                )
                .child(if habits.is_empty() {
                    div()
                        .text_size(metrics::TEXT_XS)
                        .text_color(t.muted_foreground)
                        .child(tr!("writing-style-summary-no-habits"))
                } else {
                    habit_chips(&habits, cx)
                })
            })
    }

    pub(super) fn render_learns_from(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.style_name();
        let examples = self.examples.len();
        let notes = self.notes.len();
        let learned = self
            .status
            .as_ref()
            .is_some_and(|status| status.ready || status.runs > 0);
        let page = cx.entity().downgrade();

        SettingSection::new(tr!(
            "writing-style-settings-learns-from",
            style = name.clone()
        ))
        .child(
            SettingRow::new(tr!("writing-style-settings-examples-title"))
                .description(if examples > 0 {
                    tr!(
                        "writing-style-settings-examples-description",
                        count = examples
                    )
                } else {
                    tr!("writing-style-settings-examples-empty")
                })
                .action(
                    Button::new("view-examples")
                        .outline()
                        .small()
                        .disabled(examples == 0)
                        .label(tr!("writing-style-settings-examples-view"))
                        .on_click(
                            cx.listener(|page, _, window, cx| page.open_examples(window, cx)),
                        ),
                ),
        )
        .child(
            SettingRow::new(tr!("writing-style-settings-notes-title"))
                .description(if notes > 0 {
                    tr!("writing-style-settings-notes-description", count = notes)
                } else {
                    tr!("writing-style-settings-notes-empty")
                })
                .action(
                    Button::new("view-notes")
                        .outline()
                        .small()
                        .disabled(notes == 0)
                        .label(tr!("writing-style-settings-notes-view"))
                        .on_click(cx.listener(|page, _, window, cx| page.open_notes(window, cx))),
                ),
        )
        .child(
            SettingRow::new(tr!("writing-style-settings-export-title"))
                .description(tr!("writing-style-settings-export-description"))
                .action(
                    Button::new("export-corrections")
                        .outline()
                        .small()
                        .label(tr!("writing-style-settings-export-action"))
                        .on_click(cx.listener(|page, _, window, cx| page.export(window, cx))),
                ),
        )
        .when(learned, |section| {
            section.child(
                SettingRow::new(tr!(
                    "writing-style-settings-reset-title",
                    style = name.clone()
                ))
                .description(tr!(
                    "writing-style-settings-reset-description",
                    style = name.clone()
                ))
                .action(
                    destructive_outline(
                        "reset-style",
                        tr!("writing-style-settings-reset-action"),
                        cx,
                    )
                    .on_click(move |_, window, cx| {
                        let page = page.clone();
                        open_confirm(
                            window,
                            cx,
                            Confirm {
                                title: tr!(
                                    "writing-style-settings-reset-confirm-title",
                                    style = name.clone()
                                )
                                .into(),
                                description: Some(
                                    tr!(
                                        "writing-style-settings-reset-description",
                                        style = name.clone()
                                    )
                                    .into(),
                                ),
                                confirm: tr!("writing-style-settings-reset-confirm").into(),
                                cancel: tr!("common-cancel").into(),
                                destructive: true,
                                on_confirm: Rc::new(move |window, cx| {
                                    page.update(cx, |page, cx| page.reset(window, cx)).ok();
                                }),
                            },
                        );
                    }),
                ),
            )
        })
    }

    /// Forgets the style's calibration, habits and rules. With nothing left
    /// to match, a style on Learned goes back to Standard.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(style) = self.style().cloned() else {
            return;
        };
        self.call(
            window,
            cx,
            move |client| {
                client.reset_writing_style(Some(&style.id))?;
                if style.punctuation_style == PunctuationStyle::Learned {
                    client.update_writing_style(
                        &style.id,
                        &WritingStyleUpdate {
                            punctuation_style: Some(PunctuationStyle::Standard),
                            ..Default::default()
                        },
                    )?;
                }
                Ok(())
            },
            |page, result, window, cx| {
                if let Err(error) = result {
                    toast_error(
                        "writing-style-styles-settings-save-failed",
                        error,
                        window,
                        cx,
                    );
                }
                page.loaded_for = None;
                page.reload(cx);
            },
        );
    }

    /// Saves every correction the user made as one JSON file.
    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let picked = cx.prompt_for_new_path(&downloads(), Some("capture-corrections.json"));
        cx.spawn_in(window, async move |page, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let written = cx
                .background_executor()
                .spawn(async move {
                    let reports = client
                        .export_capture_feedback()
                        .map_err(|e| e.to_string())?;
                    let json = serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "reports": reports,
                    }))
                    .map_err(|e| e.to_string())?;
                    std::fs::write(&path, json).map_err(|e| e.to_string())
                })
                .await;
            if let Err(error) = written {
                page.update_in(cx, |_, window, cx| {
                    toast_error("captures-feedback-export-failed", error, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    fn remove_example(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.examples.retain(|example| example.id != id);
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.remove_personal_example(&id),
            |page, result, window, cx| {
                if let Err(error) = result {
                    toast_error(
                        "writing-style-settings-examples-remove-failed",
                        error,
                        window,
                        cx,
                    );
                }
                page.loaded_for = None;
                page.load_style(cx);
            },
        );
    }

    fn remove_note(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let style = self.style_id();
        self.notes.retain(|note| note.id != id);
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.remove_correction_note(&id, style.as_deref()),
            |page, result, window, cx| {
                if let Err(error) = result {
                    toast_error(
                        "writing-style-settings-notes-remove-failed",
                        error,
                        window,
                        cx,
                    );
                }
                page.loaded_for = None;
                page.load_style(cx);
            },
        );
    }

    /// A list dialog that redraws from the page, so removed rows go away.
    fn open_list(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        title: &'static str,
        description: &'static str,
        rows: fn(
            &mut WritingStylePage,
            &WeakEntity<WritingStylePage>,
            &mut Context<WritingStylePage>,
        ) -> Vec<AnyElement>,
    ) {
        let page = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let rows = page
                .update(cx, |this, cx| rows(this, &page, cx))
                .unwrap_or_default();
            dialog.w(px(672.)).close_button(true).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(dialog_header(
                        kass_i18n::t(title),
                        Some(kass_i18n::t(description).into()),
                        cx,
                    ))
                    .child(
                        div()
                            .id("dialog-rows")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .max_h(px(520.))
                            .overflow_y_scroll()
                            .children(rows),
                    ),
            )
        });
    }

    fn open_examples(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_list(
            window,
            cx,
            "writing-style-settings-examples-dialog-title",
            "writing-style-settings-examples-dialog-description",
            |this, page, cx| {
                let t = theme(cx).clone();
                this.examples
                    .iter()
                    .map(|example| {
                        let id = example.id.clone();
                        let page = page.clone();
                        let source = if example.source == PersonalExampleSource::Correction {
                            tr!("writing-style-settings-examples-from-correction")
                        } else {
                            tr!("writing-style-settings-examples-from-calibration")
                        };
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .p(px(14.))
                            .rounded(metrics::RADIUS_LG)
                            .border_1()
                            .border_color(t.border)
                            .bg(t.card)
                            .text_size(metrics::TEXT_BASE)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(mono_label(source, cx).text_size(metrics::TEXT_CAPTION))
                                    .child(div().flex_1())
                                    .child(
                                        Button::new(SharedString::from(format!("remove-{id}")))
                                            .ghost()
                                            .small()
                                            .h(px(28.))
                                            .px_2()
                                            .text_size(metrics::TEXT_XS)
                                            .text_color(t.muted_foreground)
                                            .label(tr!("writing-style-settings-examples-remove"))
                                            .on_click(move |_, window, cx| {
                                                let id = id.clone();
                                                page.update(cx, |page, cx| {
                                                    page.remove_example(id, window, cx)
                                                })
                                                .ok();
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .child(said_or_meant(
                                        tr!("writing-style-settings-examples-said"),
                                        t.muted_foreground,
                                        example.said.clone(),
                                        t.muted_foreground,
                                        cx,
                                    ))
                                    .child(said_or_meant(
                                        tr!("writing-style-settings-examples-meant"),
                                        t.accent,
                                        example.meant.clone(),
                                        t.foreground,
                                        cx,
                                    )),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
        );
    }

    fn open_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_list(
            window,
            cx,
            "writing-style-settings-notes-dialog-title",
            "writing-style-settings-notes-dialog-description",
            |this, page, cx| {
                let t = theme(cx).clone();
                this.notes
                    .iter()
                    .map(|note| {
                        let id = note.id.clone();
                        let page = page.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .p(px(14.))
                            .rounded(metrics::RADIUS_LG)
                            .border_1()
                            .border_color(t.border)
                            .bg(t.card)
                            .text_size(metrics::TEXT_BASE)
                            .text_color(t.foreground)
                            .child(div().flex_1().child(note.text.clone()))
                            .child(
                                Button::new(SharedString::from(format!("remove-{id}")))
                                    .ghost()
                                    .small()
                                    .h(px(28.))
                                    .px_2()
                                    .text_size(metrics::TEXT_XS)
                                    .text_color(t.muted_foreground)
                                    .label(tr!("writing-style-settings-notes-remove"))
                                    .on_click(move |_, window, cx| {
                                        let id = id.clone();
                                        page.update(cx, |page, cx| {
                                            page.remove_note(id, window, cx)
                                        })
                                        .ok();
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
        );
    }

    pub(super) fn render_recent(
        &mut self,
        _: &WritingStylesResponse,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let now = chrono::Local::now();
        let rows: Vec<AnyElement> = recent_corrections(&self.examples)
            .into_iter()
            .enumerate()
            .map(|(index, example)| {
                let when = example
                    .created_at
                    .as_deref()
                    .and_then(parse_time)
                    .map(|time| match day_of(&time, &now) {
                        Day::Today => tr!("writing-style-settings-recent-today"),
                        Day::Yesterday => tr!("writing-style-settings-recent-yesterday"),
                        Day::On(date) => date,
                    });
                let meta = [example.app_name.clone(), when]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                let app = example
                    .app_name
                    .clone()
                    .or_else(|| example.app_bundle_id.clone())
                    .unwrap_or_default();
                let id = example.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px(px(14.))
                    .py(px(10.))
                    .when(index > 0, |el| {
                        el.border_t_1().border_color(t.border.opacity(0.7))
                    })
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_XS)
                    .child(app_tile(&app, px(20.), px(10.), px(5.), cx))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .line_through()
                            .text_color(t.destructive)
                            .child(example.said.clone()),
                    )
                    .child(div().flex_none().text_color(t.muted_foreground).child("→"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(t.success)
                            .child(example.meant.clone()),
                    )
                    .child(div().flex_1())
                    .child(div().flex_none().text_color(t.muted_foreground).child(meta))
                    .child(
                        div()
                            .id(SharedString::from(format!("recent-remove-{id}")))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(26.))
                            .rounded(px(5.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.secondary))
                            .tooltip(|window, cx| {
                                Tooltip::new(tr!("writing-style-settings-recent-remove"))
                                    .build(window, cx)
                            })
                            .child(icon(IconName::X, px(14.), t.muted_foreground))
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.remove_example(id.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .mb(px(28.))
            .child(
                mono_label(
                    tr!(
                        "writing-style-settings-recent-title",
                        style = self.style_name()
                    ),
                    cx,
                )
                .text_size(metrics::TEXT_CAPTION)
                .mb_2(),
            )
            .child(if rows.is_empty() {
                div()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(tr!("writing-style-settings-recent-empty"))
            } else {
                div()
                    .flex()
                    .flex_col()
                    .rounded(metrics::RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .overflow_hidden()
                    .children(rows)
            })
    }
}

/// One side of an example: "You said" or "You meant", and the words.
fn said_or_meant(label: String, label_color: Hsla, text: String, color: Hsla, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_CAPTION)
                .text_color(label_color)
                .child(label),
        )
        .child(div().text_color(color).child(text))
}
