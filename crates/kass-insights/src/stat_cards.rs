//! The four numbers at the top of Insights: words, pace, time saved, and the
//! weekdays dictated on, each against the equal period before. All time has
//! nothing before it, so its cards say where the count starts instead.
//! Today counts hours dictated in place of weekdays.

use crate::format::{
    format_change, format_count, format_day, format_saved, format_short_date, is_weekend,
    parse_local, percent_change,
};
use crate::page::{Hover, InsightsPage};
use crate::widgets::{icon, tabular, tip_below, tip_box};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{UsagePeriod, UsageStatsResponse};
use kass_i18n::tr;
use kass_ui::{metrics, theme};

/// Weekend days named in the tip before the rest are summed up.
const WEEKEND_DAYS_LISTED: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trend {
    Up,
    Down,
    Flat,
}

fn trend_of(change: i64) -> Trend {
    match change {
        c if c > 0 => Trend::Up,
        c if c < 0 => Trend::Down,
        _ => Trend::Flat,
    }
}

fn arrow(trend: Trend) -> &'static str {
    match trend {
        Trend::Up => "▲ ",
        Trend::Down => "▼ ",
        Trend::Flat => "",
    }
}

/// The tip on a note's info icon: a heading and what it refers to.
struct Tip {
    heading: String,
    /// Bold lines, then a muted one.
    lines: Vec<String>,
    more: Option<String>,
}

struct Card {
    label: String,
    value: String,
    unit: Option<String>,
    note: String,
    trend: Trend,
    tip: Option<Tip>,
}

/// The cards' notes and tips, worked out from the stats.
fn cards(stats: &UsageStatsResponse) -> [Card; 4] {
    let cur = &stats.current;
    let prev = stats.previous.as_ref();
    let start = parse_local(&stats.start);
    let compared = tr!(
        "insights-stats-compared",
        period = tr!(match stats.period {
            UsagePeriod::Today => "insights-previous-today",
            UsagePeriod::Days30 => "insights-previous-30d",
            _ => "insights-previous-7d",
        })
    );
    let compared_tip = |line: String| Tip {
        heading: compared.clone(),
        lines: vec![line],
        more: None,
    };
    let since = || {
        (
            tr!(
                "insights-stats-since",
                date = start.map(format_short_date).unwrap_or_default()
            ),
            Trend::Flat,
            Some(Tip {
                heading: tr!("insights-stats-first-dictation"),
                lines: start.map(format_day).into_iter().collect(),
                more: None,
            }),
        )
    };

    let (note, trend, tip) = match prev {
        None => since(),
        Some(prev) => {
            let pct = percent_change(cur.words, prev.words);
            let trend = match pct {
                None if cur.words > 0 => Trend::Up,
                None => Trend::Flat,
                Some(pct) => trend_of(pct),
            };
            let note = match pct {
                None if cur.words > 0 => {
                    format!("{}{}", arrow(Trend::Up), tr!("insights-stats-new"))
                }
                None | Some(0) => tr!("insights-stats-no-change"),
                Some(pct) => format!("{}{}", arrow(trend), format_change(pct)),
            };
            let tip = compared_tip(tr!(
                "insights-stats-prev-words",
                count = prev.words,
                formatted = format_count(prev.words as f64)
            ));
            (note, trend, Some(tip))
        }
    };
    let words = Card {
        label: tr!("insights-stats-words"),
        value: format_count(cur.words as f64),
        unit: None,
        note,
        trend,
        tip,
    };

    let (note, trend, tip) = match prev {
        None => since(),
        Some(prev) => {
            let tip = compared_tip(match prev.pace_wpm {
                Some(pace) => tr!("insights-stats-prev-pace", count = pace),
                None => tr!("insights-stats-no-pace"),
            });
            match (cur.pace_wpm, prev.pace_wpm) {
                (Some(now), Some(before)) => {
                    let change = now as i64 - before as i64;
                    let trend = trend_of(change);
                    let note = if change == 0 {
                        tr!("insights-stats-no-change")
                    } else {
                        format!(
                            "{}{} {}",
                            arrow(trend),
                            change.abs(),
                            tr!("insights-stats-wpm")
                        )
                    };
                    (note, trend, Some(tip))
                }
                _ => (tr!("insights-stats-nothing-before"), Trend::Flat, Some(tip)),
            }
        }
    };
    let pace = Card {
        label: tr!("insights-stats-pace"),
        value: cur
            .pace_wpm
            .map_or_else(|| "—".to_string(), |p| p.to_string()),
        unit: Some(tr!("insights-stats-wpm")),
        note,
        trend,
        tip,
    };

    let (note, trend, tip) = match prev {
        None => since(),
        Some(prev) => {
            let change = ((cur.time_saved_ms - prev.time_saved_ms) as f64 / 60_000.).round() as i64;
            let trend = trend_of(change);
            let note = if change == 0 {
                tr!("insights-stats-no-change")
            } else {
                format!("{}{}", arrow(trend), format_saved(change * 60_000))
            };
            let tip = compared_tip(tr!(
                "insights-stats-prev-saved",
                time = format_saved(prev.time_saved_ms)
            ));
            (note, trend, Some(tip))
        }
    };
    let saved = Card {
        label: tr!("insights-stats-saved"),
        value: format_saved(cur.time_saved_ms),
        unit: None,
        note,
        trend,
        tip,
    };

    let last = if stats.period == UsagePeriod::Today {
        hours_card(stats, &compared)
    } else {
        weekdays_card(stats)
    };
    [words, pace, saved, last]
}

