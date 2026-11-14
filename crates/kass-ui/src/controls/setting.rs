//! Settings pages: a titled section of rows, each a title and description on
//! the left and its control on the right.

use crate::theme::{metrics, theme};
use gpui_kit::prelude::*;
use gpui_kit::*;

/// A group of [`SettingRow`]s under a small monospace heading.
#[derive(IntoElement)]
pub struct SettingSection {
    title: SharedString,
    description: Option<AnyElement>,
    rows: Vec<AnyElement>,
}

impl SettingSection {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            rows: Vec::new(),
        }
    }

    pub fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }
}

impl ParentElement for SettingSection {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.rows.extend(elements);
    }
}

impl RenderOnce for SettingSection {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let divider = t.border.opacity(0.7);
        let count = self.rows.len();
        div()
            .flex()
            .flex_col()
            .mb(px(28.))
            .child(
                div()
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_CAPTION)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .child(self.title.to_uppercase()),
            )
            .when_some(self.description, |s, description| {
                s.child(
                    div()
                        .mt_1()
                        .text_size(metrics::TEXT_XS)
                        .line_height(metrics::LINE_XS)
                        .text_color(t.muted_foreground)
                        .child(description),
                )
            })
            .child(
                div()
                    .mt_2()
                    .flex()
                    .flex_col()
                    .children(self.rows.into_iter().enumerate().map(|(i, row)| {
                        div()
                            .border_b_1()
                            .border_color(divider)
                            .when(i + 1 == count, |row| row.border_color(t.border))
                            .child(row)
                    })),
            )
    }
}

/// One setting: title, optional description, the control on the right, and
/// optionally more below (a list, a picker) spanning the row.
#[derive(IntoElement)]
pub struct SettingRow {
    title: AnyElement,
    description: Option<AnyElement>,
    action: Option<AnyElement>,
    below: Vec<AnyElement>,
}

impl SettingRow {
    pub fn new(title: impl IntoElement) -> Self {
        Self {
            title: title.into_any_element(),
            description: None,
            action: None,
            below: Vec::new(),
        }
    }

    pub fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    /// The control on the right: a toggle, select, button.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl ParentElement for SettingRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.below.extend(elements);
    }
}

impl RenderOnce for SettingRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let has_below = !self.below.is_empty();
        div()
            .py(px(14.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(32.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(metrics::TEXT_SM)
                                    .line_height(metrics::TEXT_SM)
                                    .text_color(t.foreground)
                                    .child(self.title),
                            )
                            .when_some(self.description, |s, description| {
                                s.child(
                                    div()
                                        .mt_1()
                                        .text_size(metrics::TEXT_XS)
                                        .line_height(px(19.5))
                                        .text_color(t.muted_foreground)
                                        .child(description),
                                )
                            }),
                    )
                    .when_some(self.action, |s, action| {
                        s.child(div().flex_none().child(action))
                    }),
            )
            .when(has_below, |s| s.child(div().mt_3().children(self.below)))
    }
}
