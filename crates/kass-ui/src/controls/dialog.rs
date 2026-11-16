//! Dialogs, in the component library's dialog layer (open windows with
//! `gpui_kit::open_window` so it has a root), laid out like the React
//! `Dialog`: 512px wide, title, description, and buttons on the right.

use super::Handler;
use super::button::Button;
use crate::theme::{metrics, theme};
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::*;

/// A question that needs a yes or no before something happens.
#[derive(Clone)]
pub struct Confirm {
    pub title: SharedString,
    pub description: Option<SharedString>,
    pub confirm: SharedString,
    pub cancel: SharedString,
    /// The confirm button is red: the action deletes or discards.
    pub destructive: bool,
    pub on_confirm: Handler,
}

/// Title and description, the top of every Kass dialog.
pub fn dialog_header(
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(metrics::TEXT_LG)
                .line_height(metrics::LINE_LG)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.popover_foreground)
                .child(title.into()),
        )
        .when_some(description, |s, description| {
            s.child(
                div()
                    .text_size(metrics::TEXT_SM)
                    .line_height(metrics::LINE_SM)
                    .text_color(t.muted_foreground)
                    .child(description),
            )
        })
}

/// Buttons along the bottom, right-aligned.
pub fn dialog_footer() -> Div {
    div().flex().justify_end().gap_2()
}

/// Opens a Kass-sized dialog; `build` fills in its body.
pub fn open_dialog(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        dialog
            .w(px(512.))
            .close_button(true)
            .child(build(window, cx))
    });
}

/// Asks `confirm.title`; the confirm button runs `on_confirm` and closes it.
pub fn open_confirm(window: &mut Window, cx: &mut App, confirm: Confirm) {
    open_dialog(window, cx, move |_, cx| {
        let on_confirm = confirm.on_confirm.clone();
        let mut ok = Button::new("dialog-confirm").label(confirm.confirm.clone());
        if confirm.destructive {
            ok = ok.destructive();
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(dialog_header(
                confirm.title.clone(),
                confirm.description.clone(),
                cx,
            ))
            .child(
                dialog_footer()
                    .child(
                        Button::new("dialog-cancel")
                            .outline()
                            .label(confirm.cancel.clone())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(ok.on_click(move |_, window, cx| {
                        on_confirm(window, cx);
                        window.close_dialog(cx);
                    })),
            )
            .into_any_element()
    });
}

/// Closes the dialog on top.
pub fn close_dialog(window: &mut Window, cx: &mut App) {
    window.close_dialog(cx);
}