fn hours_card(stats: &UsageStatsResponse, compared: &str) -> Card {
    let cur = &stats.current;
    let prev = stats.previous.as_ref();
    let change = prev.map_or(0, |p| cur.hours_dictated as i64 - p.hours_dictated as i64);
    let trend = trend_of(change);
    Card {
        label: tr!("insights-stats-hours"),
        value: cur.hours_dictated.to_string(),
        unit: None,
        note: if change == 0 {
            tr!("insights-stats-no-change")
        } else {
            format!(
                "{}{}",
                arrow(trend),
                tr!("insights-stats-prev-hours", count = change.abs())
            )
        },
        trend,
        tip: prev.map(|p| Tip {
            heading: compared.to_string(),
            lines: vec![tr!("insights-stats-prev-hours", count = p.hours_dictated)],
            more: None,
        }),
    }
}

fn weekdays_card(stats: &UsageStatsResponse) -> Card {
    let cur = &stats.current;
    let weekend = &cur.weekend_days;
    let (note, tip) = if weekend.is_empty() {
        (tr!("insights-stats-weekdays-only"), no_weekend_tip(stats))
    } else {
        let lines = weekend
            .iter()
            .take(WEEKEND_DAYS_LISTED)
            .map(|day| {
                tr!(
                    "insights-stats-weekend-day",
                    day = parse_local(&day.date).map(format_day).unwrap_or_default(),
                    words = tr!(
                        "insights-chart-words",
                        count = day.words,
                        formatted = format_count(day.words as f64)
                    )
                )
            })
            .collect();
        let rest = weekend.len().saturating_sub(WEEKEND_DAYS_LISTED);
        (
            tr!("insights-stats-weekend", count = weekend.len() as u64),
            Tip {
                heading: tr!("insights-stats-weekend-tip"),
                lines,
                more: (rest > 0).then(|| tr!("insights-stats-weekend-more", count = rest as u64)),
            },
        )
    };
    Card {
        label: tr!("insights-stats-weekdays"),
        value: cur.weekdays_dictated.to_string(),
        unit: Some(tr!("insights-stats-of", count = cur.weekdays_in_period)),
        note,
        trend: Trend::Flat,
        tip: Some(tip),
    }
}

/// The weekend days the period covers, by name when there are two or fewer.
fn no_weekend_tip(stats: &UsageStatsResponse) -> Tip {
    let days = weekend_days_between(&stats.start, &stats.end);
    if days.is_empty() || days.len() > 2 {
        return Tip {
            heading: tr!("insights-stats-no-weekend-any"),
            lines: Vec::new(),
            more: None,
        };
    }
    Tip {
        heading: tr!("insights-stats-no-weekend"),
        lines: vec![days.join(" or ")],
        more: None,
    }
}

/// Every Saturday and Sunday from `start` to `end`, as "Sat, Sep 26".
fn weekend_days_between(start: &str, end: &str) -> Vec<String> {
    let (Some(start), Some(end)) = (parse_local(start), parse_local(end)) else {
        return Vec::new();
    };
    let mut day = start;
    let mut days = Vec::new();
    while day <= end {
        if is_weekend(day.date()) {
            days.push(format_day(day));
        }
        day += chrono::Duration::days(1);
    }
    days
}

impl InsightsPage {
    pub(crate) fn render_stat_cards(
        &self,
        stats: &UsageStatsResponse,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx);
        let mut row = div().flex().gap(px(12.));
        for (i, card) in cards(stats).into_iter().enumerate() {
            let color = match card.trend {
                Trend::Up => t.success,
                Trend::Down => t.destructive,
                Trend::Flat => t.muted_foreground,
            };
            let note = div()
                .text_size(metrics::TEXT_XS)
                .line_height(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .font_features(tabular())
                .text_color(color)
                .child(card.note);
            let open = self.hover == Some(Hover::Note(i));
            let note =
                match card.tip {
                    None => note.into_any_element(),
                    Some(tip) => div()
                        .id(("stat-note", i))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .on_hover(cx.listener(move |page, hovered: &bool, _, cx| {
                            page.set_hover(Hover::Note(i), *hovered, cx)
                        }))
                        .child(note)
                        .child(icon(
                            IconName::Info,
                            px(14.),
                            if open {
                                t.foreground
                            } else {
                                t.muted_foreground.opacity(0.7)
                            },
                        ))
                        .when(open, |n| {
                            n.child(tip_below(
                                px(6.),
                                tip_box(cx)
                                    .child(div().text_color(t.muted_foreground).child(tip.heading))
                                    .children(tip.lines.into_iter().map(|line| {
                                        div().font_weight(FontWeight::SEMIBOLD).child(line)
                                    }))
                                    .children(tip.more.map(|more| {
                                        div().text_color(t.muted_foreground).child(more)
                                    })),
                            ))
                        })
                        .into_any_element(),
                };
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(6.))
                    .rounded(metrics::RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.card)
                    .px(px(16.))
                    .py(px(14.))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(metrics::TEXT_XS)
                            .line_height(px(16.))
                            .text_color(t.muted_foreground)
                            .child(card.label),
                    )
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .whitespace_nowrap()
                            .font_features(tabular())
                            .child(
                                div()
                                    .text_size(px(28.))
                                    .line_height(px(31.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(card.value),
                            )
                            .children(card.unit.map(|unit| {
                                div()
                                    .ml(px(4.))
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.muted_foreground)
                                    .child(unit)
                            })),
                    )
                    .child(note),
            );
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use super::weekend_days_between;

    #[test]
    fn finds_the_weekend_days_in_a_period() {
        assert_eq!(
            weekend_days_between("2026-09-21", "2026-09-27"),
            ["Sat, Sep 26", "Sun, Sep 27"]
        );
        assert!(weekend_days_between("2026-09-21", "2026-09-25").is_empty());
        assert!(weekend_days_between("soon", "2026-09-25").is_empty());
    }
}
