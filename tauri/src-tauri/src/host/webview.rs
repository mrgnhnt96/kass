//! The app's GPUI windows, by label.
//!
//! The windows live on the main thread, in `WINDOWS`. A `WebviewWindow`
//! (named for the Tauri type it replaced) is only a label, so it can go to
//! any thread: what it does is posted to the main queue, in order.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use gpui_kit::{
    div, AnyView, AnyWindowHandle, App, AppContext as _, Context, IntoElement, ParentElement,
    Render, Styled, Window, WindowOptions,
};
use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{main_async, main_sync, with_app, Error, Result};

struct Entry {
    window: Retained<NSWindow>,
    handle: AnyWindowHandle,
}

thread_local! {
    static WINDOWS: RefCell<HashMap<String, Entry>> = RefCell::new(HashMap::new());
}

/// The open windows' labels, readable from any thread.
fn labels() -> &'static Mutex<HashSet<String>> {
    static LABELS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LABELS.get_or_init(Default::default)
}

pub(crate) fn get(label: &str) -> Option<WebviewWindow> {
    labels()
        .lock()
        .unwrap()
        .contains(label)
        .then(|| WebviewWindow {
            label: label.to_string(),
        })
}

pub(crate) fn all() -> Vec<WebviewWindow> {
    labels()
        .lock()
        .unwrap()
        .iter()
        .map(|label| WebviewWindow {
            label: label.clone(),
        })
        .collect()
}

/// Run `f` with the window's NSWindow, if it's open. Main thread only.
pub(crate) fn with_window<R>(label: &str, f: impl FnOnce(&NSWindow) -> R) -> Option<R> {
    let window = WINDOWS.with(|w| w.borrow().get(label).map(|e| e.window.clone()))?;
    Some(f(&window))
}

/// Run `f` in window `label`'s GPUI window, if it's open.
pub(crate) fn update_window(label: &str, cx: &mut App, f: impl FnOnce(&mut Window, &mut App)) {
    let handle = WINDOWS.with(|w| w.borrow().get(label).map(|e| e.handle));
    if let Some(handle) = handle {
        let _ = handle.update(cx, |_, window, cx| f(window, cx));
    }
}

/// A window, by label (`main`, `onboarding`, `dictate`).
#[derive(Clone, Debug)]
pub struct WebviewWindow {
    label: String,
}

impl WebviewWindow {
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Post `f` to the main queue, after the work already posted there.
    pub fn with_ns_window(&self, f: impl FnOnce(&NSWindow) + Send + 'static) -> Result<()> {
        let label = self.label.clone();
        main_async(move || {
            with_window(&label, f);
        });
        Ok(())
    }

    fn read<R: Send>(&self, f: impl FnOnce(&NSWindow) -> R + Send) -> Result<R> {
        main_sync(|| with_window(&self.label, f))
            .ok_or_else(|| Error(format!("The {} window is closed", self.label)))
    }

    pub fn show(&self) -> Result<()> {
        self.with_ns_window(|w| w.makeKeyAndOrderFront(None))
    }

    pub fn hide(&self) -> Result<()> {
        self.with_ns_window(|w| w.orderOut(None))
    }

    /// Bring Kass forward with this window as the key window.
    pub fn set_focus(&self) -> Result<()> {
        self.with_ns_window(|w| {
            if let Some(mtm) = MainThreadMarker::new() {
                #[allow(deprecated)]
                NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            }
            w.makeKeyAndOrderFront(None);
        })
    }

    pub fn unminimize(&self) -> Result<()> {
        self.with_ns_window(|w| unsafe {
            let _: () = msg_send![w, deminiaturize: Option::<&NSWindow>::None];
        })
    }

    pub fn set_ignore_cursor_events(&self, ignore: bool) -> Result<()> {
        self.with_ns_window(move |w| unsafe {
            let _: () = msg_send![w, setIgnoresMouseEvents: ignore];
        })
    }

    pub fn is_focused(&self) -> Result<bool> {
        self.read(|w| w.isKeyWindow())
    }

    pub fn is_visible(&self) -> Result<bool> {
        self.read(|w| w.isVisible())
    }

    /// Close the window without asking, as Tauri's `destroy` did.
    pub fn destroy(&self) -> Result<()> {
        let label = self.label.clone();
        main_async(move || {
            if let Some(handle) = forget_window(&label) {
                with_app(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                });
            }
        });
        Ok(())
    }
}

/// Take the window out of the registry. Returns its GPUI handle.
fn forget_window(label: &str) -> Option<AnyWindowHandle> {
    labels().lock().unwrap().remove(label);
    WINDOWS
        .with(|w| w.borrow_mut().remove(label))
        .map(|e| e.handle)
}

/// What happens when the user closes the window.
#[derive(Clone, Copy)]
pub enum OnClose {
    /// It hides, and stays ready to show again.
    Hide,
    /// It closes, after `then` runs.
    Close(fn()),
    /// It stays open.
    Never,
}

pub struct Spec {
    pub label: &'static str,
    pub options: WindowOptions,
    /// Runs on the new NSWindow before GPUI draws in it.
    pub prepare: fn(&NSWindow),
    pub on_close: OnClose,
    /// What GPUI draws in the window.
    pub root: fn(&mut Window, &mut App) -> AnyView,
}

struct Host {
    root: AnyView,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.root.clone())
    }
}

/// Open the window `spec` describes, or return it when it's open already.
pub fn open(cx: &mut App, spec: Spec) -> Result<WebviewWindow> {
    if let Some(window) = get(spec.label) {
        return Ok(window);
    }
    let Spec {
        label,
        options,
        prepare,
        on_close,
        root,
    } = spec;
    gpui_kit::open_window(options, cx, |window, cx| {
        let ns_window = ns_window(window);
        prepare(&ns_window);
        WINDOWS.with(|w| {
            w.borrow_mut().insert(
                label.to_string(),
                Entry {
                    window: ns_window,
                    handle: Window::window_handle(window),
                },
            )
        });
        labels().lock().unwrap().insert(label.to_string());
        window.on_window_should_close(cx, move |_, _| match on_close {
            OnClose::Hide => {
                with_window(label, |w| w.orderOut(None));
                false
            }
            OnClose::Close(then) => {
                forget_window(label);
                then();
                true
            }
            OnClose::Never => false,
        });
        let root = root(window, cx);
        cx.new(|_| Host { root })
    })
    .map_err(|e| Error(e.to_string()))?;
    Ok(WebviewWindow {
        label: label.to_string(),
    })
}

fn ns_window(window: &Window) -> Retained<NSWindow> {
    let handle = HasWindowHandle::window_handle(window).expect("GPUI window handle");
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        unreachable!("macOS windows are AppKit windows")
    };
    // SAFETY: GPUI's AppKit handle points at the window's live content view.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window().expect("the content view is in its window")
}
