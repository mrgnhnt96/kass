//! Words over the period against the period before, on one axis: the
//! current period solid with a light fill, the one before dashed. Today is
//! by hour against yesterday; All time is weekly totals with nothing to
//! compare. Hovering a point shows both values and the change.

use crate::format::{
    TipAlign, axis_top, format_change, format_count, format_day, format_short_date, format_weekday,
    hour_label, hour_of, parse_local, percent_change, tip_align,
};
use crate::page::{Hover, InsightsPage};
use crate::widgets::{
    add_curve, anchor, card, card_title, chart_space, dot_at, legend_line, tabular, tip_above,
    tip_box, tip_title,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{UsageBucket, UsagePeriod, UsagePoint, UsageStatsResponse};
use kass_i18n::tr;
use kass_ui::theme;

const W: f64 = 700.;
const H: f64 = 170.;
/// Hours Today always shows, widened to take in any dictation outside them.
const FIRST_HOUR: usize = 7;
const LAST_HOUR: usize = 22;
/// Past this many points, dots only show on the hovered one.
const MAX_DOTS: usize = 14;
/// About how many x labels fit without crowding.
const MAX_LABELS: usize = 8;
/// Room left of the chart for the axis labels.
const AXIS: Pixels = px(44.);

/// Today's hours from 7a to 10p, or wider when there was dictation outside them.
pub(crate) fn shown_points(stats: &UsageStatsResponse) -> &[UsagePoint] {
    if stats.bucket != UsageBucket::Hour {
        return &stats.series;
    }
    let active = stats
        .series
        .iter()
        .enumerate()
        .filter(|(_, p)| p.words.unwrap_or(0) > 0 || p.previous_words.unwrap_or(0) > 0)
        .map(|(i, _)| i);
    let (first, last) = active.fold((FIRST_HOUR, LAST_HOUR), |(lo, hi), i| {
        (lo.min(i), hi.max(i))
    });
    let last = (last + 1).min(stats.series.len());
    stats.series.get(first..last).unwrap_or_default()
}

/// Every `step`th point gets an x label, counted back from the newest so
/// it's always labeled.
pub(crate) fn label_step(bucket: UsageBucket, count: usize) -> usize {
    if bucket == UsageBucket::Hour {
        return 3;
    }
    count.div_ceil(MAX_LABELS).max(1)
}

fn bucket_key(bucket: UsageBucket) -> &'static str {
    match bucket {
        UsageBucket::Hour => "insights-chart-hour",
        UsageBucket::Week => "insights-chart-week",
        _ => "insights-chart-day",
    }
}

fn legend_keys(period: UsagePeriod) -> (&'static str, &'static str) {
    match period {
        UsagePeriod::Today => (
            "insights-legend-current-today",
            "insights-legend-previous-today",
        ),
        UsagePeriod::Days30 => (
            "insights-legend-current-30d",
            "insights-legend-previous-30d",
        ),
        UsagePeriod::All => ("insights-legend-current-all", "insights-legend-previous-7d"),
        _ => ("insights-legend-current-7d", "insights-legend-previous-7d"),
    }
}

fn words(v: Option<u64>) -> String {
    match v {
        Some(v) if v > 0 => tr!(
            "insights-chart-words",
            count = v,
            formatted = format_count(v as f64)
        ),
        _ => tr!("insights-chart-no-dictation"),
    }
}

fn point_title(bucket: UsageBucket, point: &UsagePoint, last: bool) -> String {
    let Some(date) = parse_local(&point.start) else {
        return point.start.clone();
    };
    match bucket {
        UsageBucket::Hour => {
            let hour = hour_of(date);
            format!(
                "{}–{} {}",
                hour_label(hour),
                hour_label(hour + 1),
                tr!("insights-chart-today")
            )
        }
        UsageBucket::Week => {
            let week = tr!("insights-chart-week-of", date = format_short_date(date));
            if last {
                format!("{week} · {}", tr!("insights-chart-this-week"))
            } else {
                week
            }
        }
        _ if last => format!("{} · {}", format_day(date), tr!("insights-chart-today")),
        _ => format_day(date),
    }
}

