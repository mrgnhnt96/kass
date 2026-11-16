//! Spike (T561): can one GPUI app own the macOS loop and still host Kass's
//! React screens (wry WKWebView in GPUI windows, Tauri-compatible IPC), a
//! never-key pill panel, a menu bar item and the Edit menu?
//!
//! `gpui-host-spike` runs the app. `gpui-host-spike <cmd...>` talks to it
//! over a Unix socket so the checks can be scripted.
use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSPasteboard, NSView, NSWindow, NSWorkspace};
use objc2_foundation::NSString;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    http::{Request, Response},
    BackgroundThrottlingPolicy, Rect, WebView, WebViewBuilder,
};

actions!(spike, [Quit, Undo, Redo, Cut, Copy, Paste, SelectAll]);

const HEADER: f32 = 36.0;
const PILL_W: f64 = 360.0;
const PILL_H: f64 = 140.0;

fn sock_path() -> String {
    format!("{}/gpui-host-spike.sock", std::env::temp_dir().display())
}

fn dist_dir() -> PathBuf {
    std::env::var("SPIKE_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tauri/dist")))
}

type Req = (String, mpsc::Sender<String>);

thread_local! {
    static WEBVIEWS: RefCell<Vec<(String, WebView)>> = const { RefCell::new(Vec::new()) };
    static MAIN_WIN: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static PILL_WIN: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static REPLIES: RefCell<VecDeque<(String, u64, bool, serde_json::Value)>> = const { RefCell::new(VecDeque::new()) };
    static IPC_LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static TRAY: RefCell<Option<tray_icon::TrayIcon>> = const { RefCell::new(None) };
}

fn log(line: impl AsRef<str>) {
    eprintln!("[spike] {}", line.as_ref());
}

fn ns_window(window: &Window) -> Retained<NSWindow> {
    let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).unwrap().as_raw() else {
        panic!("not AppKit")
    };
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    view.window().expect("window")
}

// ---------------- Tauri-compatible bridge ----------------

/// Stands in for Tauri's injected `__TAURI_INTERNALS__`, so `@tauri-apps/api`
/// (invoke, listen, getCurrentWebviewWindow) works unchanged. Events are
/// kept in JS; Rust emits with `window.__KASS_EMIT__(event, payload)`.
fn bridge_js(label: &str) -> String {
    format!(
        r#"(function () {{
  const callbacks = new Map();
  const pending = new Map();
  const listeners = new Map();
  let next = 1;
  const transformCallback = (cb, once) => {{
    const id = next++;
    callbacks.set(id, (data) => {{ if (once) callbacks.delete(id); return cb && cb(data); }});
    return id;
  }};
  const send = (cmd, args) => new Promise((resolve, reject) => {{
    const id = next++;
    pending.set(id, {{ resolve, reject }});
    window.ipc.postMessage(JSON.stringify({{ id, cmd, args }}));
  }});
  window.__TAURI_INTERNALS__ = {{
    metadata: {{ currentWindow: {{ label: "{label}" }}, currentWebview: {{ windowLabel: "{label}", label: "{label}" }} }},
    transformCallback,
    unregisterCallback: (id) => callbacks.delete(id),
    runCallback: (id, data) => {{ const f = callbacks.get(id); if (f) f(data); }},
    callbacks,
    convertFileSrc: (p, protocol = "asset") => `${{protocol}}://localhost/${{encodeURIComponent(p)}}`,
    invoke(cmd, args = {{}}, _options) {{
      if (cmd === "plugin:event|listen") {{
        const id = next++;
        if (!listeners.has(args.event)) listeners.set(args.event, new Map());
        listeners.get(args.event).set(id, args.handler);
        return Promise.resolve(id);
      }}
      if (cmd === "plugin:event|unlisten") return Promise.resolve();
      if (cmd === "plugin:event|emit" || cmd === "plugin:event|emit_to") return send(cmd, args);
      return send(cmd, args);
    }},
    __resolve(id, ok, value) {{
      const p = pending.get(id);
      if (!p) return;
      pending.delete(id);
      ok ? p.resolve(value) : p.reject(value);
    }},
  }};
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {{
    unregisterListener(event, id) {{ listeners.get(event)?.delete(id); }},
  }};
  window.__KASS_EMIT__ = (event, payload) => {{
    for (const [id, handler] of listeners.get(event) ?? []) {{
      const f = callbacks.get(handler);
      if (f) f({{ event, id, payload }});
    }}
  }};
}})();"#
    )
}

