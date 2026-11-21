//! The chosen style's settings: its name, how the user writes in its apps,
//! punctuation, technical terms, whether new apps use it, and deleting it.

use super::{TakeOwner, WritingStylePage, toast_error};
use crate::dictation::TakeState;
use crate::model::MAX_DESCRIPTION;
use crate::widgets::icon;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Escape, InputEvent, Textarea};
use gpui_kit::component::select::SelectState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{PunctuationStyle, WritingStyle, WritingStyleUpdate, WritingStylesResponse};
use kass_dictionary::chord_keys;
use kass_i18n::tr;
use kass_ui::{
    Button, Confirm, SelectOption, SelectOptions, SettingRow, SettingSection, Toggle, metrics,
    open_confirm, select, select_state, text_input, theme,
};
use std::rc::Rc;

const PUNCTUATION: &str = "settings-captures-refinement-punctuation-style";

/// What the name, description and punctuation fields were last filled from.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FormFor {
    id: String,
    name: String,
    description: String,
    punctuation: PunctuationStyle,
    ready: bool,
}

fn punctuation_options(ready: bool) -> SelectOptions<PunctuationStyle> {
    vec![
        SelectOption::new(
            PunctuationStyle::Casual,
            tr!(&format!("{PUNCTUATION}-casual")),
        ),
        SelectOption::new(
            PunctuationStyle::Standard,
            tr!(&format!("{PUNCTUATION}-standard")),
        ),
        SelectOption::new(
            PunctuationStyle::Learned,
            if ready {
                tr!(&format!("{PUNCTUATION}-learned"))
            } else {
                tr!(&format!("{PUNCTUATION}-learned-locked"))
            },
        ),
    ]
}

/// The punctuation dropdown; Learned says it's locked until the style has learned enough.
pub(super) fn punctuation_state(
    selected: Option<PunctuationStyle>,
    ready: bool,
    window: &mut Window,
    cx: &mut Context<SelectState<SelectOptions<PunctuationStyle>>>,
) -> SelectState<SelectOptions<PunctuationStyle>> {
    select_state(punctuation_options(ready), selected.as_ref(), window, cx)
}

