//! Numbers, dates and chart geometry for Insights, as the React screen
//! formatted them (app/src/components/InsightsTab/usageFormat.ts).

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// A local day or hour from the stats ("2026-09-28", "2026-09-28T14:00").
/// The server has already turned them into the Mac's time, so they're read
/// as local, never as UTC.
pub fn parse_local(value: &str) -> Option<NaiveDateTime> {
    let (day, time) = match value.split_once('T') {
        Some((day, time)) => (day, Some(time)),
        None => (value, None),
    };
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let hour = time
        .and_then(|t| t.get(..2))
        .and_then(|h| h.parse::<u32>().ok())
        .unwrap_or(0);
    Some(date.and_time(NaiveTime::from_hms_opt(hour.min(23), 0, 0)?))
}

/// "12,345".
pub fn format_count(n: f64) -> String {
    let n = n.round().max(0.) as u64;
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Time saved: "2h 21m", "47m", "0m".
pub fn format_saved(ms: i64) -> String {
    let minutes = (ms.unsigned_abs() as f64 / 60_000.).round() as u64;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h {}m", minutes / 60, minutes % 60)
    }
}

/// "Mon, Sep 28".
pub fn format_day(date: NaiveDateTime) -> String {
    date.format("%a, %b %-d").to_string()
}

/// "Sep 28".
pub fn format_short_date(date: NaiveDateTime) -> String {
    date.format("%b %-d").to_string()
}

/// "Mon".
pub fn format_weekday(date: NaiveDateTime) -> String {
    date.format("%a").to_string()
}

/// A compact hour for axes: "7a", "12p", "10p".
pub fn hour_label(hour: i64) -> String {
    let h = hour.rem_euclid(24);
    match h {
        0 => "12a".into(),
        12 => "12p".into(),
        h if h < 12 => format!("{h}a"),
        h => format!("{}p", h - 12),
    }
}

/// The hour of a local time.
pub fn hour_of(date: NaiveDateTime) -> i64 {
    i64::from(date.hour())
}

/// Saturday or Sunday.
pub fn is_weekend(date: NaiveDate) -> bool {
    matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
}

/// Percent change from `previous` to `current`, or None when there's nothing
/// to compare with.
pub fn percent_change(current: u64, previous: u64) -> Option<i64> {
    if previous == 0 {
        return None;
    }
    Some(((current as f64 - previous as f64) / previous as f64 * 100.).round() as i64)
}

/// A percent change as people read it: "18%", or a multiple once it's at
/// least tripled ("16×"), where a percentage like 1550% stops meaning much.
/// Unsigned; callers add the arrow.
pub fn format_change(pct: i64) -> String {
    if pct >= 200 {
        format!("{}×", ((pct + 100) as f64 / 100.).round() as i64)
    } else {
        format!("{}%", pct.abs())
    }
}

/// "34%", "<1%" for a share too small to round up to 1.
pub fn format_share(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".into();
    }
    let pct = part as f64 / whole as f64 * 100.;
    if pct > 0. && pct < 1. {
        return "<1%".into();
    }
    format!("{}%", pct.round() as i64)
}

/// The share of captures that needed a fix: "12%", "3.4%", "0%".
pub fn fix_rate(fixed: u64, captures: u64) -> String {
    if captures == 0 {
        return "—".into();
    }
    let pct = fixed as f64 / captures as f64 * 100.;
    if pct == 0. || pct >= 10. {
        format!("{}%", pct.round() as i64)
    } else {
        format!("{pct:.1}%")
    }
}

/// "3.4 s", "12 s", "1:05".
pub fn format_seconds(ms: u64) -> String {
    let s = ms as f64 / 1000.;
    if s >= 60. {
        let minutes = (s / 60.).floor() as u64;
        let seconds = (s % 60.).round() as u64;
        return format!("{minutes}:{seconds:02}");
    }
    if s < 10. {
        format!("{s:.1} s")
    } else {
        format!("{} s", s.round() as u64)
    }
}

/// The length bins' labels: "<2s", "2–5s", …, "30s+".
pub fn bin_labels(edges: &[u64]) -> Vec<String> {
    let Some((first, rest)) = edges.split_first() else {
        return Vec::new();
    };
    let mut labels = vec![format!("<{first}s")];
    let mut low = *first;
    for &edge in rest {
        labels.push(format!("{low}–{edge}s"));
        low = edge;
    }
    labels.push(format!("{low}s+"));
    labels
}

