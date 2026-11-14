//! Text fields, in the component library's editor, sized and colored like
//! the React `Input`: 36px tall, white (card) fill, 13px text.

use crate::theme::{metrics, theme};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;

/// A one-line text field over `state` (`cx.new(|cx| InputState::new(window, cx))`).
pub fn text_input(state: &Entity<InputState>, cx: &App) -> Input {
    let t = theme(cx);
    Input::new(state)
        .h(px(36.))
        .px_3()
        .rounded(metrics::RADIUS_MD)
        .bg(t.card)
        .text_size(metrics::TEXT_BASE)
}
