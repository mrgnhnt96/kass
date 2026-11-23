//! Menus opened from a button: the button, which shows whether its menu is
//! open, and the rows inside.

use crate::theme::{metrics, theme};
use gpui_kit::component::Selectable;
use gpui_kit::prelude::*;
use gpui_kit::*;

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
