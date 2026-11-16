//! Small pieces the Dictionary draws in several places.

use crate::keys::{key_label, key_side};
use crate::scopes::Scope;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_i18n::parse_markup;
use kass_ui::{metrics, theme};

pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// The arrow between what's said and what's written.
pub fn arrow(amber: bool, cx: &App) -> Svg {
    let t = theme(cx);
    let color = if amber {
        t.accent
    } else {
        t.muted_foreground.opacity(0.6)
    };
    icon(IconName::ArrowRight, px(14.), color)
}

/// A small uppercase monospace heading.
pub fn mono_label(text: impl AsRef<str>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_2XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
        .child(text.as_ref().to_uppercase())
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

/// A scope's mark: a globe for everywhere, an outlined initial for a style,
/// a filled initial for an app.
pub fn scope_icon(scope: &Scope, name: &str, size: Pixels, cx: &App) -> AnyElement {
    let t = theme(cx);
    match scope {
        Scope::Global => icon(IconName::Globe, size, t.muted_foreground).into_any_element(),
        Scope::Style(_) => div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(size)
            .rounded(px(3.))
            .border_1()
            .border_color(t.muted_foreground.opacity(0.6))
            .font_family(t.mono_font.clone())
            .text_size(px(8.))
            .line_height(px(8.))
            .text_color(t.muted_foreground)
            .child(initial(name))
            .into_any_element(),
        Scope::App(_) => div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(size)
            .rounded(size * 0.22)
            .bg(t.muted_foreground.opacity(0.7))
            .font_family(t.mono_font.clone())
            .text_size(if size > px(20.) { px(13.) } else { px(8.) })
            .line_height(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.background)
            .child(initial(name))
            .into_any_element(),
    }
}

/// The dictation chord as keycaps.
pub fn chord_keys(keys: &[String], cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(keys.iter().map(|code| {
            div()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .h(px(24.))
                .min_w(px(28.))
                .px_2()
                .rounded(px(5.))
                .border_1()
                .border_b_2()
                .border_color(t.input)
                .bg(t.secondary)
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_XS)
                .text_color(t.foreground)
                .child(key_label(code))
                .when_some(key_side(code), |cap, side| {
                    cap.child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .right(px(2.))
                            .text_size(px(8.))
                            .line_height(px(8.))
                            .text_color(t.accent)
                            .child(side),
                    )
                })
        }))
}

/// A line whose `<keys/>` is the chord drawn as keycaps.
pub fn with_keys(text: &str, keys: &[String], cx: &App) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .children(parse_markup(text, &["keys"]).into_iter().map(|run| {
            if run.tag.is_some() {
                div().mx_1().child(chord_keys(keys, cx)).into_any_element()
            } else {
                div().child(run.text).into_any_element()
            }
        }))
}
