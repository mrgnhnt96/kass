//! Small pieces the Insights page draws in several places.

use crate::format::TipAlign;
use crate::host::InsightsHost;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{metrics, theme};
use std::sync::Arc;

/// Every app tile's size.
pub const TILE: Pixels = px(24.);

pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// Figures that line up: tabular digits.
pub fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// A small uppercase monospace heading.
pub fn mono_label(text: impl AsRef<str>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
        .line_height(metrics::LINE_XS)
        .text_color(t.muted_foreground)
        .child(text.as_ref().to_uppercase())
}

/// An app's icon, or its initial on a muted tile when it isn't installed.
pub fn app_tile(host: &dyn InsightsHost, bundle_id: &str, name: &str, cx: &App) -> AnyElement {
    let t = theme(cx);
    if let Some(image) = host.app_icon(bundle_id) {
        return img(image).size(TILE).flex_none().into_any_element();
    }
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(TILE)
        .rounded(metrics::RADIUS_MD)
        .bg(t.muted)
        .text_size(metrics::TEXT_CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.muted_foreground)
        .child(
            name.chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default(),
        )
        .into_any_element()
}

/// A symbol in an app-sized tile, for "All apps" and "Unknown app".
pub fn symbol_tile(name: IconName, cx: &App) -> AnyElement {
    let t = theme(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(TILE)
        .rounded(metrics::RADIUS_MD)
        .bg(t.muted)
        .child(icon(name, px(14.), t.muted_foreground))
        .into_any_element()
}

/// One of the page's cards: a bordered panel with a title row.
pub fn card(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(14.))
        .rounded(metrics::RADIUS_LG)
        .border_1()
        .border_color(t.border)
        .bg(t.card)
        .px(px(18.))
        .py(px(16.))
}

/// A card's title, with what goes on its right (a legend or a hint).
pub fn card_title(title: impl Into<SharedString>, aside: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .text_size(metrics::TEXT_BASE)
        .line_height(metrics::LINE_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .child(title.into())
        .children(aside)
}

/// A muted hint beside a card's title, or under a chart.
pub fn hint(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(px(11.5))
        .line_height(px(16.))
        .font_weight(FontWeight::NORMAL)
        .text_color(theme(cx).muted_foreground)
        .child(text.into())
}

/// A hover tip's box: a small popover card.
pub fn tip_box(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex_none()
        .flex()
        .flex_col()
        .whitespace_nowrap()
        .rounded(metrics::RADIUS_MD)
        .border_1()
        .border_color(t.border)
        .bg(t.popover)
        .text_color(t.popover_foreground)
        .px(px(10.))
        .py(px(6.))
        .text_size(px(11.5))
        .line_height(px(16.))
        .font_weight(FontWeight::NORMAL)
        .shadow_md()
}

/// A tip's first line, in bold.
pub fn tip_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.5))
        .font_weight(FontWeight::BOLD)
        .font_features(tabular())
        .child(text.into())
}

/// A zero-sized point inside a positioned box, `left` and `bottom` as
/// fractions of it. What's put in it is aligned on the point by
/// `justify`/`items`, and may spill out of it on any side.
pub fn anchor(left: f32, bottom: f32) -> Div {
    div()
        .absolute()
        .left(relative(left))
        .bottom(relative(bottom))
        .w(px(0.))
        .h(px(0.))
        .flex()
}

/// `tip` above the point (`left`, `bottom`) of a positioned box, `gap` over
/// it, aligned as `align` says. It draws above everything else and never
/// takes the pointer.
pub fn tip_above(left: f32, bottom: f32, gap: Pixels, align: TipAlign, tip: Div) -> AnyElement {
    let point = anchor(left, bottom).items_end();
    let point = match align {
        TipAlign::Start => point.justify_start(),
        TipAlign::Center => point.justify_center(),
        TipAlign::End => point.justify_end(),
    };
    deferred(point.child(tip.mb(gap)))
        .with_priority(1)
        .into_any_element()
}

/// `tip` under the bottom-left corner of a positioned box, `gap` below it.
pub fn tip_below(gap: Pixels, tip: Div) -> AnyElement {
    deferred(
        div()
            .absolute()
            .left_0()
            .top(relative(1.))
            .w(px(0.))
            .h(px(0.))
            .flex()
            .items_start()
            .justify_start()
            .child(tip.mt(gap)),
    )
    .with_priority(1)
    .into_any_element()
}

/// A dot centred on (`left`, `bottom`) of a positioned box.
pub fn dot_at(left: f32, bottom: f32, dot: Div) -> Div {
    anchor(left, bottom)
        .items_center()
        .justify_center()
        .child(dot.flex_none())
}

/// A legend's line: solid, or dashed like the period before.
pub fn legend_line(color: Hsla, dashed: bool) -> Div {
    let line = div().flex_none().w(px(16.)).h(px(2.)).flex().gap(px(2.));
    if dashed {
        line.children((0..3).map(move |_| div().w(px(4.)).h_full().bg(color)))
    } else {
        line.bg(color)
    }
}

/// Adds a smooth curve through `points` (chart units) to `path`, starting
/// with a move to the first point. `at` maps chart units to the window.
pub fn add_curve(
    path: &mut PathBuilder,
    points: &[(f64, f64)],
    at: &impl Fn((f64, f64)) -> Point<Pixels>,
) {
    let Some(&first) = points.first() else {
        return;
    };
    path.move_to(at(first));
    for segment in crate::format::smooth_segments(points) {
        path.cubic_bezier_to(at(segment.to), at(segment.ctrl_a), at(segment.ctrl_b));
    }
}

/// Maps chart units (`width` × `height`, y down) onto `bounds`.
pub fn chart_space(
    bounds: Bounds<Pixels>,
    width: f64,
    height: f64,
) -> impl Fn((f64, f64)) -> Point<Pixels> {
    move |(x, y)| {
        point(
            bounds.origin.x + bounds.size.width * (x / width) as f32,
            bounds.origin.y + bounds.size.height * (y / height) as f32,
        )
    }
}
