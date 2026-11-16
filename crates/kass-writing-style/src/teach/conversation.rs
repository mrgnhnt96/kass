//! The open conversation: its header, the thread, the note for the answer
//! and the reply box.

use super::sidebar::kind_menu;
use super::{Busy, TeachView};
use crate::dictation::TakeState;
use crate::model::{initial, kind_icon, kind_key};
use crate::widgets::{MenuTrigger, icon, kbd, spinner};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{TeachConversation, TeachNote, TeachSession, TeachTrick};
use kass_dictionary::chord_keys;
use kass_i18n::tr;
use kass_ui::{Button, ButtonSize, metrics, theme};

fn trick_key(trick: TeachTrick) -> Option<&'static str> {
    match trick {
        TeachTrick::ChangeOfMind => Some("change-of-mind"),
        TeachTrick::Long => Some("long"),
        TeachTrick::Unknown => None,
    }
}

impl TeachView {
    pub(super) fn render_conversation(
        &mut self,
        session: &TeachSession,
        conversation: &TeachConversation,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let sending = self.busy == Busy::Sending;
        let turn = conversation.messages.len();
        let pending = self
            .sent
            .as_ref()
            .filter(|(_, at)| sending && *at == turn)
            .map(|(text, _)| text.clone());

        let mut messages: Vec<(bool, String)> = conversation
            .messages
            .iter()
            .map(|m| (m.from_you, m.text.clone()))
            .collect();
        if let Some(text) = pending {
            messages.push((true, text));
        }
        let you_initial = tr!("writing-style-teach-you-initial");
        let you = tr!("writing-style-teach-you");

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_header(session, conversation, cx))
            .child(
                div()
                    .id("teach-thread")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(14.))
                    .px_6()
                    .py(px(20.))
                    .overflow_y_scroll()
                    .track_scroll(&self.thread)
                    .child(div().flex_1())
                    .children(messages.into_iter().map(|(from_you, text)| {
                        div()
                            .flex()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .size(px(28.))
                                    .rounded(metrics::RADIUS_MD)
                                    .text_size(metrics::TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .when(from_you, |el| {
                                        el.bg(t.accent.opacity(0.15)).text_color(t.accent)
                                    })
                                    .when(!from_you, |el| {
                                        el.bg(t.secondary).text_color(t.foreground)
                                    })
                                    .child(if from_you {
                                        you_initial.clone()
                                    } else {
                                        initial(&conversation.persona)
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(metrics::TEXT_XS)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(t.foreground)
                                            .child(if from_you {
                                                you.clone()
                                            } else {
                                                conversation.persona.clone()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(metrics::TEXT_SM)
                                            .line_height(relative(1.6))
                                            .text_color(t.foreground)
                                            .child(text),
                                    ),
                            )
                    }))
                    .when(sending, |el| {
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .pl(px(36.))
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.muted_foreground)
                                .child(spinner("typing", px(12.), t.muted_foreground))
                                .child(tr!(
                                    "writing-style-teach-typing",
                                    persona = conversation.persona.clone()
                                )),
                        )
                    }),
            )
            .child(if conversation.wrapped {
                div()
                    .px_6()
                    .py_4()
                    .border_t_1()
                    .border_color(t.border)
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(tr!("writing-style-teach-wrapped"))
            } else {
                self.render_reply(conversation, sending, cx)
            })
            .child(
                div()
                    .id("teach-close")
                    .absolute()
                    .top_4()
                    .right_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(metrics::RADIUS_SM)
                    .cursor_pointer()
                    .opacity(0.7)
                    .hover(|s| s.opacity(1.))
                    .child(icon(IconName::X, px(16.), t.foreground))
                    .on_click(cx.listener(|view, _, window, cx| view.close(window, cx))),
            )
    }

    fn render_header(
        &self,
        session: &TeachSession,
        conversation: &TeachConversation,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let key = kind_key(conversation.kind);
        let kind_icon = kind_icon(conversation.kind);
        let switching = matches!(self.busy, Busy::Adding | Busy::Switching);
        let sending = self.busy == Busy::Sending;
        let view = cx.entity().downgrade();
        let suggested = session.suggested_kinds.clone();
        let current = conversation.kind;

        let kind_button = MenuTrigger::new(
            "kind-picker",
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon(kind_icon, px(14.), t.foreground))
                .child(tr!(&format!("writing-style-teach-kinds-{key}-short")))
                .child(icon(IconName::ChevronDown, px(12.), t.muted_foreground)),
            move |el, open, cx| {
                let t = theme(cx);
                el.flex()
                    .items_center()
                    .h(px(32.))
                    .px_3()
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(if open { t.ring } else { t.input })
                    .bg(t.background)
                    .text_size(metrics::TEXT_BASE)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .when(switching, |el| el.opacity(0.5))
                    .hover(|s| s.bg(t.secondary))
            },
        );

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .pl_6()
            .pr(px(52.))
            .py(px(14.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded(metrics::RADIUS_LG)
                    .bg(t.accent.opacity(0.15))
                    .child(icon(kind_icon, px(16.), t.accent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(
                                conversation
                                    .title
                                    .clone()
                                    .unwrap_or_else(|| conversation.persona.clone()),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.muted_foreground)
                            .child(tr!(
                                "writing-style-teach-subtitle",
                                kind = tr!(&format!("writing-style-teach-kinds-{key}-label")),
                                relation = conversation.relation.clone()
                            )),
                    ),
            )
            .map(|el| {
                if switching {
                    el.child(kind_button)
                } else {
                    el.child(
                        Popover::new("kind-menu")
                            .anchor(Anchor::TopRight)
                            .w(px(300.))
                            .p_1()
                            .trigger(kind_button)
                            .content(move |_, _, cx| {
                                let t = theme(cx).clone();
                                kind_menu(view.clone(), cx.entity(), &suggested, Some(current), cx)
                                    .child(
                                        div()
                                            .mt_1()
                                            .px_2()
                                            .pt_2()
                                            .pb_1()
                                            .border_t_1()
                                            .border_color(t.border)
                                            .text_size(px(11.5))
                                            .text_color(t.muted_foreground)
                                            .child(tr!("writing-style-teach-kind-menu-note")),
                                    )
                            }),
                    )
                }
            })
            .child(
                Button::new("new-theme")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(tr!("writing-style-teach-new-theme"))
                    .disabled(switching || sending)
                    .on_click(cx.listener(|view, _, window, cx| view.new_theme(window, cx))),
            )
            .when(!conversation.wrapped, |el| {
                el.child(
                    Button::new("wrap-up")
                        .ghost()
                        .small()
                        .text_color(t.muted_foreground)
                        .label(tr!("writing-style-teach-wrap-up"))
                        .disabled(switching || sending)
                        .on_click(cx.listener(|view, _, window, cx| view.wrap_up(window, cx))),
                )
            })
    }

    fn render_reply(
        &self,
        conversation: &TeachConversation,
        sending: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let can_send = self.can_send(cx);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap_3()
            .px_6()
            .pt(px(20.))
            .pb_4()
            .border_t_1()
            .border_color(t.border)
            .when_some(
                conversation.note.as_ref().filter(|_| !sending),
                |el, note| el.child(for_you(note, &conversation.persona, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(10.))
                    .child(
                        div().flex_1().child(
                            Textarea::new(&self.reply)
                                .w_full()
                                .min_h(px(76.))
                                .max_h(px(192.))
                                .px(px(14.))
                                .py(px(10.))
                                .rounded(metrics::RADIUS_LG)
                                .border_color(t.accent)
                                .bg(t.background)
                                .text_size(metrics::TEXT_SM)
                                .disabled(sending),
                        ),
                    )
                    .child(
                        Button::new("send-reply")
                            .icon(IconName::ArrowUp)
                            .size(ButtonSize::Icon)
                            .rounded_full()
                            .tooltip(tr!("writing-style-teach-send"))
                            .loading(sending)
                            .disabled(!can_send)
                            .on_click(cx.listener(|view, _, window, cx| view.send(window, cx))),
                    ),
            )
            .child(self.render_dictation_row(&conversation.persona, cx))
    }

    /// How to dictate the reply, following how the current take started.
    fn render_dictation_row(&self, persona: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let hold = self.hold_keys.clone();
        let hands_free = self.hands_free_keys.clone();

        let action = match self.take {
            TakeState::Listening { from_button: true } => Some(
                Button::new("teach-stop")
                    .small()
                    .icon(IconName::Square)
                    .label(tr!("writing-style-teach-stop"))
                    .on_click(cx.listener(|view, _, _, cx| view.dictate(false, cx)))
                    .into_any_element(),
            ),
            _ if self.dictation => Some(
                div()
                    .id("teach-dictate")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_3()
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(t.input)
                    .bg(t.background)
                    .text_size(metrics::TEXT_BASE)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .map(|el| {
                        if self.take == TakeState::Idle {
                            el.cursor_pointer()
                                .hover(|s| s.bg(t.secondary))
                                .on_click(cx.listener(|view, _, _, cx| view.dictate(true, cx)))
                        } else {
                            el.opacity(0.5)
                        }
                    })
                    .child(icon(IconName::Mic, px(14.), t.accent))
                    .child(tr!("writing-style-teach-dictate"))
                    .when(!hold.is_empty(), |el| el.child(chord_keys(&hold, cx)))
                    .into_any_element(),
            ),
            _ => None,
        };

        let status = match self.take {
            TakeState::Listening { from_button } => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.accent)
                        .child(tr!("writing-style-teach-listening")),
                )
                .map(|el| {
                    if from_button {
                        el.child(tr!("writing-style-teach-finish-button"))
                    } else {
                        el.child(tr!("writing-style-teach-finish-hold"))
                            .child(chord_keys(&hold, cx))
                            .when(!hands_free.is_empty(), |el| {
                                el.child(tr!("writing-style-teach-finish-hands-free"))
                                    .child(chord_keys(&hands_free, cx))
                            })
                    }
                })
                .child(
                    div()
                        .text_color(t.muted_foreground.opacity(0.8))
                        .child(tr!("writing-style-teach-esc-cancels")),
                ),
            TakeState::Cleaning => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(spinner("cleaning", px(12.), t.muted_foreground))
                .child(tr!("writing-style-teach-cleaning")),
            _ => div().child(tr!(
                "writing-style-teach-answers-after",
                persona = persona.to_string()
            )),
        };

        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(32.))
            .text_size(metrics::TEXT_XS)
            .text_color(t.muted_foreground)
            .children(action)
            .child(div().flex_1().min_w_0().child(status))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .child(kbd("⌘⏎", cx))
                    .child(tr!("writing-style-teach-send-short")),
            )
    }
}