/// What the stub backend answers. A real host routes these to the same
/// functions the Tauri commands call today.
fn handle_invoke(label: &str, cmd: &str, args: &serde_json::Value) -> (bool, serde_json::Value) {
    IPC_LOG.with(|l| l.borrow_mut().push(format!("{label}:{cmd}")));
    match cmd {
        "plugin:app|version" => (true, "0.10.0-beta.7".into()),
        "spike_echo" => (true, args.clone()),
        "dictation_start" => (true, 1.into()),
        "plugin:event|emit" | "plugin:event|emit_to" => {
            let event = args["event"].as_str().unwrap_or_default().to_string();
            let payload = args.get("payload").cloned().unwrap_or(serde_json::Value::Null);
            log(format!("{label} emitted {event}"));
            if event == "dictate:hide" {
                pill_hide();
            }
            emit_all(&event, &payload);
            (true, serde_json::Value::Null)
        }
        _ => (false, format!("{cmd}: not in the spike").into()),
    }
}

fn emit_all(event: &str, payload: &serde_json::Value) {
    let js = format!(
        "window.__KASS_EMIT__ && window.__KASS_EMIT__({}, {})",
        serde_json::to_string(event).unwrap(),
        payload
    );
    WEBVIEWS.with(|w| {
        for (_, wv) in w.borrow().iter() {
            let _ = wv.evaluate_script(&js);
        }
    });
}

fn drain_replies() {
    let replies: Vec<_> = REPLIES.with(|r| r.borrow_mut().drain(..).collect());
    for (label, id, ok, value) in replies {
        let js = format!("window.__TAURI_INTERNALS__.__resolve({id}, {ok}, {value})");
        WEBVIEWS.with(|w| {
            if let Some((_, wv)) = w.borrow().iter().find(|(l, _)| *l == label) {
                let _ = wv.evaluate_script(&js);
            }
        });
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        Some("wav") | Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// Builds a web view as a child of a GPUI window, served from `tauri://localhost`
/// so the origin (and so localStorage and the server's CORS list) is the one
/// the Tauri build has today.
fn build_webview(window: &Window, label: &str, query: &str, bounds: Rect, transparent: bool) -> WebView {
    let ipc_label = label.to_string();
    let builder = WebViewBuilder::new()
        .with_initialization_script(bridge_js(label))
        .with_custom_protocol("tauri".into(), |_id, req: Request<Vec<u8>>| {
            let path = req.uri().path().trim_start_matches('/');
            let path = if path.is_empty() { "index.html" } else { path };
            match std::fs::read(dist_dir().join(path)) {
                Ok(bytes) => Response::builder()
                    .header("Content-Type", mime(path))
                    .body(Cow::Owned(bytes))
                    .unwrap(),
                Err(_) => Response::builder().status(404).body(Cow::Borrowed(&b""[..])).unwrap(),
            }
        })
        .with_ipc_handler(move |req: Request<String>| {
            let msg: serde_json::Value = serde_json::from_str(req.body()).unwrap_or_default();
            let id = msg["id"].as_u64().unwrap_or(0);
            let cmd = msg["cmd"].as_str().unwrap_or_default().to_string();
            let (ok, value) = handle_invoke(&ipc_label, &cmd, &msg["args"]);
            REPLIES.with(|r| r.borrow_mut().push_back((ipc_label.clone(), id, ok, value)));
        })
        .with_url(format!("tauri://localhost/{query}"))
        .with_bounds(bounds)
        .with_transparent(transparent)
        .with_devtools(true)
        .with_accept_first_mouse(true)
        .with_background_throttling(BackgroundThrottlingPolicy::Disabled);
    builder.build_as_child(window).expect("webview")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(w, h).into(),
    }
}

// ---------------- Main window ----------------

struct MainHost {
    last: Option<(f32, f32)>,
}

impl Render for MainHost {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        let (w, h) = (f32::from(size.width), f32::from(size.height));
        if self.last != Some((w, h)) {
            self.last = Some((w, h));
            WEBVIEWS.with(|v| {
                if let Some((_, wv)) = v.borrow().iter().find(|(l, _)| l == "main") {
                    let _ = wv.set_bounds(rect(0.0, HEADER as f64, w as f64, (h - HEADER) as f64));
                }
            });
        }
        div().size_full().bg(rgb(0x101014)).child(
            div()
                .h(px(HEADER))
                .w_full()
                .pl(px(80.0))
                .flex()
                .items_center()
                .text_color(rgb(0x7fe0b0))
                .text_sm()
                .child("GPUI header · React screen below in a wry WKWebView"),
        )
    }
}