/// A point's x label, and the date under it for days.
fn x_label(bucket: UsageBucket, point: &UsagePoint) -> (String, Option<String>) {
    let Some(date) = parse_local(&point.start) else {
        return (String::new(), None);
    };
    match bucket {
        UsageBucket::Hour => (hour_label(hour_of(date)), None),
        UsageBucket::Week => (format_short_date(date), None),
        _ => (
            format_weekday(date).chars().take(1).collect(),
            Some(format_short_date(date)),
        ),
    }
}

impl InsightsPage {
    pub(crate) fn render_words_chart(
        &self,
        stats: &UsageStatsResponse,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let points = shown_points(stats);
        let n = points.len().max(1);
        let compare = stats.bucket != UsageBucket::Week;
        let peak = points
            .iter()
            .flat_map(|p| [p.words.unwrap_or(0), p.previous_words.unwrap_or(0)])
            .max()
            .unwrap_or(0);
        let top = axis_top(peak);
        let x = |i: usize| (i as f64 + 0.5) / n as f64 * W;
        let y = |v: u64| H - v as f64 / top * H;
        let frac = |v: u64| (v as f64 / top) as f32;

        let current: Vec<(f64, f64)> = points
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.words.map(|w| (x(i), y(w))))
            .collect();
        let previous: Vec<(f64, f64)> = if compare {
            points
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.previous_words.map(|w| (x(i), y(w))))
                .collect()
        } else {
            Vec::new()
        };

        let title = tr!(bucket_key(stats.bucket));
        let (current_key, previous_key) = legend_keys(stats.period);
        let legend_previous = if compare {
            tr!(previous_key)
        } else {
            String::new()
        };
        let legend = compare.then(|| {
            div()
                .flex()
                .gap(px(14.))
                .text_size(px(11.5))
                .font_weight(FontWeight::NORMAL)
                .text_color(t.muted_foreground)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(legend_line(t.accent, false))
                        .child(tr!(current_key)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(legend_line(t.muted_foreground.opacity(0.7), true))
                        .child(legend_previous.clone()),
                )
                .into_any_element()
        });

        let mut chart = div().relative().ml(AXIS).mt(px(6.)).h(px(H as f32));
        for k in 0..4 {
            let at = k as f32 / 3.;
            chart = chart
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(relative(at))
                        .h(px(1.))
                        .bg(if k == 0 {
                            t.input
                        } else {
                            t.border.opacity(0.7)
                        }),
                )
                .child(
                    anchor(0., at).items_center().justify_end().child(
                        div()
                            .flex_none()
                            .mr(px(8.))
                            .text_size(px(10.5))
                            .line_height(px(14.))
                            .font_features(tabular())
                            .text_color(t.muted_foreground)
                            .child(format_count(top * k as f64 / 3.)),
                    ),
                );
        }

        let (fill, dashed, line) = (
            t.accent.opacity(0.12),
            t.muted_foreground.opacity(0.7),
            t.accent,
        );
        chart = chart.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let at = chart_space(bounds, W, H);
                    if current.len() > 1 {
                        let mut area = PathBuilder::fill();
                        add_curve(&mut area, &current, &at);
                        area.line_to(at((current[current.len() - 1].0, H)));
                        area.line_to(at((current[0].0, H)));
                        area.close();
                        if let Ok(path) = area.build() {
                            window.paint_path(path, fill);
                        }
                    }
                    if previous.len() > 1 {
                        let mut curve = PathBuilder::stroke(px(2.)).dash_array(&[px(5.), px(5.)]);
                        add_curve(&mut curve, &previous, &at);
                        if let Ok(path) = curve.build() {
                            window.paint_path(path, dashed);
                        }
                    }
                    if current.len() > 1 {
                        let mut curve = PathBuilder::stroke(px(2.5));
                        add_curve(&mut curve, &current, &at);
                        if let Ok(path) = curve.build() {
                            window.paint_path(path, line);
                        }
                    }
                },
            )
            .absolute()
            .inset_0()
            .size_full(),
        );

        let hovered = match self.hover {
            Some(Hover::Words(i)) if i < points.len() => Some(i),
            _ => None,
        };
        let dots = |i: usize, p: &UsagePoint| {
            let left = (i as f32 + 0.5) / n as f32;
            let mut dots = Vec::new();
            if let (true, Some(v)) = (compare, p.previous_words) {
                dots.push(dot_at(
                    left,
                    frac(v),
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .border_2()
                        .border_color(t.muted_foreground.opacity(0.7))
                        .bg(t.card),
                ));
            }
            if let Some(v) = p.words {
                dots.push(dot_at(
                    left,
                    frac(v),
                    div()
                        .size(px(9.))
                        .rounded_full()
                        .border_2()
                        .border_color(t.card)
                        .bg(t.accent),
                ));
            }
            dots
        };
        if points.len() <= MAX_DOTS {
            for (i, p) in points.iter().enumerate() {
                chart = chart.children(dots(i, p));
            }
        } else if let Some(i) = hovered {
            chart = chart.children(dots(i, &points[i]));
        }

        let mut columns = div().absolute().inset_0().flex();
        for i in 0..points.len() {
            columns = columns.child(
                div()
                    .id(("words-point", i))
                    .relative()
                    .flex_1()
                    .h_full()
                    .on_hover(cx.listener(move |page, hovered: &bool, _, cx| {
                        page.set_hover(Hover::Words(i), *hovered, cx)
                    }))
                    .when(hovered == Some(i), |c| {
                        c.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(relative(0.5))
                                .w(px(1.))
                                .bg(t.input),
                        )
                    }),
            );
        }
        chart = chart.child(columns);

        if let Some(i) = hovered {
            let p = &points[i];
            let align = tip_align(i, points.len(), 2);
            let left = match align {
                TipAlign::Start => i as f32 / n as f32,
                TipAlign::Center => (i as f32 + 0.5) / n as f32,
                TipAlign::End => (i as f32 + 1.) / n as f32,
            };
            let higher = p.words.unwrap_or(0).max(p.previous_words.unwrap_or(0));
            let change = match (compare, p.words, p.previous_words) {
                (true, Some(now), Some(before)) => percent_change(now, before),
                _ => None,
            };
            let tip = tip_box(cx)
                .font_features(tabular())
                .child(tip_title(point_title(
                    stats.bucket,
                    p,
                    i + 1 == points.len(),
                )))
                .when_some(p.words, |tip, v| {
                    tip.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .when(compare, |r| r.child(legend_line(t.accent, false)))
                            .child(words(Some(v))),
                    )
                })
                .when_some(
                    p.previous_words
                        .zip(p.previous_start.as_deref().and_then(parse_local))
                        .filter(|_| compare),
                    |tip, (v, date)| {
                        tip.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .text_color(t.muted_foreground)
                                .child(legend_line(t.muted_foreground.opacity(0.7), true))
                                .child(tr!(
                                    "insights-chart-on",
                                    value = words(Some(v)),
                                    date = format_short_date(date)
                                )),
                        )
                    },
                )
                .when_some(change.filter(|&c| c != 0), |tip, change| {
                    tip.child(
                        div()
                            .text_color(if change > 0 { t.success } else { t.destructive })
                            .child(tr!(
                                "insights-chart-change",
                                pct = format!(
                                    "{} {}",
                                    if change > 0 { "▲" } else { "▼" },
                                    format_change(change)
                                ),
                                period = legend_previous.to_lowercase()
                            )),
                    )
                });
            chart = chart.child(tip_above(left, frac(higher).min(1.), px(10.), align, tip));
        }

        let step = label_step(stats.bucket, points.len());
        let mut labels = div().ml(AXIS).flex();
        for (i, p) in points.iter().enumerate() {
            let shown = (points.len() - 1 - i).is_multiple_of(step);
            let (main, sub) = x_label(stats.bucket, p);
            labels = labels.child(div().relative().flex_1().h(px(28.)).when(shown, |cell| {
                cell.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(relative(0.5))
                        .w(px(0.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .items_center()
                                .whitespace_nowrap()
                                .text_size(px(10.5))
                                .line_height(px(14.))
                                .font_features(tabular())
                                .text_color(t.muted_foreground)
                                .child(main)
                                .children(sub.map(|sub| {
                                    div().text_color(t.muted_foreground.opacity(0.6)).child(sub)
                                })),
                        ),
                )
            }));
        }

        card(cx)
            .child(card_title(title, legend))
            .child(chart)
            .child(labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // gpui's glob export has its own `test`; this is the standard one.
    use core::prelude::v1::test;

    fn point(start: &str, words: Option<u64>) -> UsagePoint {
        UsagePoint {
            start: start.into(),
            words,
            previous_start: None,
            previous_words: None,
        }
    }

    #[test]
    fn labels_spread_out_as_points_grow() {
        assert_eq!(label_step(UsageBucket::Hour, 16), 3);
        assert_eq!(label_step(UsageBucket::Day, 7), 1);
        assert_eq!(label_step(UsageBucket::Day, 30), 4);
        assert_eq!(label_step(UsageBucket::Week, 0), 1);
    }

    #[test]
    fn today_shows_7a_to_10p_widened_to_any_dictation() {
        let mut series: Vec<UsagePoint> = (0..24)
            .map(|h| point(&format!("2026-09-28T{h:02}:00"), Some(0)))
            .collect();
        let stats = |series: Vec<UsagePoint>| UsageStatsResponse {
            bucket: UsageBucket::Hour,
            series,
            ..sample_stats()
        };
        let shown = stats(series.clone());
        let hours = shown_points(&shown);
        assert_eq!(hours.len(), 16);
        assert_eq!(hours[0].start, "2026-09-28T07:00");
        series[5].words = Some(40);
        series[23].words = Some(12);
        let shown = stats(series);
        let hours = shown_points(&shown);
        assert_eq!(hours.first().unwrap().start, "2026-09-28T05:00");
        assert_eq!(hours.last().unwrap().start, "2026-09-28T23:00");
    }

    #[test]
    fn titles_name_the_hour_day_or_week() {
        let p = point("2026-09-28T09:00", Some(5));
        assert_eq!(point_title(UsageBucket::Hour, &p, false), "9a–10a today");
        let p = point("2026-09-28", Some(5));
        assert_eq!(point_title(UsageBucket::Day, &p, false), "Mon, Sep 28");
        assert_eq!(
            point_title(UsageBucket::Day, &p, true),
            "Mon, Sep 28 · today"
        );
        assert_eq!(
            x_label(UsageBucket::Day, &p),
            ("M".to_string(), Some("Sep 28".to_string()))
        );
    }

    pub(crate) fn sample_stats() -> UsageStatsResponse {
        serde_json::from_value(serde_json::json!({
            "period": "7d",
            "bucket": "day",
            "start": "2026-09-22",
            "end": "2026-09-28",
            "previous_start": null,
            "previous_end": null,
            "typing_wpm": 40,
            "current": {
                "words": 0, "captures": 0, "speaking_ms": 0, "pace_wpm": null,
                "time_saved_ms": 0, "fixed_captures": 0, "weekdays_dictated": 0,
                "weekdays_in_period": 5, "weekend_days": [], "hours_dictated": 0
            },
            "previous": null,
            "all_apps": {},
            "series": [],
            "apps": [],
            "heatmap": [],
            "lengths": {"bin_edges_s": [], "counts": [], "p25_ms": null, "median_ms": null, "p75_ms": null}
        }))
        .expect("sample stats")
    }
}