/// The facts for the answer, and sometimes a trick to try with an example.
fn for_you(note: &TeachNote, persona: &str, cx: &App) -> Div {
    let t = theme(cx);
    let trick = note.trick.and_then(trick_key);
    div()
        .relative()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px_3()
        .pt_4()
        .pb(px(10.))
        .rounded(metrics::RADIUS_LG)
        .border_1()
        .border_color(t.accent.opacity(0.3))
        .bg(t.accent.opacity(0.1))
        .text_size(metrics::TEXT_BASE)
        .line_height(relative(1.375))
        .text_color(t.foreground)
        .child(
            div()
                .absolute()
                .top(px(-8.))
                .left(px(10.))
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(t.accent)
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_2XS)
                .text_color(t.accent_foreground)
                .child(tr!("writing-style-teach-for-you").to_uppercase()),
        )
        .map(|el| {
            if !note.answers.is_empty() {
                el.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr!("writing-style-teach-what-to-answer")),
                )
                .child(div().flex().flex_col().gap(px(2.)).children(
                    note.answers.iter().map(|pair| {
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(220.))
                                    .text_color(t.foreground.opacity(0.7))
                                    .child(pair.ask.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(14.))
                                    .text_color(t.foreground.opacity(0.5))
                                    .child("→"),
                            )
                            .child(div().flex_1().min_w_0().child(pair.answer.clone()))
                    }),
                ))
            } else if let Some(facts) = note.facts.clone() {
                el.child(
                    div()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr!("writing-style-teach-tell", persona = persona.to_string())),
                        )
                        .child(facts),
                )
            } else {
                el
            }
        })
        .when_some(trick, |el, key| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .mt_1()
                    .pt_2()
                    .border_t_1()
                    .border_color(t.accent.opacity(0.25))
                    .child(
                        div()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!("writing-style-teach-try-this")),
                            )
                            .child(tr!(&format!("writing-style-teach-tricks-{key}-text"))),
                    )
                    .child(div().text_color(t.foreground.opacity(0.7)).child(
                        match note.example.clone() {
                            Some(example) => tr!(
                                &format!("writing-style-teach-tricks-{key}-example-with"),
                                example = example
                            ),
                            None => tr!(&format!("writing-style-teach-tricks-{key}-example")),
                        },
                    )),
            )
        })
}