fn open_main(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1100.0), px(720.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Kass (GPUI host spike)".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(12.0))),
        }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        let wv = build_webview(window, "main", "", rect(0.0, HEADER as f64, 1100.0, 720.0 - HEADER as f64), false);
        WEBVIEWS.with(|v| v.borrow_mut().push(("main".into(), wv)));
        MAIN_WIN.with(|m| *m.borrow_mut() = Some(ns_window(window)));
        cx.new(|_| MainHost { last: None })
    })
    .expect("main window");
}

// ---------------- Pill ----------------

struct PillHost;
impl Render for PillHost {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

extern "C-unwind" fn never_key(_this: &AnyObject, _sel: Sel) -> Bool {
    Bool::NO
}

/// GPUI's PopUp is a non-activating NSPanel, but its class answers YES to
/// canBecomeKeyWindow. Kass's pill must never take keys from the app the user
/// is typing in, so swap in a subclass that says NO (the same isa swap Tauri's
/// build does today).
fn make_never_key(win: &NSWindow) {
    unsafe {
        let current: &AnyClass = win.class();
        let name = format!("KassNeverKey_{}", current.name().to_string_lossy());
        let cname = std::ffi::CString::new(name).unwrap();
        let class = match AnyClass::get(&cname) {
            Some(c) => c,
            None => {
                let mut b = ClassBuilder::new(&cname, current).unwrap();
                b.add_method(sel!(canBecomeKeyWindow), never_key as extern "C-unwind" fn(_, _) -> _);
                b.add_method(sel!(canBecomeMainWindow), never_key as extern "C-unwind" fn(_, _) -> _);
                b.register()
            }
        };
        objc2::ffi::object_setClass(win as *const NSWindow as *mut _, class as *const AnyClass);
    }
}

fn open_pill(cx: &mut App) {
    let display = cx.primary_display();
    let screen = display.as_ref().map(|d| d.bounds()).unwrap_or(Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(1512.0), px(982.0)),
    });
    let origin = point(
        screen.origin.x + (screen.size.width - px(PILL_W as f32)) / 2.0,
        screen.origin.y + screen.size.height - px(PILL_H as f32) - px(24.0),
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: size(px(PILL_W as f32), px(PILL_H as f32)),
        })),
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: false,
        show: false,
        is_movable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        let ns = ns_window(window);
        log(format!("pill class before webview: {}", ns.class().name().to_string_lossy()));
        make_never_key(&ns);
        let wv = build_webview(window, "dictate", "?view=dictate", rect(0.0, 0.0, PILL_W, PILL_H), true);
        WEBVIEWS.with(|v| v.borrow_mut().push(("dictate".into(), wv)));
        unsafe {
            let _: () = msg_send![&*ns, setHasShadow: false];
            let _: () = msg_send![&*ns, setIgnoresMouseEvents: true];
        }
        PILL_WIN.with(|p| *p.borrow_mut() = Some(ns));
        cx.new(|_| PillHost)
    })
    .expect("pill window");
}

fn pill_show() {
    PILL_WIN.with(|p| {
        if let Some(w) = &*p.borrow() {
            unsafe {
                let _: () = msg_send![&**w, setIgnoresMouseEvents: false];
            }
            let f = w.frame();
            if f.size.width < 1.0 {
                let mtm = MainThreadMarker::new().unwrap();
                let vis = w.screen().or_else(|| objc2_app_kit::NSScreen::mainScreen(mtm)).map(|s| s.visibleFrame()).unwrap();
                let r = objc2_foundation::NSRect::new(
                    objc2_foundation::NSPoint::new(vis.origin.x + (vis.size.width - PILL_W) / 2.0, vis.origin.y + 24.0),
                    objc2_foundation::NSSize::new(PILL_W, PILL_H),
                );
                log(format!("pill frame was {:?}; setting {:?}", f, r));
                w.setFrame_display(r, true);
            }
            w.orderFrontRegardless();
        }
    });
}

fn pill_hide() {
    PILL_WIN.with(|p| {
        if let Some(w) = &*p.borrow() {
            unsafe {
                let _: () = msg_send![&**w, setIgnoresMouseEvents: true];
            }
            w.orderOut(None);
        }
    });
}

// ---------------- Menu bar item ----------------

