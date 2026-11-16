//! Small pieces the Captures page draws in several places.

use crate::host::CapturesHost;
use crate::word_diff::DiffHunk;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_i18n::tr;
use kass_ui::{metrics, theme};

pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// A small uppercase monospace heading.
pub fn mono_label(text: impl AsRef<str>, color: Hsla, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
        .line_height(metrics::LINE_XS)
        .text_color(color)
        .child(text.as_ref().to_uppercase())
}

/// A coloured dot before a tag; hollow for a tag that marks something missing.
pub fn dot(color: Hsla, size: Pixels, hollow: bool) -> Div {
    let dot = div().size(size).flex_none().rounded_full();
    if hollow {
        dot.border_1().border_color(color)
    } else {
        dot.bg(color)
    }
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

/// An app's icon, or its initial on a muted tile when the app isn't installed.
pub fn app_tile(
    host: &dyn CapturesHost,
    bundle_id: &str,
    name: &str,
    size: Pixels,
    cx: &App,
) -> AnyElement {
    let t = theme(cx);
    if let Some(image) = host.app_icon(bundle_id) {
        return img(image).size(size).flex_none().into_any_element();
    }
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(size * 0.25)
        .bg(t.muted)
        .text_size(if size > px(24.) {
            px(13.)
        } else {
            metrics::TEXT_CAPTION
        })
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.muted_foreground)
        .child(initial(name))
        .into_any_element()
}

/// A symbol on a muted tile, for "All apps" and "Unknown app".
pub fn symbol_tile(name: IconName, size: Pixels, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(metrics::RADIUS_MD)
        .bg(t.muted)
        .child(icon(name, size * 0.55, t.muted_foreground))
}

/// A square icon button, 32px unless resized.
pub fn icon_button(id: impl Into<ElementId>, name: IconName, cx: &App) -> Stateful<Div> {
    let t = theme(cx);
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded(metrics::RADIUS_MD)
        .cursor_pointer()
        .text_color(t.muted_foreground)
        .hover(|s| s.bg(t.muted).text_color(t.foreground))
        .child(
            svg()
                .path(name.path())
                .size(px(16.))
                .flex_none()
                .text_color(t.muted_foreground),
        )
}

/// A disclosure row: a chevron that turns when open, and a title.
pub fn disclosure(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    open: bool,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let chevron = icon(IconName::ChevronRight, px(14.), t.muted_foreground).with_transformation(
        Transformation::rotate(if open {
            percentage(0.25)
        } else {
            percentage(0.)
        }),
    );
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .cursor_pointer()
        .text_size(metrics::TEXT_BASE)
        .text_color(t.foreground)
        .child(chevron)
        .child(title.into())
}

/// The keys of a shortcut, as small key caps.
pub fn kbd(label: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(18.))
        .min_w(px(18.))
        .px(px(4.))
        .rounded(metrics::RADIUS_SM)
        .border_1()
        .border_color(t.border)
        .bg(t.muted)
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_2XS)
        .text_color(t.muted_foreground)
        .child(label.into())
}

/// Shown at most; the rest are counted.
pub const MAX_HUNKS_SHOWN: usize = 3;

/// "post grass → Postgres", with whatever `action` adds after each change.
pub fn hunk_list(
    hunks: &[DiffHunk],
    cx: &App,
    mut action: impl FnMut(usize, &DiffHunk) -> Option<AnyElement>,
) -> Div {
    let t = theme(cx);
    let mut list = div().flex().flex_col().gap(px(2.));
    for (i, hunk) in hunks.iter().take(MAX_HUNKS_SHOWN).enumerate() {
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .font_family(t.mono_font.clone())
            .text_size(metrics::TEXT_XS)
            .line_height(px(19.5));
        if !hunk.removed.is_empty() {
            row = row.child(
                div()
                    .text_color(t.destructive)
                    .line_through()
                    .child(hunk.removed.clone()),
            );
        }
        if !hunk.removed.is_empty() && !hunk.added.is_empty() {
            row = row.child(div().text_color(t.muted_foreground).child("\u{a0}→\u{a0}"));
        }
        if !hunk.added.is_empty() {
            row = row.child(div().text_color(t.success).child(hunk.added.clone()));
        }
        if let Some(count) = hunk.count {
            row = row.child(
                div()
                    .text_color(t.muted_foreground)
                    .child(format!("\u{a0}×{count}")),
            );
        }
        if let Some(extra) = action(i, hunk) {
            row = row.child(extra);
        }
        list = list.child(row);
    }
    if hunks.len() > MAX_HUNKS_SHOWN {
        list = list.child(
            div()
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_XS)
                .text_color(t.muted_foreground)
                .child(tr!(
                    "captures-teach-more-changes",
                    count = hunks.len() - MAX_HUNKS_SHOWN
                )),
        );
    }
    list
}

/// A small text action beside a change.
pub fn hunk_action(
    id: impl Into<ElementId>,
    name: IconName,
    label: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    div()
        .id(id)
        .ml(px(8.))
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(24.))
        .px(px(6.))
        .rounded(metrics::RADIUS_MD)
        .cursor_pointer()
        .font_family(".SystemUIFont")
        .text_size(metrics::TEXT_XS)
        .text_color(t.muted_foreground)
        .hover(|s| s.bg(t.muted).text_color(t.foreground))
        .child(icon(name, px(14.), t.muted_foreground))
        .child(label.into())
}
