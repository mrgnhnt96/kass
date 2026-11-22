//! The app list beside the stats, as Captures has it: "All apps", every app
//! with captures (most first) with its writing style, and "Unknown app". It
//! shrinks to icons with the header button.

use crate::format::format_count;
use crate::page::{InsightsPage, display_name};
use crate::widgets::{app_tile, symbol_tile};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use kass_api::types::CaptureAppFilter;
use kass_i18n::tr;
use kass_ui::{AppList, AppListLabels, AppListRow};

impl InsightsPage {
    fn app_rows(&self, cx: &mut Context<Self>) -> Vec<AppListRow> {
        let total = self.apps.as_ref().map_or(0, |a| a.total);
        let mut rows = vec![self.app_row(
            CaptureAppFilter::All,
            AppListRow::new(
                tr!("captures-apps-all"),
                symbol_tile(IconName::LayoutGrid, cx),
                format_count(total as f64),
            ),
            cx,
        )];
        let Some(apps) = &self.apps else {
            return rows;
        };
        for app in &apps.apps {
            let name = display_name(app);
            let style = if app.confirmed {
                app.style_id.as_deref().and_then(|id| self.style_name(id))
            } else {
                Some(tr!("captures-apps-no-style"))
            };
            let tile = app_tile(self.host.as_ref(), &app.app_bundle_id, &name, cx);
            let row = AppListRow::new(name, tile, format_count(app.count as f64))
                .style(style)
                .is_new(!app.confirmed);
            rows.push(self.app_row(CaptureAppFilter::App(app.app_bundle_id.clone()), row, cx));
        }
        if apps.unknown_count > 0 {
            let row = AppListRow::new(
                tr!("captures-apps-unknown"),
                symbol_tile(IconName::CircleQuestionMark, cx),
                format_count(apps.unknown_count as f64),
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
        row.active(filter == self.filter).on_click(move |_, cx| {
            page.update(cx, |page, cx| page.set_filter(filter.clone(), cx))
                .ok();
        })
    }

    pub(crate) fn render_app_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = cx.entity().downgrade();
        AppList::new("insights-apps", app_list_labels())
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
