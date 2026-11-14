//! The on/off switch: 36×20, mint when on.

use crate::theme::theme;
use gpui_kit::component::ThemeStyled as _;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::rc::Rc;
use std::time::{Duration, Instant};

type ChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

const TRACK_W: f32 = 36.;
const TRACK_H: f32 = 20.;
const THUMB: f32 = 16.;
const OFF_X: f32 = 2.;
const ON_X: f32 = 18.;
const SLIDE: Duration = Duration::from_millis(150);

#[derive(IntoElement)]
pub struct Toggle {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Toggle {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            disabled: false,
            on_change: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Called with the new value when the switch is clicked.
    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Toggle {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let (track, thumb) = if self.checked {
            (t.accent, t.accent_foreground)
        } else {
            (t.input, t.muted_foreground)
        };
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let focused = focus.is_focused(window);
        // When the value last changed, so the thumb slides rather than jumps.
        let changed =
            window.use_keyed_state(ElementId::from((self.id.clone(), "changed")), cx, |_, _| {
                (self.checked, None::<Instant>)
            });
        let (was, mut since) = *changed.read(cx);
        if was != self.checked {
            since = Some(Instant::now());
            changed.update(cx, |changed, _| *changed = (self.checked, since));
        }
        let progress = since.map_or(1., |since| {
            (since.elapsed().as_secs_f32() / SLIDE.as_secs_f32()).min(1.)
        });
        if progress < 1. {
            window.request_animation_frame();
        }
        let eased = 1. - (1. - progress).powi(3);
        let x = if self.checked {
            OFF_X + (ON_X - OFF_X) * eased
        } else {
            ON_X - (ON_X - OFF_X) * eased
        };
        let checked = self.checked;
        let interactive = !self.disabled;

        div()
            .id(self.id.clone())
            .track_focus(&focus)
            .relative()
            .flex_none()
            .w(px(TRACK_W))
            .h(px(TRACK_H))
            .rounded_full()
            .border_2()
            .border_color(transparent_black())
            .bg(track)
            .when(self.disabled, |s| s.opacity(0.5))
            .when(interactive, |s| {
                s.cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            })
            .when_some(self.on_change.filter(|_| interactive), |s, on_change| {
                s.on_click(move |_, window, cx| on_change(&!checked, window, cx))
            })
            .when(focused, |s| s.focus_ring_style(window, cx))
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(x - OFF_X))
                    .size(px(THUMB))
                    .rounded_full()
                    .bg(thumb)
                    .shadow_sm(),
            )
    }
}
