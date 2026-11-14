//! Toasts, in the component library's notification layer (open windows with
//! `gpui_kit::open_window` so it has a root): a title, an optional line under
//! it, and an optional action, bottom right.

use super::Handler;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::*;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    #[default]
    Default,
    Success,
    Warning,
    /// Something failed.
    Destructive,
}

#[derive(Clone)]
pub struct Toast {
    pub title: SharedString,
    pub description: Option<SharedString>,
    pub kind: ToastKind,
    /// A button on the toast; a toast with one stays until it's dismissed.
    pub action: Option<(SharedString, Handler)>,
}

impl Toast {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            kind: ToastKind::Default,
            action: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn kind(mut self, kind: ToastKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some((label.into(), Rc::new(on_click)));
        self
    }

    fn into_notification(self) -> Notification {
        let mut note = Notification::new().title(self.title);
        if let Some(description) = self.description {
            note = note.message(description);
        }
        note = match self.kind {
            ToastKind::Default => note,
            ToastKind::Success => note.with_type(NotificationType::Success),
            ToastKind::Warning => note.with_type(NotificationType::Warning),
            ToastKind::Destructive => note.with_type(NotificationType::Error),
        };
        if let Some((label, on_click)) = self.action {
            note = note.action(move |_, _, _| {
                let on_click = on_click.clone();
                Button::new("toast-action")
                    .outline()
                    .small()
                    .label(label.clone())
                    .on_click(move |_, window, cx| on_click(window, cx))
            });
        }
        note
    }
}

/// Shows `toast` in `window`.
pub fn show_toast(window: &mut Window, cx: &mut App, toast: Toast) {
    window.push_notification(toast.into_notification(), cx);
}
