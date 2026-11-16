//! When you dictate: words by weekday and hour, one accent ramp from fewer
//! to more, with the busiest slot named underneath. Hovering a cell shows
//! its words and time slot.

use crate::format::{TipAlign, format_count, hour_label, tip_align};
use crate::page::{Hover, InsightsPage};
use crate::widgets::{card, card_title, tip_above, tip_box, tip_title};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::UsageStatsResponse;
use kass_i18n::tr;
use kass_ui::theme;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
/// Hours always shown, widened to take in any dictation outside them.
const FIRST_HOUR: usize = 7;
const LAST_HOUR: usize = 22;
/// Label every third hour.
const LABEL_EVERY: usize = 3;
/// The lightest a cell with any words gets, so a quiet hour still reads as used.
const MIN_ALPHA: f32 = 0.15;
const LEGEND: [f32; 5] = [0.15, 0.36, 0.58, 0.79, 1.];
/// Weekends under this share of the words are called out as nearly empty.
const QUIET_WEEKEND: f64 = 0.05;
/// Weekdays with dictation needed before the weekend is called quiet.
const MIN_WEEKDAYS: usize = 3;

fn cell(grid: &[Vec<u64>], day: usize, hour: usize) -> u64 {
    grid.get(day)
        .and_then(|row| row.get(hour))
        .copied()
        .unwrap_or(0)
}

/// The hours shown: 7a to 10p, widened to any hour with dictation.
pub(crate) fn shown_hours(grid: &[Vec<u64>]) -> std::ops::RangeInclusive<usize> {
    let (first, last) = grid
        .iter()
        .flat_map(|row| {
            row.iter()
                .enumerate()
                .filter(|(_, v)| **v > 0)
                .map(|(h, _)| h)
        })
        .fold((FIRST_HOUR, LAST_HOUR), |(lo, hi), h| {
            (lo.min(h), hi.max(h))
        });
    first..=last
}

/// The weekday and hour with the most words, the first of any tie.
pub(crate) fn busiest(grid: &[Vec<u64>]) -> Option<(usize, usize)> {
    let mut best = None;
    let mut most = 0;
    for (d, row) in grid.iter().enumerate() {
        for (h, &v) in row.iter().enumerate() {
            if v > most {
                most = v;
                best = Some((d, h));
            }
        }
    }
    best
}

/// Whether the weekend is nearly empty, said only once there are enough
/// weekdays to set it against.
pub(crate) fn quiet_weekend(grid: &[Vec<u64>]) -> bool {
    let total: u64 = grid.iter().flatten().sum();
    let weekend: u64 = grid.iter().skip(5).take(2).flatten().sum();
    let weekdays_used = grid
        .iter()
        .take(5)
        .filter(|row| row.iter().any(|&v| v > 0))
        .count();
    weekdays_used >= MIN_WEEKDAYS && total > 0 && (weekend as f64 / total as f64) < QUIET_WEEKEND
}

fn slot(day: usize, hour: usize) -> String {
    format!(
        "{} {}–{}",
        WEEKDAYS[day % 7],
        hour_label(hour as i64),
        hour_label(hour as i64 + 1)
    )
}

impl InsightsPage {
    pub(crate) fn render_heat_map(
        &self,
        stats: &UsageStatsResponse,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let grid = &stats.heatmap;
        let hours: Vec<usize> = shown_hours(grid).collect();
        let max = grid.iter().flatten().copied().max().unwrap_or(0);
        let shade = |alpha: f32| t.accent.opacity(alpha);

        let legend = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(10.5))
            .font_weight(FontWeight::NORMAL)
            .text_color(t.muted_foreground)
            .child(tr!("insights-when-fewer"))
            .children(
                LEGEND
                    .iter()
                    .map(|&alpha| div().h(px(10.)).w(px(14.)).rounded(px(2.)).bg(shade(alpha))),
            )
            .child(tr!("insights-when-more"));

