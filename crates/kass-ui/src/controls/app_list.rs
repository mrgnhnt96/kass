//! The app list Captures and Insights show on their left: one row per app
//! (with its writing style and count), the chosen one highlighted. A header
//! button shrinks it to a strip of icons, each with a tooltip.

use super::Handler;
use crate::theme::{metrics, theme};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::rc::Rc;

/// The list's own words, already translated.
#[derive(Clone)]
pub struct AppListLabels {
    /// The header, shown in capitals: "Apps".
    pub title: SharedString,
    /// The toggle's tooltip while collapsed.
    pub expand: SharedString,
    /// The toggle's tooltip while open.
    pub collapse: SharedString,
    /// Shown instead of the count on an app that hasn't been given a style.
    pub new: SharedString,
}

/// One row: "All apps", an app, or "Unknown app".
pub struct AppListRow {
    name: SharedString,
    tile: AnyElement,
    count: SharedString,
    style: Option<SharedString>,
    is_new: bool,
    active: bool,
    on_click: Option<Handler>,
}

impl AppListRow {
    /// `tile` is the app's icon (24px); `count` is already formatted.
    pub fn new(
        name: impl Into<SharedString>,
        tile: impl IntoElement,
        count: impl Into<SharedString>,
    ) -> Self {
        Self {
            name: name.into(),
            tile: tile.into_any_element(),
            count: count.into(),
            style: None,
            is_new: false,
            active: false,
            on_click: None,
        }
    }

    /// The writing style's name, under the app's.
    pub fn style(mut self, style: Option<impl Into<SharedString>>) -> Self {
        self.style = style.map(Into::into);
        self
    }

    /// Marks an app with no style yet: "New" in place of the count, a dot
    /// on the icon when collapsed.
    pub fn is_new(mut self, is_new: bool) -> Self {
        self.is_new = is_new;
        self
    }

    /// Whether this row is the filter that's showing.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

#[derive(IntoElement)]
pub struct AppList {
    id: SharedString,
    labels: AppListLabels,
    collapsed: bool,
    rows: Vec<AppListRow>,
    on_toggle: Option<Handler>,
}

impl AppList {
    /// `id` keeps one page's rows apart from another's: "capture-apps".
    pub fn new(id: impl Into<SharedString>, labels: AppListLabels) -> Self {
        Self {
            id: id.into(),
            labels,
            collapsed: false,
            rows: Vec::new(),
            on_toggle: None,
        }
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = AppListRow>) -> Self {
        self.rows.extend(rows);
        self
    }

    /// Called by the header button; the page flips its open state.
    pub fn on_toggle(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for AppList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let collapsed = self.collapsed;
        let tip = if collapsed {
            self.labels.expand.clone()
        } else {
            self.labels.collapse.clone()
        };
        let on_toggle = self.on_toggle;
        let toggle = div()
            .id("apps-toggle")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(32.))
            .rounded(metrics::RADIUS_MD)
            .cursor_pointer()
            .hover(|s| s.bg(t.muted))
            .child(
                svg()
                    .path(
                        if collapsed {
                            IconName::PanelLeftOpen
                        } else {
                            IconName::PanelLeftClose
                        }
                        .path(),
                    )
                    .size(px(16.))
                    .flex_none()
                    .text_color(t.muted_foreground),
            )
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .when_some(on_toggle, |b, toggle| {
                b.on_click(move |_, window, cx| toggle(window, cx))
            });

        let header = if collapsed {
            div().mb(px(6.)).child(toggle)
        } else {
            div()
                .h(px(32.))
                .mb(px(6.))
                .pl(px(10.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_CAPTION)
                        .line_height(metrics::LINE_XS)
                        .text_color(t.muted_foreground)
                        .child(self.labels.title.to_uppercase()),
                )
                .child(toggle)
        };

        let row_id: SharedString = format!("{}-row", self.id).into();
        let mut list = div()
            .id(ElementId::Name(self.id.clone()))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .overflow_y_scroll();
        for (i, row) in self.rows.into_iter().enumerate() {
            let active = row.active;
            let base = div()
                .id((row_id.clone(), i))
                .flex_none()
                .cursor_pointer()
                .rounded(px(7.))
                .when(active, |r| r.bg(t.muted))
                .when(!active, |r| r.hover(|s| s.bg(t.muted.opacity(0.5))))
                .when_some(row.on_click, |r, click| {
                    r.on_click(move |_, window, cx| click(window, cx))
                });
            let item = if collapsed {
                let label = [
                    Some(row.name.clone()),
                    row.style.clone(),
                    Some(row.count.clone()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                base.relative()
                    .size(px(44.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                    .child(row.tile)
                    .when(row.is_new, |b| {
                        b.child(
                            div()
                                .absolute()
                                .top(px(5.))
                                .right(px(5.))
                                .size(px(9.))
                                .rounded_full()
                                .border_2()
                                .border_color(t.sidebar)
                                .bg(t.accent),
                        )
                    })
            } else {
                base.w_full()
                    .h(px(44.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(metrics::TEXT_BASE)
                    .text_color(if active {
                        t.foreground
                    } else {
                        t.foreground.opacity(0.8)
                    })
                    .child(row.tile)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .line_height(px(16.))
                            .child(div().truncate().child(row.name))
                            .when_some(row.style, |c, style| {
                                c.child(
                                    div()
                                        .truncate()
                                        .text_size(metrics::TEXT_CAPTION)
                                        .text_color(t.muted_foreground)
                                        .child(style),
                                )
                            }),
                    )
                    .child(if row.is_new {
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.accent)
                            .child(self.labels.new.clone())
                    } else {
                        div()
                            .text_size(px(11.5))
                            .text_color(t.muted_foreground)
                            .child(row.count)
                    })
            };
            list = list.child(item);
        }

        div()
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .py(px(12.))
            .border_r_1()
            .border_color(t.border)
            .bg(t.sidebar)
            .map(|d| {
                if collapsed {
                    d.w(px(64.)).items_center().px(px(10.))
                } else {
                    d.w(px(220.)).px(px(12.))
                }
            })
            .child(header)
            .child(list)
    }
}