fn make_tray() {
    use tray_icon::menu::{Menu, MenuItem};
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("show", "Show Kass", true, None));
    let _ = menu.append(&MenuItem::with_id("pill", "Show pill", true, None));
    let _ = menu.append(&MenuItem::with_id("hide-pill", "Hide pill", true, None));
    let _ = menu.append(&MenuItem::with_id("quit", "Quit", true, None));
    let tray = tray_icon::TrayIconBuilder::new()
        .with_title("Kass")
        .with_menu(Box::new(menu))
        .build()
        .expect("tray");
    TRAY.with(|t| *t.borrow_mut() = Some(tray));
}

fn poll_tray(cx: &mut App) {
    while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
        log(format!("menu bar item: {}", event.id.0));
        match event.id.0.as_str() {
            "show" => show_main(),
            "pill" => pill_show(),
            "hide-pill" => pill_hide(),
            "quit" => cx.quit(),
            _ => {}
        }
    }
}

fn show_main() {
    MAIN_WIN.with(|m| {
        if let Some(w) = &*m.borrow() {
            w.makeKeyAndOrderFront(None);
        }
    });
    let mtm = MainThreadMarker::new().unwrap();
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

// ---------------- Control socket ----------------

fn win_desc(name: &str, w: &NSWindow) -> String {
    let can_key: bool = unsafe { msg_send![w, canBecomeKeyWindow] };
    let level: isize = unsafe { msg_send![w, level] };
    let fr: Option<Retained<AnyObject>> = unsafe { msg_send![w, firstResponder] };
    let fr = fr.map(|o| o.class().name().to_string_lossy().into_owned()).unwrap_or("-".into());
    format!(
        "{name}: frame={:?} number={} class={} visible={} key={} canBecomeKey={can_key} level={level} firstResponder={fr}",
        w.frame(),
        w.windowNumber(),
        w.class().name().to_string_lossy(),
        w.isVisible(),
        w.isKeyWindow()
    )
}

fn status() -> String {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let front = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|a| a.localizedName())
        .map(|n| n.to_string())
        .unwrap_or("-".into());
    let mut out = vec![format!(
        "pid={} NSApp.class={} active={} frontmost={front} tray={}",
        std::process::id(),
        app.class().name().to_string_lossy(),
        app.isActive(),
        TRAY.with(|t| t.borrow().is_some())
    )];
    MAIN_WIN.with(|m| m.borrow().as_ref().map(|w| out.push(win_desc("main", w))));
    PILL_WIN.with(|m| m.borrow().as_ref().map(|w| out.push(win_desc("pill", w))));
    out.join("\n")
}

/// Sends a key through NSApp like a real key press: key equivalents go
/// through the window and then the main menu, the rest to the first responder.
fn send_key(chars: &str, cmd: bool) -> String {
    let Some(win) = MAIN_WIN.with(|m| m.borrow().clone()) else { return "no main".into() };
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let flags: usize = if cmd { 1 << 20 } else { 0 };
    let mut handled = Vec::new();
    for c in chars.chars() {
        let s = NSString::from_str(&c.to_string());
        let code: u16 = match c {
            'a' => 0, 'c' => 8, 'v' => 9, 'x' => 7, 'z' => 6, _ => 0,
        };
        for kind in [10usize, 11usize] {
            unsafe {
                let ev: Option<Retained<AnyObject>> = msg_send![
                    objc2::class!(NSEvent),
                    keyEventWithType: kind,
                    location: objc2_foundation::NSPoint::new(0.0, 0.0),
                    modifierFlags: flags,
                    timestamp: 0.0f64,
                    windowNumber: win.windowNumber(),
                    context: std::ptr::null_mut::<AnyObject>(),
                    characters: &*s,
                    charactersIgnoringModifiers: &*s,
                    isARepeat: false,
                    keyCode: code
                ];
                if let Some(ev) = ev {
                    if cmd && kind == 10 {
                        let menu: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
                        let by_window: bool = msg_send![&*win, performKeyEquivalent: &*ev];
                        let by_menu: bool = if by_window {
                            false
                        } else {
                            menu.map(|m| msg_send![&*m, performKeyEquivalent: &*ev]).unwrap_or(false)
                        };
                        handled.push(format!("{c}: window={by_window} menu={by_menu}"));
                    } else if !cmd {
                        let _: () = msg_send![&*app, sendEvent: &*ev];
                    }
                }
            }
        }
    }
    handled.join(", ")
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(src: *const std::ffi::c_void, key: u16, down: bool) -> *mut std::ffi::c_void;
    fn CGEventSetFlags(e: *mut std::ffi::c_void, flags: u64);
    fn CGEventPost(tap: u32, e: *mut std::ffi::c_void);
    fn CFRelease(o: *mut std::ffi::c_void);
    fn AXIsProcessTrusted() -> bool;
}

