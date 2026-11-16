//! How long you talk: the share of dictations in each length bin as one
//! smooth curve, the middle half of them (quartile to quartile) shaded
//! darker, and a line at the median. Bins are evenly spaced, so a length
//! maps onto the x axis through the bin it falls in.

use crate::format::{TipAlign, bin_labels, format_seconds, length_fraction, tip_align};
use crate::page::{Hover, InsightsPage};
use crate::widgets::{
    add_curve, card, card_title, chart_space, hint, tabular, tip_above, tip_box, tip_title,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{UsageLengths, UsageStatsResponse};
use kass_i18n::tr;
use kass_ui::theme;

const W: f64 = 600.;
const H: f64 = 150.;
/// The curve's ends run down past the first and last bins, toward these
/// fractions of them.
const START_TAPER: f64 = 0.7;
const END_TAPER: f64 = 0.5;

/// The curve's points in chart units: a tapered start, each bin's middle,
/// a tapered end.
pub(crate) fn curve_points(counts: &[u64], top: f64) -> Vec<(f64, f64)> {
    let n = counts.len();
    let (Some(&first), Some(&last)) = (counts.first(), counts.last()) else {
        return Vec::new();
    };
    let y = |c: f64| H - c / top * H;
    let mut points = vec![(0., y(first as f64 * START_TAPER))];
    points.extend(
        counts
            .iter()
            .enumerate()
            .map(|(i, &c)| ((i as f64 + 0.5) / n as f64 * W, y(c as f64))),
    );
    points.push((W, y(last as f64 * END_TAPER)));
    points
}

/// The bin most dictations fall in, the first of any tie.
fn most(counts: &[u64]) -> usize {
    let max = counts.iter().copied().max().unwrap_or(0);
    counts.iter().position(|&c| c == max).unwrap_or(0)
}

impl InsightsPage {
    pub(crate) fn render_length_curve(
        &self,
        stats: &UsageStatsResponse,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let frame = card(cx).flex_1().child(card_title(
            tr!("insights-length-title"),
            Some(hint(tr!("insights-length-hint"), cx).into_any_element()),
        ));
        let UsageLengths {
            bin_edges_s: edges,
            counts,
            p25_ms,
            median_ms,
            p75_ms,
        } = &stats.lengths;
        let total: u64 = counts.iter().sum();
        let (Some(median_ms), true) = (*median_ms, total > 0) else {
            return frame.child(
                div()
                    .text_size(px(12.5))
                    .text_color(t.muted_foreground)
                    .child(tr!("insights-length-none")),
            );
        };
        let n = counts.len();
        let labels = bin_labels(edges);
        let top = counts.iter().copied().max().unwrap_or(0) as f64 * 1.15;
        let points = curve_points(counts, top);
        let median = length_fraction(median_ms, edges, n);
        let lo = length_fraction(p25_ms.unwrap_or(median_ms), edges, n);
        let hi = length_fraction(p75_ms.unwrap_or(median_ms), edges, n);

        let (fill, band, line) = (t.accent.opacity(0.2), t.accent.opacity(0.35), t.accent);
        let mut chart = div().relative().h(px(H as f32)).child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let at = chart_space(bounds, W, H);
                    let area = || {
                        let mut area = PathBuilder::fill();
                        add_curve(&mut area, &points, &at);
                        area.line_to(at((W, H)));
                        area.line_to(at((0., H)));
                        area.close();
                        area.build().ok()
                    };
                    if let Some(path) = area() {
                        window.paint_path(path, fill);
                    }
                    let left = at((lo * W, 0.)).x;
                    let width = (at((hi * W, 0.)).x - left).max(px(1.));
                    let mask = ContentMask {
                        bounds: Bounds::new(
                            point(left, bounds.origin.y),
                            size(width, bounds.size.height),
                        ),
                    };
                    window.with_content_mask(Some(mask), |window| {
                        if let Some(path) = area() {
                            window.paint_path(path, band);
                        }
                    });
                    let mut edge = PathBuilder::stroke(px(2.));
                    add_curve(&mut edge, &points, &at);
                    if let Ok(path) = edge.build() {
                        window.paint_path(path, line);
                    }
                },
            )
            .absolute()
            .inset_0()
            .size_full(),
        );

        let label = div()
            .absolute()
            .top(px(-2.))
            .whitespace_nowrap()
            .text_size(px(11.))
            .line_height(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(tr!(
                "insights-length-median",
                value = format_seconds(median_ms)
            ));
        // Past two thirds, the label sits left of the line so it stays in the card.
        let label = if median > 0.66 {
            label.right(px(7.))
        } else {
            label.left(px(7.))
        };
        chart = chart.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(median as f32))
                .w(px(1.5))
                .bg(t.foreground)
                .child(label),
        );

        let hovered = match self.hover {
            Some(Hover::Length(i)) if i < n => Some(i),
            _ => None,
        };
        let mut bins = div().absolute().inset_0().flex();
        for i in 0..n {
            bins = bins.child(
                div()
                    .id(("length-bin", i))
                    .flex_1()
                    .h_full()
                    .when(hovered == Some(i), |b| b.bg(t.foreground.opacity(0.03)))
                    .on_hover(cx.listener(move |page, hovered: &bool, _, cx| {
                        page.set_hover(Hover::Length(i), *hovered, cx)
                    })),
            );
        }
        chart = chart.child(bins);
        if let Some(i) = hovered {
            let count = counts[i];
            let align = tip_align(i, n, 1);
            let left = match align {
                TipAlign::Start => i as f32 / n as f32,
                TipAlign::Center => (i as f32 + 0.5) / n as f32,
                TipAlign::End => (i as f32 + 1.) / n as f32,
            };
            let pct = (count as f64 / total as f64 * 100.).round() as u64;
            chart = chart.child(tip_above(
                left,
                ((count as f64 / top) as f32).min(1.),
                px(10.),
                align,
                tip_box(cx)
                    .child(tip_title(tr!("insights-length-share", pct = pct)))
                    .child(div().text_color(t.muted_foreground).child(tr!(
                        "insights-length-count",
                        count = count,
                        bin = labels.get(i).cloned().unwrap_or_default()
                    ))),
            ));
        }

        let axis = div().flex().children(labels.iter().map(|label| {
            div()
                .flex_1()
                .min_w_0()
                .text_center()
                .text_size(px(10.5))
                .line_height(px(14.))
                .font_features(tabular())
                .text_color(t.muted_foreground)
                .child(label.clone())
        }));
        let summary = format!(
            "{} {}",
            tr!(
                "insights-length-most",
                bin = labels.get(most(counts)).cloned().unwrap_or_default()
            ),
            tr!("insights-length-band")
        );

        frame.child(chart).child(axis).child(
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
    use super::{H, W, curve_points, most};

    #[test]
    fn the_curve_tapers_past_the_end_bins() {
        let points = curve_points(&[10, 20, 4], 20.);
        assert_eq!(points.len(), 5);
        assert_eq!(points[0], (0., H - 0.7 * 10. / 20. * H));
        assert_eq!(points[2], (300., 0.));
        assert_eq!(points[4], (W, H - 0.5 * 4. / 20. * H));
        assert!(curve_points(&[], 1.).is_empty());
    }

    #[test]
    fn most_dictations_fall_in_the_first_highest_bin() {
        assert_eq!(most(&[3, 9, 9, 1]), 1);
        assert_eq!(most(&[]), 0);
    }
}
