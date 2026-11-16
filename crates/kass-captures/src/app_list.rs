//! The app list beside the captures: "All apps", every app with captures
//! (most first) with its writing style, and "Unknown app". It shrinks to
//! icons with the header button.

use crate::page::CapturesPage;
use crate::widgets::{app_tile, mono_label, symbol_tile};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::CaptureAppFilter;
use kass_i18n::tr;
use kass_ui::{metrics, theme};

use crate::format::format_count;
use crate::widgets::icon_button;

struct Row {
    filter: CaptureAppFilter,
    name: String,
    count: u64,
    tile: AnyElement,
    style: Option<String>,
    is_new: bool,
}

impl CapturesPage {
    fn app_rows(&self, cx: &App) -> Vec<Row> {
        let tile = px(24.);
        let mut rows = vec![Row {
            filter: CaptureAppFilter::All,
            name: tr!("captures-apps-all"),
            count: self.apps.as_ref().map_or(0, |a| a.total),
            tile: symbol_tile(IconName::LayoutGrid, tile, cx).into_any_element(),
            style: None,
            is_new: false,
        }];
        let Some(apps) = &self.apps else {
            return rows;
        };
        for app in &apps.apps {
            let name = app
                .app_name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| app.app_bundle_id.clone());
            rows.push(Row {
                filter: CaptureAppFilter::App(app.app_bundle_id.clone()),
                tile: app_tile(self.host.as_ref(), &app.app_bundle_id, &name, tile, cx),
                name,
                count: app.count,
                style: if app.confirmed {
                    app.style_id.as_deref().and_then(|id| self.style_name(id))
                } else {
                    Some(tr!("captures-apps-no-style"))
                },
                is_new: !app.confirmed,
            });
        }
        if apps.unknown_count > 0 {
            rows.push(Row {
                filter: CaptureAppFilter::Unknown,
                name: tr!("captures-apps-unknown"),
                count: apps.unknown_count,
                tile: symbol_tile(IconName::CircleQuestionMark, tile, cx).into_any_element(),
                style: None,
                is_new: false,
            });
        }
        rows
    }

    pub(crate) fn render_app_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let collapsed = !self.apps_open;
        let toggle = icon_button(
            "apps-toggle",
            if collapsed {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            },
            cx,
        )
        .tooltip(move |window, cx| {
            Tooltip::new(tr!(if collapsed {
                "captures-apps-expand"
            } else {
                "captures-apps-collapse"
            }))
            .build(window, cx)
        })
        .on_click(cx.listener(|page, _, _, cx| {
            page.apps_open = !page.apps_open;
            cx.notify();
        }));

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
                .child(mono_label(
                    tr!("captures-apps-title"),
                    t.muted_foreground,
                    cx,
                ))
                .child(toggle)
        };

        let mut list = div()
            .id("capture-apps")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .overflow_y_scroll();
        for (i, row) in self.app_rows(cx).into_iter().enumerate() {
            let active = row.filter == self.filter;
            let filter = row.filter.clone();
            let base = div()
                .id(("capture-app", i))
                .cursor_pointer()
                .rounded(px(7.))
                .when(active, |r| r.bg(t.muted))
                .when(!active, |r| r.hover(|s| s.bg(t.muted.opacity(0.5))))
                .on_click(cx.listener(move |page, _, window, cx| {
                    page.set_filter(filter.clone(), window, cx)
                }));
            let item = if collapsed {
                let label = [
                    Some(row.name.clone()),
                    row.style.clone(),
                    Some(format_count(row.count)),
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
                            .child(tr!("captures-apps-new"))
                    } else {
                        div()
                            .text_size(px(11.5))
                            .text_color(t.muted_foreground)
                            .child(format_count(row.count))
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
