//! Small pieces the Writing Style page and the teach dialog draw in several places.

use crate::model::initial;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::TextareaState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{metrics, theme};
use std::time::Duration;

pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// A turning loader.
pub fn spinner(id: impl Into<ElementId>, size: Pixels, color: Hsla) -> impl IntoElement {
    icon(IconName::LoaderCircle, size, color).with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
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

/// An app's tile: its icon, or its initial on a tinted square when it isn't
/// installed.
pub fn app_tile(
    bundle_id: &str,
    name: &str,
    size: Pixels,
    text: Pixels,
    radius: Pixels,
    cx: &App,
) -> AnyElement {
    if let Some(image) = kass_ui::app_icon(bundle_id, cx) {
        return img(image).size(size).flex_none().into_any_element();
    }
    let t = theme(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(radius)
        .bg(t.secondary)
        .border_1()
        .border_color(t.border)
        .text_size(text)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.foreground)
        .child(initial(name))
        .into_any_element()
}

/// "Default", beside the default style's name.
pub fn default_badge(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex_none()
        .px_2()
        .rounded_full()
        .border_1()
        .border_color(t.input)
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.muted_foreground)
        .child(kass_i18n::t("writing-style-styles-default"))
}

/// A rounded chip with an accent rim, like a learned habit.
pub fn pill(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .px(px(10.))
        .py(px(4.))
        .rounded_full()
        .border_1()
        .border_color(t.accent.opacity(0.2))
        .text_size(metrics::TEXT_XS)
        .text_color(t.foreground.opacity(0.85))
        .child(text.into())
}

/// A key shown as a keycap, like ⌘⏎.
pub fn kbd(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.input)
        .bg(t.secondary)
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_2XS)
        .text_color(t.foreground)
        .child(text.into())
}

pub use kass_ui::{MenuTrigger, menu_item};

/// Cuts what's typed or pasted into `state` back to `max` characters.
pub fn limit_chars(state: &Entity<TextareaState>, max: usize, window: &mut Window, cx: &mut App) {
    let value = state.read(cx).value();
    if let Some(text) = crate::model::clipped(&value, max) {
        state.update(cx, |state, cx| state.set_value(text, window, cx));
    }
}
