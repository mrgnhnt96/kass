//! GPUI windows that each host one React screen in a wry web view.
//!
//! The web views live on the main thread, in `WINDOWS`. A `WebviewWindow`
//! is only a label, so it can go to any thread: what it does is posted to
//! the main queue, in order.

use std::borrow::Cow;
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
use serde_json::Value;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::{Request, Response};
use wry::{BackgroundThrottlingPolicy, Rect, WebView, WebViewBuilder, WebViewExtMacOS};

use super::{main_async, main_sync, with_app, Error, Result};

struct Entry {
    webview: Option<WebView>,
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

fn with_webview(label: &str, f: impl FnOnce(&WebView)) {
    WINDOWS.with(|w| {
        if let Some(webview) = w.borrow().get(label).and_then(|e| e.webview.as_ref()) {
            f(webview);
        }
    });
}

/// A window with a web view, by label (`main`, `onboarding`, `dictate`).
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

/// Take the window out of the registry and drop its web view, which must go
/// before GPUI frees the NSWindow. Returns its GPUI handle.
fn forget_window(label: &str) -> Option<AnyWindowHandle> {
    labels().lock().unwrap().remove(label);
    let entry = WINDOWS.with(|w| w.borrow_mut().remove(label))?;
    let Entry {
        webview, handle, ..
    } = entry;
    drop(webview);
    Some(handle)
}

/// Skip the web views' destructors as the app quits: GPUI has torn the
/// windows down by then, and WebKit crashes dropping one after its window.
pub(crate) fn leak_all() {
    WINDOWS.with(|w| std::mem::forget(std::mem::take(&mut *w.borrow_mut())));
}

/// Send an event to every page (see `__KASS_EMIT__` in bridge.js).
pub(crate) fn broadcast(event: &str, payload: &Value, target: Option<&str>) {
    let js = format!(
        "window.__KASS_EMIT__ && window.__KASS_EMIT__({}, {}, {})",
        Value::from(event),
        payload,
        target.map(Value::from).unwrap_or(Value::Null)
    );
    WINDOWS.with(|w| {
        for entry in w.borrow().values() {
            if let Some(webview) = &entry.webview {
                let _ = webview.evaluate_script(&js);
            }
        }
    });
}

/// Settle an `invoke` from the page in window `label`.
pub(crate) fn resolve(label: &str, id: u64, ok: bool, value: &Value) {
    let js = format!("window.__TAURI_INTERNALS__.__resolve({id}, {ok}, {value})");
    with_webview(label, |webview| {
        let _ = webview.evaluate_script(&js);
    });
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
    /// Appended to the page URL, like `?view=dictate`.
    pub query: &'static str,
    pub options: WindowOptions,
    pub transparent: bool,
    /// Runs on the new NSWindow before the web view attaches to it.
    pub prepare: fn(&NSWindow),
    pub on_close: OnClose,
    /// What GPUI draws under the web view. `None` draws nothing: the web view
    /// covers the window. A window with a root places its web view with
    /// [`place`].
    pub root: Option<fn(&mut Window, &mut App) -> AnyView>,
}

struct Host {
    root: Option<AnyView>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.root.clone())
    }
}

/// Show the web view of window `label` over `frame` (in window points, from
/// the top left), or hide it with `None`. Main thread only. The web view keeps
/// its margins as the window resizes, so call this when the frame's margins
/// change, not on every resize. False when the window has no web view yet.
pub(crate) fn place(label: &str, frame: Option<Rect>) -> bool {
    let mut placed = false;
    with_webview(label, |webview| {
        placed = true;
        match frame {
            Some(frame) => {
                let _ = webview.set_bounds(frame);
                let _ = webview.set_visible(true);
            }
            None => {
                let _ = webview.set_visible(false);
            }
        }
    });
    placed
}