        let label = |text: &str| {
            div()
                .w(px(34.))
                .flex_none()
                .text_size(px(10.5))
                .line_height(px(14.))
                .text_color(t.muted_foreground)
                .child(text.to_string())
        };
        let mut rows = div().flex().flex_col().gap(px(3.));
        for (d, day) in WEEKDAYS.iter().enumerate() {
            let mut row = div().flex().items_center().gap(px(3.)).child(label(day));
            for (c, &h) in hours.iter().enumerate() {
                let v = cell(grid, d, h);
                let hovered = self.hover == Some(Hover::Heat(d, h));
                let mut square = div()
                    .id(("heat-cell", d * 24 + h))
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h(px(18.))
                    .rounded(px(3.))
                    .bg(if v > 0 && max > 0 {
                        shade(MIN_ALPHA + (1. - MIN_ALPHA) * (v as f32 / max as f32))
                    } else {
                        t.muted
                    })
                    .on_hover(cx.listener(move |page, hovered: &bool, _, cx| {
                        page.set_hover(Hover::Heat(d, h), *hovered, cx)
                    }));
                if hovered {
                    let align = tip_align(c, hours.len(), 3);
                    let left = match align {
                        TipAlign::Start => 0.,
                        TipAlign::Center => 0.5,
                        TipAlign::End => 1.,
                    };
                    let words = if v > 0 {
                        tr!(
                            "insights-chart-words",
                            count = v,
                            formatted = format_count(v as f64)
                        )
                    } else {
                        tr!("insights-chart-no-dictation")
                    };
                    square = square
                        // React's outline, 1.5px out from the cell.
                        .child(
                            div()
                                .absolute()
                                .top(px(-2.5))
                                .bottom(px(-2.5))
                                .left(px(-2.5))
                                .right(px(-2.5))
                                .rounded(px(4.5))
                                .border(px(1.5))
                                .border_color(t.foreground.opacity(0.6)),
                        )
                        .child(tip_above(
                            left,
                            1.,
                            px(6.),
                            align,
                            tip_box(cx).child(tip_title(words)).child(
                                div().text_color(t.muted_foreground).child(format!(
                                    "{day}, {}–{}",
                                    hour_label(h as i64),
                                    hour_label(h as i64 + 1)
                                )),
                            ),
                        ));
                }
                row = row.child(square);
            }
            rows = rows.child(row);
        }
        let first = hours.first().copied().unwrap_or(FIRST_HOUR);
        rows = rows.child(
            div()
                .flex()
                .gap(px(3.))
                .child(div().w(px(34.)).flex_none())
                .children(hours.iter().map(|&h| {
                    div().relative().flex_1().min_w_0().h(px(14.)).when(
                        (h - first) % LABEL_EVERY == 0,
                        |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .whitespace_nowrap()
                                    .text_size(px(10.))
                                    .line_height(px(14.))
                                    .text_color(t.muted_foreground)
                                    .child(hour_label(h as i64)),
                            )
                        },
                    )
                })),
        );

        let summary = match busiest(grid) {
            Some((d, h)) => {
                let mut text = tr!("insights-when-busiest", slot = slot(d, h));
                if quiet_weekend(grid) {
                    text.push(' ');
                    text.push_str(&tr!("insights-when-weekends-quiet"));
                }
                text
            }
            None => tr!("insights-when-none"),
        };

        card(cx)
            .flex_1()
            .child(card_title(
                tr!("insights-when-title"),
                Some(legend.into_any_element()),
            ))
            .child(rows)
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(16.))
                    .text_color(t.muted_foreground)
                    .child(summary),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{busiest, quiet_weekend, shown_hours, slot};

    fn grid(cells: &[(usize, usize, u64)]) -> Vec<Vec<u64>> {
        let mut grid = vec![vec![0; 24]; 7];
        for &(d, h, v) in cells {
            grid[d][h] = v;
        }
        grid
    }

    #[test]
    fn hours_widen_to_take_in_any_dictation() {
        assert_eq!(shown_hours(&grid(&[])), 7..=22);
        assert_eq!(shown_hours(&grid(&[(0, 5, 3), (6, 23, 1)])), 5..=23);
    }

    #[test]
    fn names_the_busiest_slot() {
        assert_eq!(busiest(&grid(&[])), None);
        let g = grid(&[(0, 9, 40), (2, 14, 90), (4, 14, 90)]);
        assert_eq!(busiest(&g), Some((2, 14)));
        assert_eq!(slot(2, 14), "Wed 2p–3p");
    }

    #[test]
    fn calls_weekends_quiet_only_against_enough_weekdays() {
        let weekdays = [(0, 9, 100), (1, 9, 100), (2, 9, 100)];
        assert!(quiet_weekend(&grid(&weekdays)));
        let mut busy = weekdays.to_vec();
        busy.push((5, 10, 50));
        assert!(!quiet_weekend(&grid(&busy)));
        assert!(!quiet_weekend(&grid(&weekdays[..2])));
    }
}