impl WritingStylePage {
    fn ready(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.ready)
    }

    /// Fills the fields from the chosen style when it or its settings change,
    /// leaving a field alone while it's being typed in.
    pub(super) fn sync_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(style) = self.style().cloned() else {
            return;
        };
        let next = FormFor {
            id: style.id.clone(),
            name: style.name.clone(),
            description: style.description.clone(),
            punctuation: style.punctuation_style,
            ready: self.ready(),
        };
        if self.form_for.as_ref() == Some(&next) {
            return;
        }
        let switched = self.form_for.as_ref().is_none_or(|form| form.id != next.id);
        let before = self.form_for.replace(next.clone());

        let name_focused = self.name.read(cx).focus_handle(cx).is_focused(window);
        if switched || (!name_focused && before.as_ref().is_some_and(|b| b.name != next.name)) {
            self.name.update(cx, |input, cx| {
                input.set_value(next.name.clone(), window, cx)
            });
        }
        let description_focused = self
            .description
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if switched
            || (!description_focused
                && before
                    .as_ref()
                    .is_some_and(|b| b.description != next.description))
        {
            self.description.update(cx, |input, cx| {
                input.set_value(next.description.clone(), window, cx)
            });
        }
        self.punctuation.update(cx, |select, cx| {
            select.set_items(punctuation_options(next.ready), window, cx);
            select.set_selected_value(&next.punctuation, window, cx);
        });
    }

    /// Saves one change to the chosen style, then fetches the styles again.
    fn update_style(
        &mut self,
        patch: WritingStyleUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.style_id() else {
            return;
        };
        self.call(
            window,
            cx,
            move |client| client.update_writing_style(&id, &patch).map(|_| ()),
            |page, result, window, cx| {
                if let Err(error) = result {
                    toast_error(
                        "writing-style-styles-settings-save-failed",
                        error,
                        window,
                        cx,
                    );
                    // Show the saved settings again, not the refused ones.
                    page.form_for = None;
                }
                page.reload(cx);
            },
        );
    }

    pub(super) fn name_input(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { .. } => window.blur(cx),
            InputEvent::Blur => self.rename(window, cx),
            _ => {}
        }
    }

    fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(style) = self.style().cloned() else {
            return;
        };
        let next = self.name.read(cx).value().trim().to_string();
        if !next.is_empty() && next != style.name {
            self.update_style(
                WritingStyleUpdate {
                    name: Some(next),
                    ..Default::default()
                },
                window,
                cx,
            );
        } else {
            self.reset_name(window, cx);
        }
    }

    fn reset_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.style_name();
        self.name
            .update(cx, |input, cx| input.set_value(name, window, cx));
    }

    pub(super) fn description_input(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                crate::widgets::limit_chars(&self.description, MAX_DESCRIPTION, window, cx);
                cx.notify();
            }
            InputEvent::Blur => self.save_description(window, cx),
            _ => {}
        }
    }

    fn save_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(style) = self.style().cloned() else {
            return;
        };
        let next = self.description.read(cx).value().trim().to_string();
        if next != style.description {
            self.update_style(
                WritingStyleUpdate {
                    description: Some(next),
                    ..Default::default()
                },
                window,
                cx,
            );
        }
    }

    /// Adds what a Say it take said after what's in the box, and saves it.
    pub(super) fn append_description(
        &mut self,
        said: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.description.read(cx).value().to_string();
        let joined = if current.trim().is_empty() {
            said
        } else {
            format!("{} {said}", current.trim_end())
        };
        let next: String = joined.chars().take(MAX_DESCRIPTION).collect();
        self.description
            .update(cx, |input, cx| input.set_value(next, window, cx));
        self.save_description(window, cx);
    }

    pub(super) fn choose_punctuation(
        &mut self,
        choice: PunctuationStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(style) = self.style().cloned() else {
            return;
        };
        if choice == PunctuationStyle::Learned && !self.ready() {
            // Locked until the style has learned enough: put the choice back.
            self.form_for = None;
            cx.notify();
            return;
        }
        if choice != style.punctuation_style {
            self.update_style(
                WritingStyleUpdate {
                    punctuation_style: Some(choice),
                    ..Default::default()
                },
                window,
                cx,
            );
        }
    }

    fn delete_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.style_id() else {
            return;
        };
        self.call(
            window,
            cx,
            move |client| client.delete_writing_style(&id),
            |page, result, window, cx| {
                match result {
                    Ok(()) => {
                        page.wanted = None;
                        page.form_for = None;
                        page.filter = crate::model::AppFilter::All;
                    }
                    Err(error) => toast_error(
                        "writing-style-styles-settings-delete-failed",
                        error,
                        window,
                        cx,
                    ),
                }
                page.reload(cx);
            },
        );
    }

    pub(super) fn render_style_settings(
        &mut self,
        _: &WritingStylesResponse,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let style = self.style().cloned().unwrap_or_else(|| WritingStyle {
            id: String::new(),
            name: String::new(),
            position: 0,
            is_default: false,
            punctuation_style: PunctuationStyle::Standard,
            preserve_technical: false,
            description: String::new(),
        });
        let page = cx.entity().downgrade();
        let length = self.description.read(cx).value().chars().count();

        let name = div()
            .w(px(240.))
            .on_action(cx.listener(|page, _: &Escape, window, cx| page.reset_name(window, cx)))
            .child(text_input(&self.name, cx).h(px(32.)));

        let description = SettingRow::new(tr!("writing-style-styles-settings-description"))
            .description(tr!("writing-style-styles-settings-description-help"))
            .when(self.dictation.is_some(), |row| {
                row.action(self.render_say_it(cx))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Textarea::new(&self.description)
                            .w_full()
                            .px_3()
                            .py(px(10.))
                            .rounded(metrics::RADIUS_LG)
                            .bg(t.background)
                            .text_size(metrics::TEXT_BASE),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_CAPTION)
                            .text_color(t.muted_foreground)
                            .child(format!("{length} / {MAX_DESCRIPTION}")),
                    ),
            );

        let technical = {
            let page = page.clone();
            Toggle::new("preserve-technical")
                .checked(style.preserve_technical)
                .on_change(move |on, window, cx| {
                    let on = *on;
                    page.update(cx, |page, cx| {
                        page.update_style(
                            WritingStyleUpdate {
                                preserve_technical: Some(on),
                                ..Default::default()
                            },
                            window,
                            cx,
                        )
                    })
                    .ok();
                })
        };

        let default_description = if style.is_default {
            tr!("writing-style-styles-settings-default-on")
        } else {
            tr!(
                "writing-style-styles-settings-default-description",
                style = style.name.clone()
            )
        };

        SettingSection::new(tr!(
            "writing-style-styles-settings-title",
            style = style.name.clone()
        ))
        .child(SettingRow::new(tr!("writing-style-styles-settings-name")).action(name))
        .child(description)
        .child(
            SettingRow::new(tr!(&format!("{PUNCTUATION}-title")))
                .description(tr!("writing-style-styles-settings-punctuation-description"))
                .action(div().w(px(300.)).child(select(&self.punctuation, cx))),
        )
        .child(
            SettingRow::new(tr!("writing-style-styles-settings-technical"))
                .description(tr!(
                    "settings-captures-refinement-preserve-technical-description"
                ))
                .action(technical),
        )
        .child(
            SettingRow::new(tr!("writing-style-styles-settings-default"))
                .description(default_description)
                .when(!style.is_default, |row| {
                    row.action(
                        Button::new("make-default")
                            .outline()
                            .small()
                            .label(tr!("writing-style-styles-settings-make-default"))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.update_style(
                                    WritingStyleUpdate {
                                        is_default: Some(true),
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    )
                }),
        )
        .when(!style.is_default, |section| {
            let name = style.name.clone();
            section.child(
                SettingRow::new(tr!(
                    "writing-style-styles-settings-delete",
                    style = name.clone()
                ))
                .description(tr!("writing-style-styles-settings-delete-description"))
                .action(
                    destructive_outline(
                        "delete-style",
                        tr!("writing-style-styles-settings-delete-action"),
                        cx,
                    )
                    .on_click(move |_, window, cx| {
                        let page = page.clone();
                        open_confirm(
                            window,
                            cx,
                            Confirm {
                                title: tr!(
                                    "writing-style-styles-settings-delete-confirm-title",
                                    style = name.clone()
                                )
                                .into(),
                                description: Some(
                                    tr!("writing-style-styles-settings-delete-description").into(),
                                ),
                                confirm: tr!("writing-style-styles-settings-delete-action").into(),
                                cancel: tr!("common-cancel").into(),
                                destructive: true,
                                on_confirm: Rc::new(move |window, cx| {
                                    page.update(cx, |page, cx| page.delete_style(window, cx))
                                        .ok();
                                }),
                            },
                        );
                    }),
                ),
            )
        })
    }

    /// "Say it": dictates the description; the hold-to-talk keys show beside it.
    fn render_say_it(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let ours = self.take_owner == Some(TakeOwner::Description);
        let listening = ours && self.take == TakeState::Listening { from_button: true };
        let cleaning = ours && self.take == TakeState::Cleaning;
        let (bg, fg, border) = if listening {
            (t.accent, t.accent_foreground, t.accent)
        } else {
            (gpui_kit::transparent_black(), t.foreground, t.input)
        };
        div()
            .id("say-it")
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(32.))
            .px_3()
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_size(metrics::TEXT_BASE)
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .when(cleaning, |el| el.opacity(0.5))
            .when(!cleaning, |el| {
                el.cursor_pointer()
                    .when(!listening, |el| el.hover(|s| s.bg(t.secondary)))
                    .on_click(cx.listener(move |page, _, window, cx| {
                        if listening {
                            page.stop_take(cx);
                        } else {
                            page.description
                                .update(cx, |input, cx| input.focus(window, cx));
                            page.start_take(TakeOwner::Description, cx);
                        }
                    }))
            })
            .child(if listening {
                div()
                    .size(px(10.))
                    .rounded(px(2.))
                    .bg(fg)
                    .into_any_element()
            } else {
                icon(IconName::Mic, px(14.), t.accent).into_any_element()
            })
            .child(if listening {
                tr!("writing-style-styles-settings-description-stop")
            } else {
                tr!("writing-style-styles-settings-description-say")
            })
            .when(!listening && !self.hold_keys.is_empty(), |el| {
                el.child(chord_keys(&self.hold_keys, cx))
            })
            .into_any_element()
    }
}

/// An outline button in red, for deleting or forgetting.
pub(super) fn destructive_outline(
    id: &'static str,
    label: impl Into<SharedString>,
    cx: &App,
) -> Button {
    let t = theme(cx);
    Button::new(id)
        .outline()
        .small()
        .label(label)
        .border_color(t.destructive.opacity(0.4))
        .text_color(t.destructive)
}
