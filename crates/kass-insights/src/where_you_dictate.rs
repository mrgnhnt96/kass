//! Where you dictate: every app's words and share of the period, most
//! first; clicking one shows only its stats. With an app shown, the row
//! compares it with all apps instead (its share, its pace, how often it
//! needed a fix), and clicking goes back to all apps.

use crate::format::{fix_rate, format_count, format_share};
use crate::page::{InsightsPage, display_name};
use crate::widgets::{app_tile, card, card_title, hint, symbol_tile, tabular};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{CaptureAppFilter, UsageStatsResponse};
use kass_i18n::tr;
use kass_ui::{metrics, theme};

struct Chip {
    tile: AnyElement,
    name: String,
    value: String,
    sub: String,
    /// What clicking it shows.
    filter: CaptureAppFilter,
}

/// "+12 wpm", "−4 wpm", or "—" when either has no pace.
fn pace_diff(pace: Option<u64>, all: Option<u64>) -> String {
    let (Some(pace), Some(all)) = (pace, all) else {
        return "—".into();
    };
    let diff = pace as i64 - all as i64;
    format!(
        "{}{} {}",
        if diff >= 0 { "+" } else { "−" },
        diff.abs(),
        tr!("insights-stats-wpm")
    )
}

impl InsightsPage {
    fn compare_chips(&self, stats: &UsageStatsResponse, cx: &App) -> Vec<Chip> {
        let (cur, all) = (&stats.current, &stats.all_apps);
        let or_dash = |v: Option<u64>| v.map_or_else(|| "—".to_string(), |v| v.to_string());
        vec![
            Chip {
                tile: self.app_tile(cx),
                name: tr!("insights-where-share-title"),
                value: format_share(cur.words, all.words),
                sub: tr!(
                    "insights-where-share-sub",
                    words = format_count(cur.words as f64),
                    total = format_count(all.words as f64)
                ),
                filter: CaptureAppFilter::All,
            },
            Chip {
                tile: self.app_tile(cx),
                name: tr!("insights-where-pace-title"),
                value: pace_diff(cur.pace_wpm, all.pace_wpm),
                sub: tr!(
                    "insights-where-pace-sub",
                    pace = or_dash(cur.pace_wpm),
                    all = or_dash(all.pace_wpm)
                ),
                filter: CaptureAppFilter::All,
            },
            Chip {
                tile: self.app_tile(cx),
                name: tr!("insights-where-fix-title"),
                value: fix_rate(cur.fixed_captures, cur.captures),
                sub: tr!(
                    "insights-where-fix-sub",
                    pct = fix_rate(all.fixed_captures, all.captures)
                ),
                filter: CaptureAppFilter::All,
            },
        ]
    }

    fn app_chips(&self, stats: &UsageStatsResponse, cx: &App) -> Vec<Chip> {
        let all = &stats.all_apps;
        // Shares go by words; a period of empty dictations falls back to how
        // many there were.
        let by_words = all.words > 0;
        stats
            .apps
            .iter()
            .filter(|app| app.captures > 0)
            .map(|app| {
                let share = if by_words {
                    format_share(app.words, all.words)
                } else {
                    format_share(app.captures, all.captures)
                };
                let (tile, name, filter) = match &app.app_bundle_id {
                    Some(id) => {
                        let name = self.app(id).map(display_name).unwrap_or_else(|| id.clone());
                        (
                            app_tile(self.host.as_ref(), id, &name, cx),
                            name,
                            CaptureAppFilter::App(id.clone()),
                        )
                    }
                    None => (
                        symbol_tile(IconName::CircleQuestionMark, cx),
                        tr!("captures-apps-unknown"),
                        CaptureAppFilter::Unknown,
                    ),
                };
                Chip {
                    tile,
                    name,
                    value: format_count(app.words as f64),
                    sub: tr!("insights-where-share", pct = share),
                    filter,
                }
            })
            .collect()
    }

    pub(crate) fn render_where(
        &self,
        stats: &UsageStatsResponse,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let comparing = self.filter != CaptureAppFilter::All;
        let chips = if comparing {
            self.compare_chips(stats, cx)
        } else {
            self.app_chips(stats, cx)
        };
        let title = if comparing {
            card_title(
                tr!(
                    "insights-where-compare-title",
                    app = self.app_name().unwrap_or_default()
                ),
                Some(hint(tr!("insights-where-compare-hint"), cx).into_any_element()),
            )
        } else {
            card_title(
                tr!("insights-where-title"),
                (chips.len() > 1).then(|| hint(tr!("insights-where-hint"), cx).into_any_element()),
            )
        };
        if chips.is_empty() {
            return card(cx).child(title).child(
                div()
                    .text_size(px(12.5))
                    .text_color(t.muted_foreground)
                    .child(tr!("insights-where-none")),
            );
        }
        let mut row = div().flex().flex_wrap().gap(px(10.));
        for (i, chip) in chips.into_iter().enumerate() {
            let filter = chip.filter;
            row = row.child(
                div()
                    .id(("where-chip", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(metrics::RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.muted.opacity(0.4))
                    .px(px(14.))
                    .py(px(10.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.muted).border_color(t.input))
                    .on_click(
                        cx.listener(move |page, _, _, cx| page.set_filter(filter.clone(), cx)),
                    )
                    .child(chip.tile)
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(
                                div()
                                    .max_w(px(160.))
                                    .truncate()
                                    .text_size(metrics::TEXT_XS)
                                    .line_height(px(16.))
                                    .text_color(t.muted_foreground)
                                    .child(chip.name),
                            )
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .line_height(px(20.))
                                    .font_weight(FontWeight::BOLD)
                                    .font_features(tabular())
                                    .child(chip.value),
                            ),
                    )
                    .child(
                        div()
                            .ml(px(4.))
                            .whitespace_nowrap()
                            .text_size(px(11.5))
                            .font_features(tabular())
                            .text_color(t.muted_foreground)
                            .child(chip.sub),
                    ),
            );
        }
        card(cx).child(title).child(row)
    }
}

#[cfg(test)]
mod tests {
    use super::pace_diff;

    #[test]
    fn pace_differences_carry_a_sign() {
        assert_eq!(pace_diff(Some(150), Some(138)), "+12 wpm");
        assert_eq!(pace_diff(Some(130), Some(134)), "−4 wpm");
        assert_eq!(pace_diff(Some(130), Some(130)), "+0 wpm");
        assert_eq!(pace_diff(None, Some(130)), "—");
    }
}
