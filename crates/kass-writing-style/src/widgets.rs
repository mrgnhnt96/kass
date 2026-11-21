//! Small pieces the Writing Style page and the teach dialog draw in several places.

use crate::model::initial;
use gpui_kit::assets::IconName;
use gpui_kit::component::Selectable;
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

/// An app's tile: its initial on a tinted square.
pub fn app_tile(name: &str, size: Pixels, text: Pixels, radius: Pixels, cx: &App) -> Div {
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

/// A row in a menu.
pub fn menu_item(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let t = theme(cx);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(30.))
        .px_2()
        .rounded(metrics::RADIUS_SM)
        .text_size(metrics::TEXT_BASE)
        .text_color(t.foreground)
        .cursor_pointer()
        .hover(|s| s.bg(t.secondary))
}

/// Styles a menu's button, given whether its menu is open.
type TriggerStyle = Box<dyn Fn(Stateful<Div>, bool, &App) -> Stateful<Div>>;

/// A menu's button: whatever it shows, and an open state the menu sets.
#[derive(IntoElement)]
pub struct MenuTrigger {
    id: ElementId,
    content: Option<AnyElement>,
    style: TriggerStyle,
    open: bool,
}

impl MenuTrigger {
    pub fn new(
        id: impl Into<ElementId>,
        content: impl IntoElement,
        style: impl Fn(Stateful<Div>, bool, &App) -> Stateful<Div> + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            content: Some(content.into_any_element()),
            style: Box::new(style),
            open: false,
        }
    }
}

impl Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for MenuTrigger {
    fn render(mut self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let base = div().id(self.id.clone()).cursor_pointer();
        (self.style)(base, self.open, cx).children(self.content.take())
    }
}

/// Cuts what's typed or pasted into `state` back to `max` characters.
pub fn limit_chars(state: &Entity<TextareaState>, max: usize, window: &mut Window, cx: &mut App) {
    let value = state.read(cx).value();
    if let Some(text) = crate::model::clipped(&value, max) {
        state.update(cx, |state, cx| state.set_value(text, window, cx));
    }
}
