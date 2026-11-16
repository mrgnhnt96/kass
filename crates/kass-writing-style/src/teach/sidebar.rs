//! The conversations in any order, what each picked up, and finishing.

use super::{Busy, TeachView};
use crate::model::{CHIP_LIMIT, chip_key, group_kinds, kind_icon, kind_key, visible_chips};
use crate::widgets::{MenuTrigger, icon, menu_item, spinner};
use gpui_kit::assets::IconName;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{TeachChip, TeachConversation, TeachKind, TeachSession};
use kass_i18n::tr;
use kass_ui::{Button, metrics, theme};

/// One thing a reply showed, like "lowercase starts" or "“Hi Priya,” greeting".
pub(super) fn chip_label(chip: &TeachChip, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .whitespace_nowrap()
        .px_2()
        .py(px(1.))
        .rounded_full()
        .border_1()
        .border_color(t.input)
        .bg(t.secondary)
        .text_size(metrics::TEXT_CAPTION)
        .text_color(t.foreground)
        .child(tr!(
            &format!("writing-style-teach-chips-{}", chip_key(&chip.code)),
            value = chip.value.clone().unwrap_or_default()
        ))
}

/// The kinds of conversation, the style's own first. Shared by "Add a
/// conversation" and the header's kind picker, which checks `current`.
pub(super) fn kind_menu(
    view: WeakEntity<TeachView>,
    popover: Entity<PopoverState>,
    suggested: &[TeachKind],
    current: Option<TeachKind>,
    cx: &App,
) -> Div {
    let t = theme(cx).clone();
    let (own, other) = group_kinds(suggested);
    let heading = |text: String| {
        div()
            .px_2()
            .pt(px(6.))
            .pb_1()
            .font_family(t.mono_font.clone())
            .text_size(metrics::TEXT_2XS)
            .text_color(t.muted_foreground)
            .child(text.to_uppercase())
    };
    let item = |kind: TeachKind| {
        let view = view.clone();
        let popover = popover.clone();
        let key = kind_key(kind);
        menu_item(SharedString::from(format!("kind-{key}")), cx)
            .child(icon(kind_icon(kind), px(14.), t.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .child(tr!(&format!("writing-style-teach-kinds-{key}-label"))),
            )
            .when(current == Some(kind), |el| {
                el.child(icon(IconName::Check, px(14.), t.accent))
            })
            .on_click(move |_, window, cx| {
                popover.update(cx, |p, cx| p.dismiss(window, cx));
                view.update(cx, |view, cx| view.add(kind, window, cx)).ok();
            })
    };
    let has_own = !own.is_empty();
    div()
        .flex()
        .flex_col()
        .when(has_own, |el| {
            el.child(heading(tr!("writing-style-teach-kinds-own")))
                .children(own.iter().map(|kind| item(*kind)))
                .child(div().my_1().h(px(1.)).bg(t.border))
        })
        .child(heading(if has_own {
            tr!("writing-style-teach-kinds-other")
        } else {
            tr!("writing-style-teach-kinds-all")
        }))
        .children(other.iter().map(|kind| item(*kind)))
}

impl TeachView {
    pub(super) fn render_sidebar(
        &mut self,
        session: &TeachSession,
        selected: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let progress = if session.target == 0 {
            1.
        } else {
            (session.replies as f32 / session.target as f32).min(1.)
        };
        let adding = self.busy == Busy::Adding;
        let finishing = self.busy == Busy::Finishing;
        let view = cx.entity().downgrade();
        let suggested = session.suggested_kinds.clone();

        let add = MenuTrigger::new(
            "add-conversation",
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(if adding {
                    spinner("adding", px(14.), t.accent).into_any_element()
                } else {
                    icon(IconName::Plus, px(14.), t.accent).into_any_element()
                })
                .child(tr!("writing-style-teach-add-conversation")),
            move |el, open, cx| {
                let t = theme(cx);
                el.flex()
                    .items_center()
                    .mt(px(6.))
                    .h(px(36.))
                    .px_3()
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_dashed()
                    .border_color(t.input)
                    .text_size(metrics::TEXT_BASE)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.accent)
                    .when(open, |el| el.bg(t.secondary))
                    .hover(|s| s.bg(t.secondary))
            },
        );

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(290.))
            .h_full()
            .border_r_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(18.))
                    .pt(px(20.))
                    .pb(px(14.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(tr!("writing-style-teach-title")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_2()
                                    .text_size(metrics::TEXT_XS)
                                    .text_color(t.muted_foreground)
                                    .child(div().min_w_0().truncate().child(tr!(
                                        "writing-style-teach-any-order",
                                        style = self.style.name.clone()
                                    )))
                                    .child(
                                        div()
                                            .flex_none()
                                            .font_family(t.mono_font.clone())
                                            .text_size(metrics::TEXT_CAPTION)
                                            .child(tr!(
                                                "writing-style-teach-progress",
                                                count = session.replies,
                                                target = session.target
                                            )),
                                    ),
                            )
                            .child(
                                div().h(px(3.)).rounded(px(2.)).bg(t.input).child(
                                    div()
                                        .h_full()
                                        .rounded(px(2.))
                                        .bg(t.accent)
                                        .w(relative(progress)),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .id("teach-conversations")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(2.))
                    .px_2()
                    .overflow_y_scroll()
                    .children(
                        session
                            .conversations
                            .iter()
                            .map(|c| self.render_row(c, c.id == selected, cx).into_any_element()),
                    )
                    .child(
                        Popover::new("add-conversation-menu")
                            .w(px(290.))
                            .p_1()
                            .trigger(add)
                            .content(move |_, _, cx| {
                                kind_menu(view.clone(), cx.entity(), &suggested, None, cx)
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px(px(18.))
                    .py(px(14.))
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        Button::new("teach-finish")
                            .w_full()
                            .font_weight(FontWeight::SEMIBOLD)
                            .disabled(session.replies == 0 || finishing)
                            .loading(finishing)
                            .label(tr!("writing-style-teach-finish"))
                            .on_click(cx.listener(|view, _, window, cx| view.finish(window, cx))),
                    )
                    .child(
                        div()
                            .text_center()
                            .text_size(metrics::TEXT_CAPTION)
                            .text_color(t.muted_foreground)
                            .child(if session.replies > 0 {
                                tr!("writing-style-teach-finish-note", count = session.replies)
                            } else {
                                tr!("writing-style-teach-finish-empty")
                            }),
                    ),
            )
    }

    fn render_row(
        &self,
        conversation: &TeachConversation,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let id = conversation.id.clone();
        let expanded = self.expanded.contains(&id);
        let (shown, hidden) = visible_chips(&conversation.chips, CHIP_LIMIT, expanded);
        let last = if conversation.wrapped {
            tr!("writing-style-teach-wrapped-short")
        } else {
            conversation
                .messages
                .last()
                .map(|m| m.text.clone())
                .unwrap_or_default()
        };
        let replies = if conversation.reply_count > 0 {
            conversation.reply_count.to_string()
        } else {
            "—".into()
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(10.))
            .py_2()
            .rounded(metrics::RADIUS_LG)
            .when(selected, |el| el.bg(t.secondary))
            .when(conversation.wrapped && !selected, |el| el.opacity(0.6))
            .child(
                div()
                    .id(SharedString::from(format!("conversation-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |view, _, _, cx| view.select(id.clone(), cx)
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(28.))
                            .rounded(metrics::RADIUS_MD)
                            .bg(t.input.opacity(0.6))
                            .child(icon(
                                kind_icon(conversation.kind),
                                px(14.),
                                t.foreground.opacity(0.8),
                            )),
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
                                    .text_size(metrics::TEXT_BASE)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.foreground)
                                    .child(conversation.persona.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics::TEXT_XS)
                                    .text_color(t.muted_foreground)
                                    .child(last),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_CAPTION)
                            .text_color(t.muted_foreground)
                            .child(replies),
                    ),
            )
            .when(!conversation.chips.is_empty(), |el| {
                el.child(
                    div()
                        .id(SharedString::from(format!("chips-{id}")))
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .pl(px(38.))
                        .when(expanded, |el| el.max_h(px(52.)).overflow_y_scroll())
                        .children(shown.iter().map(|chip| chip_label(chip, cx)))
                        .when(hidden > 0, |el| {
                            let id = id.clone();
                            el.child(
                                div()
                                    .id("more-chips")
                                    .px_2()
                                    .py(px(1.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(t.accent.opacity(0.3))
                                    .bg(t.accent.opacity(0.1))
                                    .text_size(metrics::TEXT_CAPTION)
                                    .text_color(t.accent)
                                    .cursor_pointer()
                                    .child(format!("+{hidden}"))
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.expanded.insert(id.clone());
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(expanded, |el| {
                            let id = id.clone();
                            el.child(
                                div()
                                    .id("fewer-chips")
                                    .px_2()
                                    .py(px(1.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(t.input)
                                    .text_size(metrics::TEXT_CAPTION)
                                    .text_color(t.muted_foreground)
                                    .cursor_pointer()
                                    .child(tr!("writing-style-teach-show-less"))
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.expanded.remove(&id);
                                        cx.notify();
                                    })),
                            )
                        }),
                )
            })
    }
}