/// A real key press through the HID event tap, the path a user's keyboard takes.
fn hid_key(cmd: bool, code: u16) -> String {
    unsafe {
        for down in [true, false] {
            let e = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
            CGEventSetFlags(e, if cmd { 1 << 20 } else { 0 });
            CGEventPost(0, e);
            CFRelease(e);
        }
        format!("posted (AXIsProcessTrusted={})", AXIsProcessTrusted())
    }
}

fn edit_menu() -> String {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let mut out = vec![];
    unsafe {
        let menu: Retained<AnyObject> = msg_send![&*app, mainMenu];
        let n: isize = msg_send![&*menu, numberOfItems];
        for i in 0..n {
            let top: Retained<AnyObject> = msg_send![&*menu, itemAtIndex: i];
            let sub: Option<Retained<AnyObject>> = msg_send![&*top, submenu];
            let Some(sub) = sub else { continue };
            let m: isize = msg_send![&*sub, numberOfItems];
            for j in 0..m {
                let it: Retained<AnyObject> = msg_send![&*sub, itemAtIndex: j];
                let title: Retained<NSString> = msg_send![&*it, title];
                let key: Retained<NSString> = msg_send![&*it, keyEquivalent];
                let mask: usize = msg_send![&*it, keyEquivalentModifierMask];
                let action: Option<Sel> = msg_send![&*it, action];
                out.push(format!("{title} key={key:?} mask=0x{mask:x} action={action:?}"));
            }
        }
    }
    out.join("\n")
}

fn pasteboard() -> String {
    let pb = NSPasteboard::generalPasteboard();
    unsafe { pb.stringForType(objc2_app_kit::NSPasteboardTypeString) }
        .map(|s| s.to_string())
        .unwrap_or("<empty>".into())
}

fn eval(label: &str, js: &str, reply: mpsc::Sender<String>) {
    WEBVIEWS.with(|w| {
        if let Some((_, wv)) = w.borrow().iter().find(|(l, _)| l == label) {
            let reply2 = reply.clone();
            if wv
                .evaluate_script_with_callback(js, move |r| {
                    let _ = reply2.send(r);
                })
                .is_err()
            {
                let _ = reply.send("eval failed".into());
            }
        } else {
            let _ = reply.send(format!("no webview {label}"));
        }
    });
}

fn handle(line: &str, reply: mpsc::Sender<String>, cx: &mut App) {
    let mut parts = line.splitn(3, ' ');
    let cmd = parts.next().unwrap_or_default();
    let a = parts.next().unwrap_or_default();
    let b = parts.next().unwrap_or_default();
    let out = match cmd {
        "status" => status(),
        "ipclog" => IPC_LOG.with(|l| l.borrow().join("\n")),
        "pill" => {
            if a == "show" { pill_show() } else { pill_hide() }
            status()
        }
        "show" => {
            show_main();
            status()
        }
        "emit" => {
            emit_all(a, &serde_json::from_str(b).unwrap_or(serde_json::Value::Null));
            "emitted".into()
        }
        "focus" => {
            WEBVIEWS.with(|w| {
                if let Some((_, wv)) = w.borrow().iter().find(|(l, _)| l == a) {
                    let _ = wv.focus();
                }
            });
            status()
        }
        "key" => send_key(b, a == "cmd"),
        "paste" => pasteboard(),
        "menu" => edit_menu(),
        "send" => {
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            let name = std::ffi::CString::new(a).unwrap();
            let selector = Sel::register(&name);
            let ok: bool = unsafe {
                msg_send![&*app, sendAction: selector, to: std::ptr::null::<AnyObject>(), from: std::ptr::null::<AnyObject>()]
            };
            format!("{a} handled={ok}")
        }
        "hidkey" => hid_key(a == "cmd", b.parse().unwrap_or(0)),
        "setpaste" => {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            unsafe { pb.setString_forType(&NSString::from_str(&format!("{a} {b}").trim().to_string()), objc2_app_kit::NSPasteboardTypeString) };
            "set".into()
        }
        "eval" => return eval(a, b, reply),
        "quit" => {
            cx.quit();
            "bye".into()
        }
        _ => format!("unknown: {cmd}"),
    };
    let _ = reply.send(out);
}

