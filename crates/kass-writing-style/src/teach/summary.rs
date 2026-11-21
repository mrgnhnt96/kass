//! After finishing: what was saved and learned, and whether to follow the
//! learned punctuation.

use super::{Busy, TeachView};
use crate::model::habit_key;
use crate::widgets::icon;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::TeachFinishResult;
use kass_i18n::tr;
use kass_ui::{Button, metrics, theme};

impl TeachView {
    pub(super) fn render_summary(
        &mut self,
        result: &TeachFinishResult,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let restarting = self.busy == Busy::Starting;
        let learned_on = self
            .page
            .upgrade()
            .is_some_and(|page| page.read(cx).learned_on());
        let habits: Vec<&'static str> = result
            .status
            .habits
            .iter()
            .filter_map(|habit| habit_key(*habit))
            .collect();
        let before_after = match (&result.before, &result.after) {
            (Some(before), Some(after)) if before != after => Some((before.clone(), after.clone())),
            _ => None,
        };
        let column_label = |text: String, color: Hsla| {
            div()
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_CAPTION)
                .text_color(color)
                .child(text.to_uppercase())
        };

        div()
            .id("teach-summary")
            .flex()
            .flex_col()
            .w_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px(px(28.))
                    .pt_6()
                    .pb(px(18.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.success)
                            .child(icon(IconName::Check, px(14.), t.success))
                            .child(tr!(
                                "writing-style-teach-summary-saved",
                                count = result.replies,
                                dictated = result.dictated
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(tr!("writing-style-summary-title")),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_BASE)
                            .text_color(t.muted_foreground)
                            .child(if result.dictated > 0 {
                                tr!("writing-style-teach-summary-examples")
                            } else {
                                tr!("writing-style-teach-summary-typed-only")
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(28.))
                    .py_2()
                    .child(
                        column_label(
                            tr!("writing-style-summary-habits-title"),
                            t.muted_foreground,
                        )
                        .pt_3()
                        .pb_1(),
                    )
                    .map(|el| {
                        if habits.is_empty() {
                            el.child(
                                div()
                                    .py_3()
                                    .text_size(metrics::TEXT_BASE)
                                    .text_color(t.muted_foreground)
                                    .child(tr!("writing-style-summary-no-habits")),
                            )
                        } else {
                            el.children(habits.iter().enumerate().map(|(index, key)| {
                                div()
                                    .py(px(14.))
                                    .when(index + 1 < habits.len(), |el| {
                                        el.border_b_1().border_color(t.border.opacity(0.7))
                                    })
                                    .child(
                                        div()
                                            .text_size(metrics::TEXT_SM)
                                            .text_color(t.foreground)
                                            .child(tr!(&format!("writing-style-habits-{key}"))),
                                    )
                                    .child(
                                        div()
                                            .mt(px(2.))
                                            .font_family(t.mono_font.clone())
                                            .text_size(metrics::TEXT_XS)
                                            .text_color(t.muted_foreground)
                                            .child(tr!(&format!(
                                                "writing-style-habit-chips-{key}"
                                            ))),
                                    )
                            }))
                        }
                    }),
            )
            .when_some(before_after, |el, (before, after)| {
                el.child(
                    div()
                        .flex()
                        .gap_4()
                        .px(px(28.))
                        .pt_2()
                        .pb(px(20.))
                        .text_size(metrics::TEXT_BASE)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .gap_2()
                                .child(column_label(
                                    tr!("writing-style-summary-before"),
                                    t.muted_foreground,
                                ))
                                .child(
                                    div()
                                        .p_3()
                                        .rounded(metrics::RADIUS_LG)
                                        .border_1()
                                        .border_color(t.border)
                                        .bg(t.card)
                                        .text_color(t.muted_foreground)
                                        .child(before),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .gap_2()
                                .child(column_label(tr!("writing-style-summary-after"), t.accent))
                                .child(
                                    div()
                                        .p_3()
                                        .rounded(metrics::RADIUS_LG)
                                        .border_1()
                                        .border_color(t.accent.opacity(0.3))
                                        .bg(t.accent.opacity(0.04))
                                        .text_color(t.foreground)
                                        .child(after),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(28.))
                    .py_4()
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        Button::new("keep-teaching")
                            .ghost()
                            .text_color(t.muted_foreground)
                            .loading(restarting)
                            .disabled(restarting)
                            .label(tr!("writing-style-teach-summary-keep-teaching"))
                            .on_click(cx.listener(|view, _, window, cx| view.start(window, cx))),
                    )
                    .child(div().flex_1())
                    .map(|el| {
                        if learned_on || !result.status.ready {
                            el.child(
                                Button::new("summary-done")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .px(px(18.))
                                    .label(tr!("writing-style-summary-done"))
                                    .on_click(
                                        cx.listener(|view, _, window, cx| view.dismiss(window, cx)),
                                    ),
                            )
                        } else {
                            el.child(
                                Button::new("keep-current")
                                    .outline()
                                    .label(tr!("writing-style-summary-keep-current"))
                                    .on_click(
                                        cx.listener(|view, _, window, cx| view.dismiss(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("use-learned")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .label(tr!("writing-style-summary-use-learned"))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.use_learned(window, cx)
                                    })),
                            )
                        }
                    }),
            )
    }
}
