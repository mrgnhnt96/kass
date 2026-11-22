//! The app list beside the captures: "All apps", every app with captures
//! (most first) with its writing style, and "Unknown app". It shrinks to
//! icons with the header button.

use crate::format::format_count;
use crate::page::CapturesPage;
use crate::widgets::{app_tile, symbol_tile};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::CaptureAppFilter;
use kass_i18n::tr;
use kass_ui::{AppList, AppListLabels, AppListRow};

const TILE: Pixels = px(24.);

impl CapturesPage {
    fn app_rows(&self, cx: &mut Context<Self>) -> Vec<AppListRow> {
        let mut rows = vec![self.app_row(
            CaptureAppFilter::All,
            AppListRow::new(
                tr!("captures-apps-all"),
                symbol_tile(IconName::LayoutGrid, TILE, cx),
                format_count(self.apps.as_ref().map_or(0, |a| a.total)),
            ),
            cx,
        )];
        let Some(apps) = &self.apps else {
            return rows;
        };
        for app in &apps.apps {
            let name = app
                .app_name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| app.app_bundle_id.clone());
            let style = if app.confirmed {
                app.style_id.as_deref().and_then(|id| self.style_name(id))
            } else {
                Some(tr!("captures-apps-no-style"))
            };
            let tile = app_tile(self.host.as_ref(), &app.app_bundle_id, &name, TILE, cx);
            let row = AppListRow::new(name, tile, format_count(app.count))
                .style(style)
                .is_new(!app.confirmed);
            rows.push(self.app_row(CaptureAppFilter::App(app.app_bundle_id.clone()), row, cx));
        }
        if apps.unknown_count > 0 {
            let row = AppListRow::new(
                tr!("captures-apps-unknown"),
                symbol_tile(IconName::CircleQuestionMark, TILE, cx),
                format_count(apps.unknown_count),
            );
            rows.push(self.app_row(CaptureAppFilter::Unknown, row, cx));
        }
        rows
    }

    /// The row, highlighted when it's the filter, choosing it on click.
    fn app_row(
        &self,
        filter: CaptureAppFilter,
        row: AppListRow,
        cx: &mut Context<Self>,
    ) -> AppListRow {
        let page = cx.entity().downgrade();
        row.active(filter == self.filter)
            .on_click(move |window, cx| {
                page.update(cx, |page, cx| page.set_filter(filter.clone(), window, cx))
                    .ok();
            })
    }

    pub(crate) fn render_app_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = cx.entity().downgrade();
        AppList::new("capture-apps", app_list_labels())
            .collapsed(!self.apps_open)
            .rows(self.app_rows(cx))
            .on_toggle(move |_, cx| {
                page.update(cx, |page, cx| {
                    page.apps_open = !page.apps_open;
                    cx.notify();
                })
                .ok();
            })
    }
}

fn app_list_labels() -> AppListLabels {
    AppListLabels {
        title: tr!("captures-apps-title").into(),
        expand: tr!("captures-apps-expand").into(),
        collapse: tr!("captures-apps-collapse").into(),
        new: tr!("captures-apps-new").into(),
    }
}