fn serve(tx: mpsc::Sender<Req>) {
    let path = sock_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind");
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let (rtx, rrx) = mpsc::channel();
                let _ = tx.send((line.trim().to_string(), rtx));
                let out = rrx.recv_timeout(Duration::from_secs(10)).unwrap_or("timeout".into());
                let mut s = stream;
                let _ = s.write_all(out.as_bytes());
                let _ = s.write_all(b"\n");
            }
        });
    }
}

fn client(args: &[String]) {
    let mut s = UnixStream::connect(sock_path()).expect("spike not running");
    s.write_all(args.join(" ").as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).ok();
    let mut rest = String::new();
    print!("{out}");
    let _ = rest;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let mut out = String::new();
        let mut s = UnixStream::connect(sock_path()).expect("spike not running");
        s.write_all(args.join(" ").as_bytes()).unwrap();
        s.write_all(b"\n").unwrap();
        let _ = std::io::Read::read_to_string(&mut s, &mut out);
        print!("{out}");
        let _ = client;
        return;
    }

    let app = gpui_kit::application();
    app.on_reopen(|cx| {
        log("reopen (Dock icon clicked)");
        let _ = cx;
        show_main();
    });
    app.on_open_urls(|urls| log(format!("open urls: {urls:?}")));
    app.run(|cx| {
        gpui_kit::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        // Web views go before their windows do; dropping one after GPUI has
        // freed its NSWindow crashes in WebKit's window observer.
        cx.on_app_quit(|_| {
            // By now GPUI has torn the windows down; the process is exiting,
            // so skip the web views' destructors instead of running them late.
            WEBVIEWS.with(|w| std::mem::forget(std::mem::take(&mut *w.borrow_mut())));
            async {}
        })
        .detach();
        // No GPUI element handled the edit key (a web view has focus):
        // hand it to the native responder chain, where WKWebView takes it.
        fn native(selector: Sel) {
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            let ok: bool = unsafe {
                msg_send![&*app, sendAction: selector, to: std::ptr::null::<AnyObject>(), from: std::ptr::null::<AnyObject>()]
            };
            log(format!("native {:?} -> {ok}", selector));
        }
        cx.on_action(|_: &Undo, _| native(sel!(undo:)));
        cx.on_action(|_: &Redo, _| native(sel!(redo:)));
        cx.on_action(|_: &Cut, _| native(sel!(cut:)));
        cx.on_action(|_: &Copy, _| native(sel!(copy:)));
        cx.on_action(|_: &Paste, _| native(sel!(paste:)));
        cx.on_action(|_: &SelectAll, _| native(sel!(selectAll:)));
        // Bind before set_menus: GPUI reads the keymap for the menu's shortcuts.
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-z", Undo, None),
            KeyBinding::new("cmd-shift-z", Redo, None),
            KeyBinding::new("cmd-x", Cut, None),
            KeyBinding::new("cmd-c", Copy, None),
            KeyBinding::new("cmd-v", Paste, None),
            KeyBinding::new("cmd-a", SelectAll, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Kass".into(),
                items: vec![MenuItem::action("Quit Kass", Quit)],
                disabled: false,
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::os_action("Undo", Undo, OsAction::Undo),
                    MenuItem::os_action("Redo", Redo, OsAction::Redo),
                    MenuItem::separator(),
                    MenuItem::os_action("Cut", Cut, OsAction::Cut),
                    MenuItem::os_action("Copy", Copy, OsAction::Copy),
                    MenuItem::os_action("Paste", Paste, OsAction::Paste),
                    MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                ],
                disabled: false,
            },
        ]);

        cx.observe_keystrokes(|ev, _, _| {
            log(format!("gpui keystroke {:?} action={:?}", ev.keystroke.unparse(), ev.action.as_ref().map(|a| a.name())));
        })
        .detach();
        open_main(cx);
        open_pill(cx);
        make_tray();
        cx.activate(true);

        let (tx, rx) = mpsc::channel::<Req>();
        std::thread::spawn(move || serve(tx));
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            let _ = cx.update(|cx| {
                drain_replies();
                poll_tray(cx);
                while let Ok((line, reply)) = rx.try_recv() {
                    handle(&line, reply, cx);
                }
            });
        })
        .detach();
        log("up");
    });
}