/// Where the open-ended last length bin is drawn to end.
const LAST_BIN_END_S: f64 = 60.;

/// A length's place on the length curve's x axis, 0 to 1: its bin, then how
/// far through it. Bins are evenly spaced.
pub fn length_fraction(ms: u64, edges: &[u64], bins: usize) -> f64 {
    if bins == 0 {
        return 0.;
    }
    let bounds: Vec<f64> = std::iter::once(0.)
        .chain(edges.iter().map(|&e| e as f64))
        .chain(std::iter::once(LAST_BIN_END_S))
        .collect();
    let s = ms as f64 / 1000.;
    let mut bin = 0;
    while bin < bins - 1 && bin + 1 < bounds.len() && s >= bounds[bin + 1] {
        bin += 1;
    }
    let (low, high) = (
        bounds[bin.min(bounds.len() - 1)],
        bounds[(bin + 1).min(bounds.len() - 1)],
    );
    let within = if high > low {
        ((s - low) / (high - low)).min(1.)
    } else {
        0.
    };
    (bin as f64 + within) / bins as f64
}

const STEPS: [f64; 10] = [1., 1.5, 2., 2.5, 3., 4., 5., 6., 8., 10.];

/// The words chart's top: three even, round steps above the highest point,
/// with headroom.
pub fn axis_top(peak: u64) -> f64 {
    let raw = (peak as f64 * 1.1 / 3.).max(5.);
    let p = 10f64.powf(raw.log10().floor());
    let step = STEPS
        .iter()
        .find(|&&m| m * p >= raw)
        .copied()
        .unwrap_or(10.)
        * p;
    step * 3.
}

/// Where a tip sits over a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipAlign {
    /// Its left edge on the column.
    Start,
    Center,
    /// Its right edge on the column.
    End,
}

/// Where a tip sits over column `index` of `count`: flush with the chart at
/// the two ends.
pub fn tip_align(index: usize, count: usize, edge: usize) -> TipAlign {
    if count <= edge * 2 {
        return if (index as f64) < count as f64 / 2. {
            TipAlign::Start
        } else {
            TipAlign::End
        };
    }
    if index < edge {
        TipAlign::Start
    } else if index >= count - edge {
        TipAlign::End
    } else {
        TipAlign::Center
    }
}

/// One cubic Bézier segment: two control points and where it ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub ctrl_a: (f64, f64),
    pub ctrl_b: (f64, f64),
    pub to: (f64, f64),
}

/// A smooth curve through points, as monotone cubic Béziers
/// (Fritsch–Carlson): the curve never swings above a peak or below a zero
/// between two points, so it never shows words that weren't there. Points
/// go left to right; the curve starts at the first.
pub fn smooth_segments(points: &[(f64, f64)]) -> Vec<Segment> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let slopes: Vec<f64> = points
        .windows(2)
        .map(|w| {
            let dx = w[1].0 - w[0].0;
            (w[1].1 - w[0].1) / if dx == 0. { 1. } else { dx }
        })
        .collect();
    // Each point's tangent: flat at a turn, else its neighbours' mean slope,
    // limited to what keeps the curve between them (as d3's curveMonotoneX).
    let tangents: Vec<f64> = (0..n)
        .map(|i| {
            if i == 0 {
                return slopes[0];
            }
            if i == n - 1 {
                return slopes[n - 2];
            }
            let (a, b) = (slopes[i - 1], slopes[i]);
            let mean = (a + b) / 2.;
            (sign(a) + sign(b)) * a.abs().min(b.abs()).min(0.5 * mean.abs())
        })
        .collect();
    (0..n - 1)
        .map(|i| {
            let (x0, y0) = points[i];
            let (x1, y1) = points[i + 1];
            let dx = (x1 - x0) / 3.;
            Segment {
                ctrl_a: (x0 + dx, y0 + tangents[i] * dx),
                ctrl_b: (x1 - dx, y1 - tangents[i + 1] * dx),
                to: (x1, y1),
            }
        })
        .collect()
}