/// Open the window `spec` describes, or return it when it's open already.
pub fn open(cx: &mut App, spec: Spec) -> Result<WebviewWindow> {
    if let Some(window) = get(spec.label) {
        return Ok(window);
    }
    let Spec {
        label,
        query,
        options,
        transparent,
        prepare,
        on_close,
        root,
    } = spec;
    let mut failed = None;
    gpui_kit::open_window(options, cx, |window, cx| {
        let ns_window = ns_window(window);
        prepare(&ns_window);
        let size = window.viewport_size();
        let bounds = Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(
                f64::from(f32::from(size.width)),
                f64::from(f32::from(size.height)),
            )
            .into(),
        };
        match build_webview(window, label, query, bounds, transparent) {
            Ok(webview) => {
                // Follow the window's size, as Tauri's did. wry pins a child
                // web view's top edge instead.
                unsafe {
                    let _: () = msg_send![&*webview.webview(), setAutoresizingMask: 18usize];
                }
                WINDOWS.with(|w| {
                    w.borrow_mut().insert(
                        label.to_string(),
                        Entry {
                            webview: Some(webview),
                            window: ns_window,
                            handle: Window::window_handle(window),
                        },
                    )
                });
                labels().lock().unwrap().insert(label.to_string());
            }
            Err(e) => failed = Some(e.to_string()),
        }
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
        let root = root.map(|build| build(window, cx));
        cx.new(|_| Host { root })
    })
    .map_err(|e| Error(e.to_string()))?;
    match failed {
        Some(e) => Err(Error(format!("The {label} window has no web view: {e}"))),
        None => Ok(WebviewWindow {
            label: label.to_string(),
        }),
    }
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

fn build_webview(
    window: &Window,
    label: &'static str,
    query: &str,
    bounds: Rect,
    transparent: bool,
) -> wry::Result<WebView> {
    WebViewBuilder::new()
        .with_initialization_script(super::ipc::bridge_js(label))
        .with_custom_protocol("tauri".into(), |_, request: Request<Vec<u8>>| {
            serve(request.uri().path())
        })
        .with_ipc_handler(move |request: Request<String>| super::ipc::handle(label, request.body()))
        .with_url(page_url(query))
        .with_bounds(bounds)
        .with_transparent(transparent)
        .with_devtools(cfg!(debug_assertions))
        .with_accept_first_mouse(true)
        // The main page starts the server and arms the keys while hidden.
        .with_background_throttling(BackgroundThrottlingPolicy::Disabled)
        .build_as_child(window)
}

/// Where a screen loads from: the bundled pages at `tauri://localhost`, the
/// origin the Tauri build had (so saved settings and the server's allowed
/// origins carry over), or Vite's dev server.
fn page_url(query: &str) -> String {
    if cfg!(feature = "custom-protocol") {
        format!("tauri://localhost/{query}")
    } else {
        let base = std::env::var("KASS_DEV_URL").unwrap_or_else(|_| "http://localhost:5173".into());
        format!("{}/{query}", base.trim_end_matches('/'))
    }
}

#[cfg(feature = "custom-protocol")]
static DIST: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../dist");

/// The bundled page file at `path`, and its type. A path with no extension
/// is a route, which is `index.html`.
fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    #[cfg(feature = "custom-protocol")]
    {
        let path = path.trim_start_matches('/');
        let file = DIST.get_file(path).or_else(|| {
            (!path.contains('.'))
                .then(|| DIST.get_file("index.html"))
                .flatten()
        })?;
        let name = file.path().to_str().unwrap_or_default();
        Some((file.contents(), mime(name)))
    }
    #[cfg(not(feature = "custom-protocol"))]
    {
        let _ = path;
        None
    }
}

fn serve(path: &str) -> Response<Cow<'static, [u8]>> {
    match asset(path) {
        Some((bytes, kind)) => Response::builder()
            .header("Content-Type", kind)
            .body(Cow::Borrowed(bytes)),
        None => Response::builder()
            .status(404)
            .body(Cow::Borrowed(&b""[..])),
    }
    .expect("a valid response")
}

#[cfg_attr(not(feature = "custom-protocol"), allow(dead_code))]
fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wasm" => "application/wasm",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_types_by_extension() {
        assert_eq!(mime("assets/index-abc.js"), "text/javascript");
        assert_eq!(mime("index.html"), "text/html");
        assert_eq!(mime("fonts/x.woff2"), "font/woff2");
        assert_eq!(mime("README"), "application/octet-stream");
    }

    #[test]
    fn dev_build_loads_from_vite() {
        if !cfg!(feature = "custom-protocol") && std::env::var("KASS_DEV_URL").is_err() {
            assert_eq!(
                page_url("?view=dictate"),
                "http://localhost:5173/?view=dictate"
            );
        }
    }
}