/// JavaScript's Math.sign.
fn sign(v: f64) -> f64 {
    if v > 0. {
        1.
    } else if v < 0. {
        -1.
    } else {
        0.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_days_and_hours() {
        let day = parse_local("2026-09-28").unwrap();
        assert_eq!(format_day(day), "Mon, Sep 28");
        assert_eq!(format_short_date(day), "Sep 28");
        assert_eq!(format_weekday(day), "Mon");
        assert_eq!(hour_of(parse_local("2026-09-28T14:00").unwrap()), 14);
        assert!(parse_local("soon").is_none());
    }

    #[test]
    fn counts_have_thousands_separators() {
        assert_eq!(format_count(0.), "0");
        assert_eq!(format_count(999.), "999");
        assert_eq!(format_count(1000.), "1,000");
        assert_eq!(format_count(1234567.4), "1,234,567");
    }

    #[test]
    fn time_saved_reads_in_hours_and_minutes() {
        assert_eq!(format_saved(0), "0m");
        assert_eq!(format_saved(47 * 60_000), "47m");
        assert_eq!(format_saved(-47 * 60_000), "47m");
        assert_eq!(format_saved((2 * 60 + 21) * 60_000), "2h 21m");
    }

    #[test]
    fn hours_label_compactly() {
        assert_eq!(hour_label(0), "12a");
        assert_eq!(hour_label(7), "7a");
        assert_eq!(hour_label(12), "12p");
        assert_eq!(hour_label(22), "10p");
        assert_eq!(hour_label(24), "12a");
    }

    #[test]
    fn changes_turn_into_multiples_once_tripled() {
        assert_eq!(percent_change(118, 100), Some(18));
        assert_eq!(percent_change(5, 0), None);
        assert_eq!(format_change(18), "18%");
        assert_eq!(format_change(-40), "40%");
        assert_eq!(format_change(1550), "17×");
    }

    #[test]
    fn shares_and_fix_rates() {
        assert_eq!(format_share(34, 100), "34%");
        assert_eq!(format_share(1, 1000), "<1%");
        assert_eq!(format_share(0, 0), "—");
        assert_eq!(fix_rate(0, 10), "0%");
        assert_eq!(fix_rate(1, 30), "3.3%");
        assert_eq!(fix_rate(3, 10), "30%");
        assert_eq!(fix_rate(0, 0), "—");
    }

    #[test]
    fn lengths_label_and_place() {
        assert_eq!(format_seconds(3400), "3.4 s");
        assert_eq!(format_seconds(12_300), "12 s");
        assert_eq!(format_seconds(65_000), "1:05");
        let edges = [2, 5, 10, 20, 30];
        assert_eq!(
            bin_labels(&edges),
            ["<2s", "2–5s", "5–10s", "10–20s", "20–30s", "30s+"]
        );
        // Bins are even: 6 of them, so each is a sixth of the axis.
        assert_eq!(length_fraction(0, &edges, 6), 0.);
        assert!((length_fraction(1000, &edges, 6) - 0.5 / 6.).abs() < 1e-9);
        assert!((length_fraction(7500, &edges, 6) - 2.5 / 6.).abs() < 1e-9);
        assert_eq!(length_fraction(600_000, &edges, 6), 1.);
    }

    #[test]
    fn the_axis_top_is_three_round_steps() {
        assert_eq!(axis_top(0), 15.);
        assert_eq!(axis_top(100), 120.);
        assert_eq!(axis_top(1234), 1500.);
    }

    #[test]
    fn tips_stay_inside_the_chart() {
        assert_eq!(tip_align(0, 30, 2), TipAlign::Start);
        assert_eq!(tip_align(1, 30, 2), TipAlign::Start);
        assert_eq!(tip_align(15, 30, 2), TipAlign::Center);
        assert_eq!(tip_align(29, 30, 2), TipAlign::End);
        assert_eq!(tip_align(1, 4, 2), TipAlign::Start);
        assert_eq!(tip_align(2, 4, 2), TipAlign::End);
    }

    #[test]
    fn the_curve_never_overshoots() {
        let points = [(0., 10.), (1., 0.), (2., 10.), (3., 10.)];
        let segments = smooth_segments(&points);
        assert_eq!(segments.len(), 3);
        // A turn at the bottom is flat: no control point dips below it.
        assert_eq!(segments[0].ctrl_b.1, 0.);
        assert_eq!(segments[1].ctrl_a.1, 0.);
        // A flat run stays flat.
        assert_eq!(segments[2].ctrl_a.1, 10.);
        assert_eq!(segments[2].to, (3., 10.));
        assert!(smooth_segments(&points[..1]).is_empty());
    }
}
