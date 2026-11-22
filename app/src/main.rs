mod accessibility;
mod app_hide;
mod app_icon;
mod app_location;
mod clipboard;
mod deep_link;
mod dictate;
mod dictation;
mod dictation_handshake;
mod focus_capture;
mod host;
mod hotkey_monitor;
mod identifier_move;
mod input_monitoring;
#[cfg(test)]
mod insert_bench;
mod insert_chain;
mod join;
mod key_codes;
mod keyboard_layout;
mod keystroke_insert;
mod login_item;
mod onboarding;
mod overlap;
mod palette;
mod prefs;
mod report;
mod server_process;
mod server_version;
mod settings;
mod shell;
mod sidecar;
mod sound_cues;
mod synthetic_keys;
mod text_insert;
mod this_mac;
mod updater;

use std::sync::Mutex;

use gpui_kit::{
    point, px, size, App, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions,
};
use host::windows::{self, OnClose, Spec};
use host::{AppHandle, Emitter, Manager, State};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_app_kit::{NSScreen, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};

pub const DICTATE_WINDOW_LABEL: &str = "dictate";
const MAIN_WINDOW_LABEL: &str = "main";
/// The origin Kass's requests carry, which the server allows. It is the one
/// the app's web pages had, so servers on other Macs keep accepting it.
const APP_ORIGIN: &str = "tauri://localhost";
pub const ONBOARDING_WINDOW_LABEL: &str = "onboarding";
const MAIN_WINDOW_WIDTH: f32 = 1200.0;
const MAIN_WINDOW_HEIGHT: f32 = 800.0;
const ONBOARDING_WINDOW_WIDTH: f32 = 880.0;
const ONBOARDING_WINDOW_HEIGHT: f32 = 600.0;
const DICTATE_WINDOW_WIDTH: f64 = 420.0;
const DICTATE_WINDOW_HEIGHT: f64 = 64.0;
const DICTATE_BOTTOM_PADDING: f64 = 24.0;
/// Room above the pill for the style chip (22 pt, its 8 pt gap, its glow and
/// its drift). Only added while the chip shows: the window takes every click
/// over it, so the rest of the time it stays the pill's size.
const DICTATE_CHIP_SPACE: f64 = 40.0;

/// A title bar drawn over the page, with no title, as Tauri's `Overlay`.
fn overlay_titlebar() -> Option<TitlebarOptions> {
    Some(TitlebarOptions {
        title: Some("Kass".into()),
        appears_transparent: true,
        traffic_light_position: None,
    })
}

/// The main window. It starts hidden so a login launch stays out of the way
/// until the Dock icon is clicked; the server and the global keys start
/// without it (src/shell/launch.rs).
fn main_window_spec(cx: &App) -> Spec {
    Spec {
        label: MAIN_WINDOW_LABEL,
        options: WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(MAIN_WINDOW_WIDTH), px(MAIN_WINDOW_HEIGHT)),
                cx,
            ))),
            titlebar: overlay_titlebar(),
            show: false,
            focus: false,
            window_min_size: Some(size(px(800.0), px(600.0))),
            ..Default::default()
        },
        prepare: |_| {},
        // Closing it hides it: Kass keeps running for dictation, and the
        // Dock icon brings it back. The server stops when Kass quits.
        on_close: OnClose::Hide,
        root: shell::root,
    }
}

/// The floating dictate pill, built hidden before the first chord. The
/// HotkeyMonitor shows it on chord-start; the pill (src/dictate.rs) parks it
/// when the take finishes.
fn dictate_window_spec() -> Spec {
    Spec {
        label: DICTATE_WINDOW_LABEL,
        options: WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(-10_000.0), px(-10_000.0)),
                size: size(
                    px(DICTATE_WINDOW_WIDTH as f32),
                    px(DICTATE_WINDOW_HEIGHT as f32),
                ),
            })),
            titlebar: None,
            // A non-activating panel, so it can float over another app's
            // fullscreen Space.
            kind: WindowKind::PopUp,
            focus: false,
            show: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        prepare: prepare_pill,
        on_close: OnClose::Never,
        root: dictate::root,
    }
}

fn onboarding_window_spec(cx: &App) -> Spec {
    Spec {
        label: ONBOARDING_WINDOW_LABEL,
        options: WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(ONBOARDING_WINDOW_WIDTH), px(ONBOARDING_WINDOW_HEIGHT)),
                cx,
            ))),
            titlebar: overlay_titlebar(),
            is_resizable: false,
            ..Default::default()
        },
        prepare: |_| {},
        root: onboarding::root,
        // Its close button: the same as finish_onboarding(None).
        on_close: OnClose::Close(|| {
            let app = AppHandle;
            release_onboarding(&app);
            show_main_after_onboarding(&app, None);
        }),
    }
}

/// Show the first-run onboarding window (docs/plans/ONBOARDING.md), building
/// it the first time, and hide the main window behind it. A new window picks
/// up the step it was on with `resume` (the launch after a permission's
/// quit), else starts over.
fn open_onboarding(app: AppHandle, resume: bool) -> Result<(), String> {
    host::with_app(move |cx| {
        if app.get_window(ONBOARDING_WINDOW_LABEL).is_none() {
            onboarding::start_over(!resume);
        }
        let window = match windows::open(cx, onboarding_window_spec(cx)) {
            Ok(window) => window,
            Err(e) => {
                eprintln!("Failed to open onboarding: {e}");
                return;
            }
        };
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(main) = app.get_window(MAIN_WINDOW_LABEL) {
            let _ = main.hide();
        }
    });
    Ok(())
}

/// Close the onboarding window and bring the main window back, on `show`
/// (a route to open) if given.
fn finish_onboarding(app: AppHandle, show: Option<String>) {
    if let Some(window) = app.get_window(ONBOARDING_WINDOW_LABEL) {
        // Not a close: that would come back through its close handler.
        let _ = window.destroy();
    }
    release_onboarding(&app);
    show_main_after_onboarding(&app, show);
}

/// Give back what the onboarding window held: the microphone test and
/// practice mode. Runs whenever the window goes away. The keys and the
/// dictation gate go back to the main window's settings
/// (`shell::resume_after_onboarding`).
fn release_onboarding(app: &AppHandle) {
    dictation::mic::stop_preview(&app.state::<dictation::DictationState>());
    if let Some(mode) = app.try_state::<hotkey_monitor::ChordMode>() {
        mode.end_practice();
    }
}

/// Show and focus the main window, on `show` if given, with the keys
/// following its settings again.
fn show_main_after_onboarding(app: &AppHandle, show: Option<String>) {
    if let Some(main) = app.get_window(MAIN_WINDOW_LABEL) {
        let _ = main.show();
        let _ = main.set_focus();
    }
    host::with_app(move |cx| {
        shell::resume_after_onboarding(cx);
        if let Some(route) = show {
            let (path, search) = match route.split_once('?') {
                Some((path, search)) => (path.to_string(), format!("?{search}")),
                None => (route, String::new()),
            };
            shell::navigate(&path, &search, cx);
        }
    });
}

/// Whether the onboarding window exists.
fn onboarding_window_open(app: AppHandle) -> bool {
    app.get_window(ONBOARDING_WINDOW_LABEL).is_some()
}

/// Center the pill above the usable screen edge of the display the user is
/// working on, leaving room for the Dock.
pub(crate) fn position_dictate_window(window: &host::AppWindow) -> host::Result<()> {
    // Located here, off the main thread: the Accessibility lookup can be slow.
    let target = [
        focus_capture::focused_window_center,
        focus_capture::cursor_location,
    ]
    .iter()
    .filter_map(|locate| locate())
    .collect::<Vec<_>>();
    window.with_ns_window(move |ns_window| {
        let Some(area) = dictate_work_area(&target) else {
            return;
        };
        let mut frame = ns_window.frame();
        // The pill's size, even if AppKit hasn't laid the panel out yet.
        if frame.size.width < 1.0 {
            frame.size = NSSize::new(DICTATE_WINDOW_WIDTH, DICTATE_WINDOW_HEIGHT);
        }
        frame.origin.x = area.origin.x + (area.size.width - frame.size.width).max(0.0) / 2.0;
        // AppKit's y grows upward: the pill's bottom edge sits above the
        // work area's bottom edge.
        frame.origin.y = area.origin.y + DICTATE_BOTTOM_PADDING;
        ns_window.setFrame_display(frame, true);
    })
}

/// The work area (`visibleFrame`) of the display with the first of
/// `points` (top-left global coordinates), else of the main display.
fn dictate_work_area(points: &[(f64, f64)]) -> Option<NSRect> {
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    // Screen frames are bottom-left based on the first screen; the points
    // are top-left based, as CoreGraphics reports them.
    let primary_height = screens.firstObject()?.frame().size.height;
    let bounds: Vec<_> = screens
        .iter()
        .map(|screen| {
            let f = screen.frame();
            (
                f.origin.x,
                primary_height - f.origin.y - f.size.height,
                f.size.width,
                f.size.height,
            )
        })
        .collect();
    let screen = points
        .iter()
        .find_map(|&point| display_containing(&bounds, point))
        .and_then(|index| screens.iter().nth(index))
        .or_else(|| NSScreen::mainScreen(mtm))?;
    Some(screen.visibleFrame())
}

/// Make room above the pill for the style chip, or give it back. The bottom
/// edge stays put, so the pill (drawn at the window's bottom) doesn't move:
/// AppKit frames grow up from their origin, and one `setFrame:` changes the
/// height without a frame drawn in between.
fn set_dictate_chip_space(window: &host::AppWindow, show: bool) {
    let height = DICTATE_WINDOW_HEIGHT + if show { DICTATE_CHIP_SPACE } else { 0.0 };
    let _ = window.with_ns_window(move |ns_window| {
        let mut frame = ns_window.frame();
        if frame.size.height != height {
            frame.size.height = height;
            ns_window.setFrame_display(frame, true);
        }
    });
}

/// Called by the pill around the style chip's animation.
pub(crate) fn dictate_chip_space(app: AppHandle, show: bool) {
    if let Some(window) = app.get_window(DICTATE_WINDOW_LABEL) {
        set_dictate_chip_space(&window, show);
    }
}

/// Index of the display whose `(x, y, width, height)` bounds hold `point`.
fn display_containing(bounds: &[(f64, f64, f64, f64)], point: (f64, f64)) -> Option<usize> {
    let (px, py) = point;
    bounds
        .iter()
        .position(|&(x, y, w, h)| px >= x && px < x + w && py >= y && py < y + h)
}

/// `canBecomeKeyWindow` and `canBecomeMainWindow` for the pill. The pill
/// must never take keyboard focus: it floats over the app being dictated
/// into, and while it is key that app's window looks focused but every
/// keystroke (the Enter after a paste, say) lands in the pill instead. Audio
/// capture is native, so nothing in the pill needs key status.
extern "C-unwind" fn pill_never_key(_this: &AnyObject, _sel: Sel) -> Bool {
    Bool::NO
}

/// Swap the pill's class for a subclass of it that never becomes key. GPUI's
/// pop-up is already a non-activating NSPanel (the only kind of window macOS
/// lets into another app's fullscreen Space), but it can become key. Its
/// subclass adds no instance variables, so swapping the live object's class
/// is safe.
fn make_never_key(window: &NSWindow) {
    let current: &AnyClass = window.class();
    let name = format!("KassPill_{}", current.name().to_string_lossy());
    let Ok(name) = std::ffi::CString::new(name) else {
        return;
    };
    let class = match AnyClass::get(&name) {
        Some(class) => class,
        None => {
            let Some(mut builder) = ClassBuilder::new(&name, current) else {
                return;
            };
            // SAFETY: both methods match the selectors' `BOOL (id, SEL)`.
            unsafe {
                builder.add_method(
                    sel!(canBecomeKeyWindow),
                    pill_never_key as extern "C-unwind" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(canBecomeMainWindow),
                    pill_never_key as extern "C-unwind" fn(_, _) -> _,
                );
            }
            builder.register()
        }
    };
    // SAFETY: `class` subclasses the window's own class and adds no ivars.
    unsafe {
        objc2::ffi::object_setClass(
            window as *const NSWindow as *mut AnyObject,
            class as *const AnyClass,
        );
    }
}

/// Set the pill up as Tauri's build had it, before its view attaches:
/// never key, borderless, no shadow, over every Space and fullscreen app,
/// and click-through until a take shows it.
fn prepare_pill(window: &NSWindow) {
    // NSWindowStyleMask: borderless | nonactivating panel.
    const NONACTIVATING_PANEL: usize = 1 << 7;
    // NSWindowCollectionBehavior: can join all spaces | stationary |
    // fullscreen auxiliary.
    const ON_EVERY_SPACE: usize = 1 | (1 << 4) | (1 << 8);
    // NSScreenSaverWindowLevel: above fullscreen app content.
    const OVERLAY_WINDOW_LEVEL: isize = 1000;

    // SAFETY: standard NSWindow/NSPanel setters, on the main thread.
    unsafe {
        let _: () = msg_send![window, setStyleMask: NONACTIVATING_PANEL];
        let _: () = msg_send![window, setHidesOnDeactivate: false];
        let _: () = msg_send![window, setBecomesKeyOnlyIfNeeded: true];
        let _: () = msg_send![window, setFloatingPanel: true];
        let behavior: usize = msg_send![window, collectionBehavior];
        let _: () = msg_send![window, setCollectionBehavior: behavior | ON_EVERY_SPACE];
        let _: () = msg_send![window, setLevel: OVERLAY_WINDOW_LEVEL];
        let _: () = msg_send![window, setHasShadow: false];
        let _: () = msg_send![window, setIgnoresMouseEvents: true];
    }
    // Swap the class only after the style mask change. Dropping the title bar
    // makes its view remove a KVO observer from the window, and AppKit can't
    // find that registration once the class has changed: it throws, and at
    // startup that aborts the app.
    make_never_key(window);
    window.setFrame_display(
        NSRect::new(
            NSPoint::new(-10_000.0, -10_000.0),
            NSSize::new(DICTATE_WINDOW_WIDTH, DICTATE_WINDOW_HEIGHT),
        ),
        false,
    );
}

/// Show the pill over whatever Space is active without taking keyboard focus.
/// This replaces `window.show()`, which calls `makeKeyAndOrderFront:`.
/// `orderFrontRegardless` works even though the app is inactive (it always is
/// mid-dictation — the user is typing in some other app).
pub fn force_order_front(window: &host::AppWindow) {
    let _ = window.with_ns_window(|ns_window| ns_window.orderFrontRegardless());
}

/// Park the pill: off-screen, click-through and hidden. Hiding alone has
/// been unreliable for transparent always-on-top windows on macOS: the
/// window could linger as an invisible click target that steals focus to
/// Kass when the user clicks where it used to be.
pub(crate) fn park_dictate_window(window: &host::AppWindow) {
    // A chip cut off by the end of the take gives its room back.
    set_dictate_chip_space(window, false);
    let _ = window.set_ignore_cursor_events(true);
    let _ = window.with_ns_window(|ns_window| {
        ns_window.setFrameOrigin(NSPoint::new(-10_000.0, -10_000.0));
        ns_window.orderOut(None);
    });
}

/// Build the pill if it doesn't exist yet. Idempotent.
pub fn ensure_dictate_window() {
    host::with_app(|cx| {
        if let Err(e) = windows::open(cx, dictate_window_spec()) {
            eprintln!("ensure_dictate_window: failed to build pill: {e}");
        }
    });
}

pub(crate) const SERVER_PORT: u16 = 17493;
/// The Python model worker's own port, on this Mac only. Kass's API on
/// `SERVER_PORT` (the `kass-server` crate) passes requests on to it.
pub(crate) const WORKER_PORT: u16 = 17494;

/// Check if a Kass server is responding on the given port.
///
/// Sends an HTTP GET to `/health` and returns `true` only if the response
/// is valid JSON with `status == "healthy"`, which filters out unrelated
/// services that answer `/health` with something else.
fn check_health(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/health", port);
    match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(client) => match client.get(&url).send() {
            Ok(resp) => {
                if !resp.status().is_success() {
                    return false;
                }
                // Parse as JSON and validate Kass-specific fields
                match resp.json::<serde_json::Value>() {
                    Ok(body) => body.get("status").and_then(|v| v.as_str()) == Some("healthy"),
                    Err(_) => false,
                }
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}

struct ServerState {
    child: Mutex<Option<sidecar::Child>>,
    server_pid: Mutex<Option<u32>>,
    models_dir: Mutex<Option<String>>,
    /// Beta `network`: the other Mac's server this one stands in for while
    /// it's away (backend/services/sync), as of the last start.
    shared_server: Mutex<Option<String>>,
    /// Kass's API on `SERVER_PORT`, in front of the worker.
    api: Mutex<Option<Api>>,
}

struct Api {
    running: kass_server::Running,
    /// Listening for other Macs too, not just this one.
    shared: bool,
    /// The other Mac's server it stood in for when it started.
    shared_server: Option<String>,
    /// The models folder it downloads to, as the worker was given it.
    models_dir: Option<String>,
}

impl Api {
    fn serves(
        &self,
        share: bool,
        shared_server: &Option<String>,
        models_dir: &Option<String>,
    ) -> bool {
        self.shared == share
            && self.shared_server == *shared_server
            && self.models_dir == *models_dir
    }
}

/// Whether the server should listen for other Macs on the network: asked
/// for, on the beta channel, and not using another Mac's (src/prefs.rs).
fn shares_on_network(app: &AppHandle) -> bool {
    prefs::get().shares(updater::beta_features(app))
}

/// Beta `network`: the other Mac's server this Mac's stands in for.
fn shared_server(app: &AppHandle) -> Option<String> {
    prefs::get()
        .remote(updater::beta_features(app))
        .map(str::to_string)
}

/// This Mac's address for other Macs on the network, like
/// `http://Studio.local:17493`.
fn network_address() -> Result<String, String> {
    let output = std::process::Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .map_err(|e| e.to_string())?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || name.is_empty() {
        return Err("This Mac has no local network name".into());
    }
    Ok(format!("http://{name}.local:{SERVER_PORT}"))
}

async fn start_server(
    app: AppHandle,
    state: State<'_, ServerState>,
    models_dir: Option<String>,
) -> Result<String, String> {
    *state.shared_server.lock().unwrap() = shared_server(&app);
    // Store models_dir for use on restart (empty string means reset to default)
    if let Some(ref dir) = models_dir {
        if dir.is_empty() {
            *state.models_dir.lock().unwrap() = None;
        } else {
            *state.models_dir.lock().unwrap() = Some(dir.clone());
        }
    }
    let share = shares_on_network(&app);
    if state.api.lock().unwrap().is_none() {
        free_api_port().await?;
    }
    // The worker only uses the tables kass.db already has.
    let database = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?
        .join("kass.db");
    tokio::task::spawn_blocking(move || {
        kass_server::db::prepare(&database)?;
        kass_server::db::seed(&database)?;
        // Before the worker reads the trained models in use (model_improvement),
        // and with the pipeline id worked out off the async runtime.
        kass_server::model_improvement::pipeline();
        kass_server::model_improvement::restamp_legacy(&database);
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    start_worker(&app, &state, models_dir).await?;
    serve_api(&app, &state, share).await?;
    Ok(format!("http://127.0.0.1:{}", SERVER_PORT))
}

/// Start the Python model worker on `WORKER_PORT`, or reuse one already
/// there, and return once it answers.
async fn start_worker(
    app: &AppHandle,
    state: &ServerState,
    models_dir: Option<String>,
) -> Result<(), String> {
    // Check if the worker is already running (managed by this app instance)
    if state.child.lock().unwrap().is_some() {
        return Ok(());
    }

    // Check if a kass worker is already running on its port (e.g. one left
    // over from a previous session, or started by hand via `python`/`uvicorn`).
    // It's reused only when it's this version's; after an update, a server
    // left over from the old version is stopped and a new one started.
    let app_version = app.package_info().version.to_string();
    {
        use std::process::Command;
        if let Ok(output) = Command::new("lsof")
            .args(["-i", &format!(":{}", WORKER_PORT), "-sTCP:LISTEN"])
            .output()
        {
            let output_str = String::from_utf8_lossy(&output.stdout);
            for line in output_str.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let command = parts[0];
                    let pid_str = parts[1];
                    if command.contains("kass") {
                        if let Ok(pid) = pid_str.parse::<u32>() {
                            let running = server_version::running(WORKER_PORT);
                            if !server_version::is_current(running.as_deref(), &app_version) {
                                println!(
                                    "Found kass-server {} on port {} (PID: {}), but this is {}; replacing it",
                                    running.as_deref().unwrap_or("of unknown version"),
                                    WORKER_PORT,
                                    pid,
                                    app_version
                                );
                                server_process::stop(pid)?;
                                wait_for_port_free(WORKER_PORT).await?;
                                break;
                            }
                            println!(
                                "Found existing kass-server on port {} (PID: {}), reusing it",
                                WORKER_PORT, pid
                            );
                            // Store the PID so we can kill it on exit if needed
                            *state.server_pid.lock().unwrap() = Some(pid);
                            return Ok(());
                        }
                    } else {
                        // Process name doesn't contain "kass" — could be an external
                        // Python/uvicorn/Docker server. Verify via HTTP health check.
                        println!(
                            "Port {} in use by '{}' (PID: {}), checking if it's a Kass server...",
                            WORKER_PORT, command, pid_str
                        );
                        if check_health(WORKER_PORT) {
                            let running = server_version::running(WORKER_PORT);
                            // A release build replaces an old server, such as
                            // one still named voicebox-server. A dev build keeps
                            // the server started by hand, whatever its version.
                            if !cfg!(debug_assertions)
                                && !server_version::is_current(running.as_deref(), &app_version)
                            {
                                if let Ok(pid) = pid_str.parse::<u32>() {
                                    println!(
                                        "Server on port {} is {}, but this is {}; replacing it",
                                        WORKER_PORT,
                                        running.as_deref().unwrap_or("of unknown version"),
                                        app_version
                                    );
                                    server_process::stop(pid)?;
                                    wait_for_port_free(WORKER_PORT).await?;
                                    break;
                                }
                            }
                            println!(
                                "Health check passed — reusing external server on port {}",
                                WORKER_PORT
                            );
                            return Ok(());
                        }
                        println!("Health check failed — port is occupied by a non-Kass process");
                        return Err(format!(
                            "Port {} is already in use by another application ({}). \
                             Close it or change the Kass server port.",
                            WORKER_PORT, command
                        ));
                    }
                }
            }
        }
    }

    // Get app data directory
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    // Ensure data directory exists
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data dir: {}", e))?;

    println!("=================================================================");
    println!("Starting kass-server sidecar");
    println!("Data directory: {:?}", data_dir);

    let sidecar_result = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())
        .and_then(|dir| server_process::bundled_executable(&dir));

    let mut sidecar = match sidecar_result {
        Ok(path) => sidecar::Command::new(path),
        Err(e) => {
            eprintln!("Failed to get sidecar: {}", e);

            // In dev mode, check if the server is already running (started manually)
            #[cfg(debug_assertions)]
            {
                eprintln!(
                    "Dev mode: Checking if server is already running on port {}...",
                    WORKER_PORT
                );

                // Try to connect to the server port
                use std::net::TcpStream;
                if TcpStream::connect_timeout(
                    &format!("127.0.0.1:{}", WORKER_PORT).parse().unwrap(),
                    std::time::Duration::from_secs(1),
                )
                .is_ok()
                {
                    println!("Found server already running on port {}", WORKER_PORT);
                    return Ok(());
                }

                eprintln!();
                eprintln!("=================================================================");
                eprintln!("DEV MODE: No server found on port {}", WORKER_PORT);
                eprintln!();
                eprintln!("Start the Python server in a separate terminal:");
                eprintln!("  cargo xtask dev-backend");
                eprintln!("=================================================================");
                eprintln!();
            }

            return Err("Failed to start server. In dev mode, run 'cargo xtask dev-backend' in a separate terminal.".to_string());
        }
    };

    println!("Sidecar command created successfully");

    // Build common args
    let data_dir_str = data_dir
        .to_str()
        .ok_or_else(|| "Invalid data dir path".to_string())?
        .to_string();
    let port_str = WORKER_PORT.to_string();
    let parent_pid_str = std::process::id().to_string();

    // Resolve the custom models directory from the parameter or stored state
    let effective_models_dir = models_dir.or_else(|| state.models_dir.lock().unwrap().clone());
    if let Some(ref dir) = effective_models_dir {
        println!("Custom models directory: {}", dir);
    }

    sidecar = sidecar.args([
        "--data-dir",
        &data_dir_str,
        "--host",
        "127.0.0.1",
        "--port",
        &port_str,
        "--parent-pid",
        &parent_pid_str,
    ]);
    if let Some(ref dir) = effective_models_dir {
        sidecar = sidecar.env("KASS_MODELS_DIR", dir);
    }
    // The worker drops a cleanup adapter tested with another cleanup pipeline.
    sidecar = sidecar.env(
        "KASS_CLEANUP_PIPELINE",
        kass_server::model_improvement::pipeline(),
    );
    if let Some(url) = state.shared_server.lock().unwrap().clone() {
        sidecar = sidecar.env("KASS_SHARED_SERVER", url);
    }
    println!("Spawning bundled server process...");
    let spawn_result = sidecar.spawn();

    let (mut rx, child) = match spawn_result {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Failed to spawn server process: {}", e);

            // In dev mode, check if a manually-started server is available
            #[cfg(debug_assertions)]
            {
                use std::net::TcpStream;
                if TcpStream::connect_timeout(
                    &format!("127.0.0.1:{}", WORKER_PORT).parse().unwrap(),
                    std::time::Duration::from_secs(1),
                )
                .is_ok()
                {
                    println!("Found manually-started server on port {}", WORKER_PORT);
                    return Ok(());
                }

                eprintln!();
                eprintln!("=================================================================");
                eprintln!("DEV MODE: Server binary failed to start");
                eprintln!();
                eprintln!("Start the Python server in a separate terminal:");
                eprintln!("  cargo xtask dev-backend");
                eprintln!("=================================================================");
                eprintln!();
                return Err(
                    "Dev mode: Start server manually with 'cargo xtask dev-backend'".to_string(),
                );
            }

            #[cfg(not(debug_assertions))]
            {
                eprintln!("This could be due to:");
                eprintln!("  - Missing or corrupted binary");
                eprintln!("  - Missing execute permissions");
                eprintln!("  - Code signing issues on macOS");
                eprintln!("  - Missing dependencies");
                return Err(format!("Failed to spawn: {}", e));
            }
        }
    };

    println!("Server process spawned, waiting for ready signal...");
    println!("=================================================================");

    // Store child process and PID
    let process_pid = child.pid();
    *state.server_pid.lock().unwrap() = Some(process_pid);
    *state.child.lock().unwrap() = Some(child);

    // Wait for server to be ready by listening for startup log
    // PyInstaller bundles can be slow on first import, especially torch/transformers
    // Startup now loads the installed dictation models before serving requests.
    let timeout = tokio::time::Duration::from_secs(600);
    let start_time = tokio::time::Instant::now();
    let mut error_output = Vec::new();

    loop {
        if start_time.elapsed() > timeout {
            eprintln!("Server startup timeout after 600 seconds");
            if !error_output.is_empty() {
                eprintln!("Collected error output:");
                for line in &error_output {
                    eprintln!("  {}", line);
                }
            }

            // In dev mode, check if a manual server came up during the wait
            #[cfg(debug_assertions)]
            {
                use std::net::TcpStream;
                if TcpStream::connect_timeout(
                    &format!("127.0.0.1:{}", WORKER_PORT).parse().unwrap(),
                    std::time::Duration::from_secs(1),
                )
                .is_ok()
                {
                    // Kill the placeholder process
                    let _ = state.child.lock().unwrap().take();
                    println!("Found manually-started server on port {}", WORKER_PORT);
                    return Ok(());
                }
            }

            return Err("Server startup timeout - check Console.app for detailed logs".to_string());
        }

        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                match event {
                    sidecar::CommandEvent::Stdout(line) => {
                        let line_str = String::from_utf8_lossy(&line);
                        println!("Server output: {}", line_str);
                        let _ = app.emit(
                            "server-log",
                            serde_json::json!({
                                "stream": "stdout",
                                "line": line_str.trim_end(),
                            }),
                        );

                        if line_str.contains("Uvicorn running")
                            || line_str.contains("Application startup complete")
                        {
                            println!("Server is ready!");
                            break;
                        }
                    }
                    sidecar::CommandEvent::Stderr(line) => {
                        let line_str = String::from_utf8_lossy(&line).to_string();
                        eprintln!("Server: {}", line_str);
                        let _ = app.emit(
                            "server-log",
                            serde_json::json!({
                                "stream": "stderr",
                                "line": line_str.trim_end(),
                            }),
                        );

                        // Collect error lines for debugging
                        if line_str.contains("ERROR")
                            || line_str.contains("Error")
                            || line_str.contains("Failed")
                        {
                            error_output.push(line_str.clone());
                        }

                        // Uvicorn logs to stderr, so check there too
                        if line_str.contains("Uvicorn running")
                            || line_str.contains("Application startup complete")
                        {
                            println!("Server is ready!");
                            break;
                        }
                    }
                    // The channel closes right after; that ends the wait.
                    sidecar::CommandEvent::Terminated => {}
                }
            }
            Ok(None) => {
                // In dev mode, this is expected when using the placeholder binary
                #[cfg(debug_assertions)]
                {
                    use std::net::TcpStream;
                    eprintln!("Server process ended (dev mode placeholder detected)");

                    // Check if a manually-started server is available
                    if TcpStream::connect_timeout(
                        &format!("127.0.0.1:{}", WORKER_PORT).parse().unwrap(),
                        std::time::Duration::from_secs(1),
                    )
                    .is_ok()
                    {
                        // Clean up state
                        let _ = state.child.lock().unwrap().take();
                        let _ = state.server_pid.lock().unwrap().take();
                        println!("Found manually-started server on port {}", WORKER_PORT);
                        return Ok(());
                    }

                    eprintln!();
                    eprintln!("=================================================================");
                    eprintln!("DEV MODE: No bundled server binary available");
                    eprintln!();
                    eprintln!("Start the Python server in a separate terminal:");
                    eprintln!("  cargo xtask dev-backend");
                    eprintln!("=================================================================");
                    eprintln!();
                    return Err(
                        "Dev mode: Start server manually with 'cargo xtask dev-backend'"
                            .to_string(),
                    );
                }

                #[cfg(not(debug_assertions))]
                {
                    eprintln!("Server process ended unexpectedly during startup!");
                    eprintln!("The server binary may have crashed or exited with an error.");
                    eprintln!("Check Console.app logs for more details (search for 'kass')");
                    return Err("Server process ended unexpectedly".to_string());
                }
            }
            Err(_) => {
                // Timeout on this recv, continue loop
                continue;
            }
        }
    }

    // Spawn task to continue reading output and emit to frontend
    let app_handle = app.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                sidecar::CommandEvent::Stdout(line) => {
                    let line_str = String::from_utf8_lossy(&line);
                    println!("Server: {}", line_str);
                    let _ = app_handle.emit(
                        "server-log",
                        serde_json::json!({
                            "stream": "stdout",
                            "line": line_str.trim_end(),
                        }),
                    );
                }
                sidecar::CommandEvent::Stderr(line) => {
                    let line_str = String::from_utf8_lossy(&line);
                    eprintln!("Server error: {}", line_str);
                    let _ = app_handle.emit(
                        "server-log",
                        serde_json::json!({
                            "stream": "stderr",
                            "line": line_str.trim_end(),
                        }),
                    );
                }
                // The API goes with its worker, unless a restart has
                // already put another worker in its place.
                sidecar::CommandEvent::Terminated => {
                    let state = app_handle.state::<ServerState>();
                    if *state.server_pid.lock().unwrap() == Some(process_pid) {
                        state.api.lock().unwrap().take();
                    }
                }
            }
        }
    });

    Ok(())
}

/// Before this app serves `SERVER_PORT` itself, stop a server an older
/// version of Kass left there, when all of Kass ran in Python.
async fn free_api_port() -> Result<(), String> {
    use std::process::Command;
    let Ok(output) = Command::new("lsof")
        .args(["-i", &format!(":{}", SERVER_PORT), "-sTCP:LISTEN"])
        .output()
    else {
        return Ok(());
    };
    let output = String::from_utf8_lossy(&output.stdout);
    for line in output.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (Some(command), Some(Ok(pid))) =
            (parts.first(), parts.get(1).map(|p| p.parse::<u32>()))
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        // The app itself is `kass`; its Python server was `kass-server`.
        if *command == "kass" {
            return Err(format!(
                "Port {SERVER_PORT} is in use by another copy of Kass (PID {pid}). Quit it first."
            ));
        }
        if !command.contains("kass") {
            if !check_health(SERVER_PORT) {
                return Err(format!(
                    "Port {} is already in use by another application ({}). \
                     Close it or change the Kass server port.",
                    SERVER_PORT, command
                ));
            }
            if cfg!(debug_assertions) {
                return Err(format!(
                    "A Kass server started by hand is on port {SERVER_PORT} (PID {pid}). \
                     The app serves that port itself now: stop it and run the Python \
                     worker on {WORKER_PORT} instead (cargo xtask dev-backend)."
                ));
            }
        }
        println!("Found an older Kass server on port {SERVER_PORT} (PID: {pid}); replacing it");
        server_process::stop(pid)?;
        wait_for_port_free(SERVER_PORT).await?;
        break;
    }
    Ok(())
}

/// Silero's voice detector, as the worker's bundle carries it, or the repo's
/// copy in dev.
fn voice_model(app: &AppHandle) -> Option<std::path::PathBuf> {
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join("kass-server/_internal/backend/assets/silero_vad.onnx"));
    let repo =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../backend/assets/silero_vad.onnx");
    bundled
        .into_iter()
        .chain(cfg!(debug_assertions).then_some(repo))
        .find(|path| path.is_file())
}

/// Serve Kass's API on `SERVER_PORT`, in front of the worker: to this Mac
/// only, or to other Macs on the network too while `share` is on. One
/// already listening for the right Macs, standing in for the same server,
/// keeps running.
async fn serve_api(app: &AppHandle, state: &ServerState, share: bool) -> Result<(), String> {
    let shared_server = state.shared_server.lock().unwrap().clone();
    let models_dir = state
        .models_dir
        .lock()
        .unwrap()
        .clone()
        .filter(|dir| !dir.is_empty());
    let previous = {
        let mut api = state.api.lock().unwrap();
        if api
            .as_ref()
            .is_some_and(|api| api.serves(share, &shared_server, &models_dir))
        {
            return Ok(());
        }
        api.take()
    };
    if let Some(previous) = previous {
        previous.running.stop();
        wait_for_port_free(SERVER_PORT).await?;
    }
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let host = if share {
        std::net::Ipv4Addr::UNSPECIFIED
    } else {
        std::net::Ipv4Addr::LOCALHOST
    };
    let started = kass_server::start(kass_server::Config {
        listen: (host, SERVER_PORT).into(),
        worker: (std::net::Ipv4Addr::LOCALHOST, WORKER_PORT).into(),
        database: data_dir.join("kass.db"),
        shared_server: shared_server.clone(),
        voice_model: voice_model(app),
        models_dir: models_dir.as_deref().map(std::path::PathBuf::from),
        app_version: env!("CARGO_PKG_VERSION").into(),
    })
    .await;
    let mut api = state.api.lock().unwrap();
    match started {
        Ok(running) => {
            println!("Kass API listening on {}", running.address());
            *api = Some(Api {
                running,
                shared: share,
                shared_server,
                models_dir: models_dir.clone(),
            });
            Ok(())
        }
        // Another start got there first.
        Err(_)
            if api
                .as_ref()
                .is_some_and(|api| api.serves(share, &shared_server, &models_dir)) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn stop_server(state: State<'_, ServerState>) -> Result<(), String> {
    stop_managed_server(&state)
}

fn stop_managed_server(state: &ServerState) -> Result<(), String> {
    state.api.lock().unwrap().take();
    let mut pid = state.server_pid.lock().unwrap();
    if let Some(value) = *pid {
        server_process::stop(value)?;
        *pid = None;
        state.child.lock().unwrap().take();
    }
    Ok(())
}

async fn wait_for_server_exit() -> Result<(), String> {
    wait_for_port_free(SERVER_PORT).await?;
    wait_for_port_free(WORKER_PORT).await
}

async fn wait_for_port_free(port: u16) -> Result<(), String> {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(
        "The local server is still running. Restart was cancelled; no second server was launched."
            .into(),
    )
}

async fn restart_app(state: State<'_, ServerState>) -> Result<(), String> {
    stop_server(state.clone()).await?;
    wait_for_server_exit().await?;
    host::with_app(|cx| cx.restart());
    Ok(())
}

/// Move the app into `/Applications` and relaunch it from there. Stops the
/// server first so the new copy can start its own on the same port.
async fn move_to_applications(state: State<'_, ServerState>) -> Result<(), String> {
    let installed = app_location::copy_into_applications()?;
    stop_server(state.clone()).await?;
    wait_for_server_exit().await?;
    app_location::relaunch_after_exit(std::process::id(), &installed)?;
    host::with_app(|cx| cx.quit());
    Ok(())
}

async fn restart_server(
    app: AppHandle,
    state: State<'_, ServerState>,
    models_dir: Option<String>,
) -> Result<String, String> {
    println!("restart_server: stopping current server...");

    // Update stored models_dir: empty string means reset to default, non-empty means set
    if let Some(ref dir) = models_dir {
        if dir.is_empty() {
            *state.models_dir.lock().unwrap() = None;
        } else {
            *state.models_dir.lock().unwrap() = Some(dir.clone());
        }
    }

    // Stop the current server
    stop_server(state.clone()).await?;

    // Wait for port to be released
    println!("restart_server: waiting for port release...");
    wait_for_server_exit().await?;

    // Start server again (uses the stored models_dir)
    println!("restart_server: starting server...");
    start_server(app, state.clone(), None).await
}

/// Identifier of the Kass app itself — used to short-circuit auto-paste
/// when the user fires a chord while focus was inside one of our own
/// windows. Dictation into Kass goes to the focused Kass window's own
/// field instead (`dictation::insert_in_app`).
///
/// Value matches the reverse-DNS bundle id `focus_capture::capture_focus`
/// writes into `FocusSnapshot::bundle_id`, and `BUNDLE_ID` in crates/xtask, which
/// the bundle's Info.plist gets.
const KASS_BUNDLE_ID: &str = "com.mrgnhnt.kass";

/// Reports whether the process currently has macOS Accessibility trust.
/// Used by the settings UI and the paste debug harness to decide whether
/// synthetic key events will actually land.
fn check_accessibility_permission() -> bool {
    accessibility::is_trusted()
}

/// Reports whether the process can observe global keyboard events. Read by
/// the Captures settings UI to surface a "missing — open Settings" hint
/// beside the hotkey toggle. No prompt side-effect.
fn check_input_monitoring_permission() -> bool {
    input_monitoring::is_trusted()
}

/// Holds the lazily-spawned global hotkey monitor. The monitor is `None`
/// until the user opts in via the Captures settings toggle — that opt-in is
/// what triggers the macOS Input Monitoring TCC prompt, so a fresh-install
/// user who never enables the hotkey never sees the prompt.
///
/// Disabling the hotkey clears the monitor's internal `ChordMatcher` so
/// keytap's event tap is released while the app still holds this `HotkeyState`
/// for the rest of the process. A subsequent enable re-arms without
/// re-prompting for the Input Monitoring permission.
#[derive(Default)]
pub struct HotkeyState {
    monitor: Mutex<Option<hotkey_monitor::HotkeyMonitor>>,
}

fn build_chord_bindings(
    push_to_talk: &[String],
    toggle_to_talk: &[String],
    command: &[String],
) -> Result<hotkey_monitor::Bindings, String> {
    use hotkey_monitor::{Bindings, ChordAction};
    use keytap::Key;
    use std::collections::HashSet;

    fn build_chord(name: &str, names: &[String]) -> Result<HashSet<Key>, String> {
        if names.is_empty() {
            return Err(format!("{name} chord must have at least one key"));
        }
        let mut chord = HashSet::new();
        for raw in names {
            let key = key_codes::key_from_str(raw)
                .ok_or_else(|| format!("Unsupported key in {name} chord: {raw}"))?;
            chord.insert(key);
        }
        Ok(chord)
    }

    let push_chord = build_chord("push-to-talk", push_to_talk)?;
    let toggle_chord = build_chord("toggle-to-talk", toggle_to_talk)?;

    let mut bindings = Bindings::new();
    bindings.insert(ChordAction::PushToTalk, push_chord);
    bindings.insert(ChordAction::ToggleToTalk, toggle_chord);
    // Command Mode's chord is optional: empty turns it off.
    if !command.is_empty() {
        bindings.insert(ChordAction::Command, build_chord("command", command)?);
    }
    Ok(bindings)
}

/// Spawn the global hotkey monitor on first call; subsequent calls just push
/// the new bindings into the existing monitor. Idempotent on purpose — the
/// frontend invokes this both at startup (when `capture_settings.hotkey_enabled`
/// is true) and from the settings toggle.
///
/// On macOS this is the call that triggers the "Kass would like to receive
/// keystrokes from any application" TCC prompt, since keytap's `Tap` creates
/// the CGEventTap inside `HotkeyMonitor::spawn`.
fn enable_hotkey(
    app: AppHandle,
    state: State<'_, HotkeyState>,
    push_to_talk: Vec<String>,
    toggle_to_talk: Vec<String>,
    command: Option<Vec<String>>,
) -> Result<(), String> {
    let bindings =
        build_chord_bindings(&push_to_talk, &toggle_to_talk, &command.unwrap_or_default())?;

    // Fire the Input Monitoring TCC prompt explicitly from the user's
    // toggle click, before keytap's Tap would do it implicitly via
    // CGEventTap creation. Two reasons: (1) the prompt timing becomes
    // deterministic — it appears in response to a click instead of as a
    // mysterious side-effect of "the app started"; (2) on subsequent
    // launches we can short-circuit the spawn entirely if the user
    // revoked the grant, instead of relying on the tap silently failing.
    // The call returns the current grant state; we ignore it because
    // keytap surfaces its own error via stderr, and the settings UI
    // polls `check_input_monitoring_permission` separately.
    let _ = input_monitoring::request();

    // The dictate pill must exist before the first chord fires so it hears
    // the take's first state. Build it here (idempotent: the existing window
    // is kept when one with this label already exists).
    if app.get_window(DICTATE_WINDOW_LABEL).is_none() {
        ensure_dictate_window();
    }

    let mut slot = state.monitor.lock().map_err(|e| e.to_string())?;
    match slot.as_mut() {
        Some(monitor) => monitor.update_bindings(bindings),
        None => {
            *slot = Some(hotkey_monitor::HotkeyMonitor::spawn(app, bindings));
        }
    }
    Ok(())
}

/// Quiet the global hotkey. Tears down the `ChordMatcher` (which stops
/// keytap's chord worker and closes the OS event tap) but keeps the
/// `HotkeyMonitor` handle around so a subsequent `enable_hotkey` re-arms
/// without re-prompting for Input Monitoring permission.
fn disable_hotkey(state: State<'_, HotkeyState>) -> Result<(), String> {
    let mut slot = state.monitor.lock().map_err(|e| e.to_string())?;
    if let Some(monitor) = slot.as_mut() {
        monitor.update_bindings(hotkey_monitor::Bindings::new());
    }
    Ok(())
}

/// Open the Privacy & Security → Accessibility pane in System Settings so
/// the user can grant the permission. The URL scheme is stable across
/// macOS 10.14–15.
///
/// The first time, only asks: the request is what lists Kass in the pane,
/// and macOS's prompt has its own button to open it. Opening the pane too
/// covered the prompt and showed a list without Kass in it.
fn open_accessibility_settings(app: AppHandle) -> Result<(), String> {
    if !accessibility::was_asked(&app) {
        accessibility::remember_asked(&app);
        if !accessibility::request() {
            return Ok(());
        }
    }
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    host::open_external(url).map_err(|e| format!("Failed to open Accessibility settings: {e}"))?;
    Ok(())
}

/// Open the Privacy & Security → Input Monitoring pane in System Settings.
/// Used by the Captures settings UI when the toggle is on but the grant
/// is missing, so the user can flip the system toggle without hunting.
///
/// When Kass isn't listed yet, only asks: the request is what lists it, and
/// macOS's prompt has its own button to open the pane.
fn open_input_monitoring_settings() -> Result<(), String> {
    if !input_monitoring::is_listed() {
        let _ = input_monitoring::request();
        return Ok(());
    }
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";
    host::open_external(url)
        .map_err(|e| format!("Failed to open Input Monitoring settings: {e}"))?;
    Ok(())
}

/// [`paste_final_text`] with a clipboard snapshot taken earlier, at dictation
/// key-down, reused when nothing was copied since.
pub(crate) async fn paste_final_text_with(
    text: String,
    focus: focus_capture::FocusSnapshot,
    prepared: Option<clipboard::ClipboardSnapshot>,
) -> Result<bool, String> {
    paste_final_text_tracked(text, focus, prepared, false)
        .await
        .0
}

/// [`paste_final_text_with`], and with `track` what is known of the text in
/// the field, for a voice edit (docs/plans/VOICE_EDITS.md) or a correction
/// saved in Captures (docs/plans/CORRECTIONS_IN_PLACE.md).
pub(crate) async fn paste_final_text_tracked(
    text: String,
    focus: focus_capture::FocusSnapshot,
    prepared: Option<clipboard::ClipboardSnapshot>,
    track: bool,
) -> (Result<bool, String>, Tracked) {
    if focus.bundle_id.as_deref() == Some(KASS_BUNDLE_ID) {
        return (Ok(false), Tracked::Untracked);
    }
    if !accessibility::is_trusted() {
        return (Err(ACCESSIBILITY_REQUIRED.into()), Tracked::Untracked);
    }

    // Only re-activate the target when the user actually left it. When it is
    // still frontmost (the common case — the dictate pill is non-activating,
    // and in a fullscreen Space the target never loses frontmost), activation
    // is a no-op; on macOS 26 fullscreen Spaces `activate` returns NO for an
    // already-frontmost app, which would otherwise abort the paste entirely.
    let already_front = focus_capture::frontmost_pid() == Some(focus.pid);
    let pid = focus.pid;
    let bundle_id = focus.bundle_id.clone();
    let role = focus.role.clone();
    let inserted = tokio::task::spawn_blocking(move || {
        run_insert_chain(
            pid,
            bundle_id.as_deref(),
            role.as_deref(),
            &text,
            prepared,
            track,
        )
    })
    .await
    .map_err(|e| {
        // A panic mid-step: we cannot know whether anything was inserted.
        format!(
            "Text insertion stopped unexpectedly ({e}). It was not pasted again; \
             copy it from Captures if it is missing."
        )
    });
    let (report, tracked) = match inserted {
        Ok(inserted) => inserted,
        Err(message) => return (Err(message), Tracked::Untracked),
    };

    let app = focus.bundle_id.as_deref().unwrap_or("unknown app");
    eprintln!("[kass] insert into {app}: {}", report.summary());
    let result = match report.delivery() {
        insert_chain::Delivery::Inserted { method, .. } => {
            // Accessibility writes without activating; bring the user back
            // to the app they dictated into, as the other steps do.
            if method == insert_chain::Method::Accessibility && !already_front {
                let _ = focus_capture::activate_pid(focus.pid);
            }
            Ok(true)
        }
        insert_chain::Delivery::Uncertain { message, .. } => Err(message),
        insert_chain::Delivery::Exhausted => {
            Err("Could not insert the dictated text into this app. Copy it from Captures.".into())
        }
    };
    (result, tracked)
}

const ACCESSIBILITY_REQUIRED: &str = "Accessibility permission required for auto-paste. Open System Settings → Privacy & Security → Accessibility and enable Kass.";

/// Paste the clipboard as it is (every format, not just text) into the
/// target focused at chord start: the "paste from clipboard" command.
pub(crate) async fn paste_clipboard_into(
    focus: focus_capture::FocusSnapshot,
) -> Result<bool, String> {
    if !accessibility::is_trusted() {
        return Err(ACCESSIBILITY_REQUIRED.into());
    }
    let pid = focus.pid;
    tokio::task::spawn_blocking(move || {
        clipboard::restore_pending();
        if focus_capture::frontmost_pid() != Some(pid) {
            focus_capture::activate_pid(pid)?;
            std::thread::sleep(POST_ACTIVATE_SETTLE);
        }
        synthetic_keys::send_paste()?;
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Insert `parts` with the clipboard pasted as it is between each: for an
/// image or file named mid-dictation. Each ⌘V is given time to land before
/// the next words go in, which may not wait on the event queue.
pub(crate) async fn paste_around_clipboard(
    parts: Vec<String>,
    focus: focus_capture::FocusSnapshot,
) -> Result<bool, String> {
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            paste_clipboard_into(focus.clone()).await?;
            tokio::time::sleep(clipboard::PASTE_CONSUME).await;
        }
        let part = part.trim();
        if !part.is_empty() && !paste_final_text_with(part.to_string(), focus.clone(), None).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Settle time after activating the target, so AppKit finishes re-ordering
/// windows and restoring its last-focused field before keys arrive.
const POST_ACTIVATE_SETTLE: std::time::Duration = std::time::Duration::from_millis(120);

/// Make `pid` frontmost, for steps that send key events. Blocking.
fn bring_to_front(pid: i32) -> Result<(), String> {
    if focus_capture::frontmost_pid() == Some(pid) {
        return Ok(());
    }
    focus_capture::activate_pid(pid)?;
    std::thread::sleep(POST_ACTIVATE_SETTLE);
    Ok(())
}

/// The insertion chain for a target app, and with `track` the text as
/// owned where the Accessibility step wrote it. Blocking.
fn run_insert_chain(
    pid: i32,
    bundle_id: Option<&str>,
    role: Option<&str>,
    text: &str,
    prepared: Option<clipboard::ClipboardSnapshot>,
    track: bool,
) -> (insert_chain::Report, Tracked) {
    let bring_front = || bring_to_front(pid);
    let in_front = |inner| insert_chain::InFront {
        inner,
        bring_front: &bring_front,
    };
    // Every method inserts the same text, fitted to what is around the caret
    // and without a repeat of the words after it
    // (docs/plans/MID_SENTENCE_DICTATION.md).
    let text = &text_insert::fit_to_focused(pid, bundle_id, text);
    let keys = keystroke_insert::Keystrokes::new();
    let paste = clipboard::Paste::new(prepared);
    let (keys, paste) = (in_front(&keys), in_front(&paste));
    // Fastest first. In TextEdit a verified Accessibility write lands in
    // 3-6 ms, typing ~6 ms plus 2.7 ms per keystroke, and a paste ~20 ms
    // (`insert_bench.rs`).
    let chain: [&dyn insert_chain::Inserter; 3] = [&text_insert::Accessibility, &keys, &paste];
    let report = insert_chain::deliver(
        &chain,
        &insert_chain::Request {
            pid,
            bundle_id,
            role,
            text,
        },
    );
    // Only where Accessibility wrote it can an edit be written and checked
    // the same way (docs/plans/VOICE_EDITS.md). Keys and ⌘V land a moment
    // later; their text is read back off this path, for a correction saved
    // in Captures (docs/plans/CORRECTIONS_IN_PLACE.md).
    let tracked = match report.delivery() {
        _ if !track => Tracked::Untracked,
        insert_chain::Delivery::Inserted {
            method: insert_chain::Method::Accessibility,
            ..
        } => text_insert::owned_before_focused(pid, bundle_id, text)
            .map_or(Tracked::Untracked, Tracked::Owned),
        insert_chain::Delivery::Inserted { .. } => Tracked::Typed(text.to_string()),
        _ => Tracked::Untracked,
    };
    (report, tracked)
}

/// What Kass knows of the text a take put in the field.
pub(crate) enum Tracked {
    Untracked,
    /// Written over Accessibility, and read back.
    Owned(text_insert::Owned),
    /// This text, as fitted, went in by keys or ⌘V.
    Typed(String),
}

/// Type or paste `text` over the selection in `pid`'s focused field, for a
/// voice edit where Accessibility can't write it. True when it was sent; the
/// edit reads the field back. Blocking.
pub(crate) fn type_over_selection(
    pid: i32,
    bundle_id: Option<&str>,
    role: Option<&str>,
    text: &str,
) -> bool {
    let bring_front = || bring_to_front(pid);
    let in_front = |inner| insert_chain::InFront {
        inner,
        bring_front: &bring_front,
    };
    let keys = keystroke_insert::Keystrokes::new();
    let paste = clipboard::Paste::new(None);
    let (keys, paste) = (in_front(&keys), in_front(&paste));
    let chain: [&dyn insert_chain::Inserter; 2] = [&keys, &paste];
    let report = insert_chain::deliver(
        &chain,
        &insert_chain::Request {
            pid,
            bundle_id,
            role,
            text,
        },
    );
    let app = bundle_id.unwrap_or("unknown app");
    eprintln!("[kass] voice edit typed into {app}: {}", report.summary());
    matches!(report.delivery(), insert_chain::Delivery::Inserted { .. })
}

pub fn run() {
    app_location::carry_over_old_bundle();
    identifier_move::move_from_old_identifier(KASS_BUNDLE_ID);
    prefs::adopt_old_page();
    let handle = AppHandle;
    handle.manage(updater::UpdaterState::default());
    handle.manage(ServerState {
        child: Mutex::new(None),
        server_pid: Mutex::new(None),
        models_dir: Mutex::new(None),
        shared_server: Mutex::new(None),
        api: Mutex::new(None),
    });
    handle.manage(dictation::DictationState::default());
    handle.manage(deep_link::DeepLinkState::default());
    handle.manage(hotkey_monitor::ChordMode::default());
    // HotkeyMonitor is spawned lazily via the `enable_hotkey` command — see
    // HotkeyState.
    handle.manage(HotkeyState::default());

    let application = gpui_kit::application().with_assets(kass_ui::Assets);
    application.on_open_urls(|urls| {
        for url in urls {
            deep_link::open(&AppHandle, &url);
        }
    });
    // A Dock click with no window showing brings Kass back. Onboarding
    // stands in for the main window while it's open.
    application.on_reopen(|_| {
        let app = AppHandle;
        let any_visible = app
            .windows()
            .values()
            .any(|window| window.is_visible().unwrap_or(false));
        if any_visible {
            return;
        }
        let window = app
            .get_window(ONBOARDING_WINDOW_LABEL)
            .or_else(|| app.get_window(MAIN_WINDOW_LABEL));
        if let Some(window) = window {
            let _ = window.show();
            let _ = window.set_focus();
        }
    });
    application.run(|cx| {
        host::install(cx);
        gpui_kit::init(cx);
        kass_ui::init(cx);
        shell::init(cx);
        cx.on_app_quit(|_| {
            let app = AppHandle;
            let state = app.state::<ServerState>();
            if let Err(error) = stop_managed_server(&state) {
                eprintln!("Failed to stop local server on exit: {error}");
            }
            // A downloaded update goes in as Kass quits, with the server
            // stopped, so the next launch is the new version.
            if let Err(error) = updater::install_pending(&app) {
                eprintln!("Failed to install the downloaded update: {error}");
            }
            async {}
        })
        .detach();

        match windows::open(cx, main_window_spec(cx)) {
            // A login launch stays out of the way until the Dock icon is
            // clicked.
            Ok(main) if !login_item::launched_at_login() => {
                let _ = main.show();
                let _ = main.set_focus();
            }
            Ok(_) => {}
            Err(e) => eprintln!("Failed to open the main window: {e}"),
        }
        // Built hidden now, so it's ready before the first chord. It
        // creates no keyboard tap and asks for no permission.
        if let Err(e) = windows::open(cx, dictate_window_spec()) {
            eprintln!("Failed to build the dictate pill: {e}");
        }

        let app = AppHandle;
        login_item::register_by_default(&app);
        this_mac::init(&app);
        dictation::restore(&app);
        sound_cues::init(&app);
        updater::start(app.clone());
        // Resolve the active keyboard layout's V keycode now, on the main
        // thread, and register an observer for layout changes. The
        // synthetic-paste hot path then only reads an atomic. See
        // keyboard_layout.rs for why this matters (Cmd+V is matched by
        // translated character, not keycode, so QWERTY keycode 9 produces
        // Cmd+. on Dvorak).
        keyboard_layout::init();
        // ⌘H hides the windows, so the pill can show without them.
        app_hide::init();
        // Apps that show a text field when dictation starts
        // (docs/DICTATION_HANDSHAKE.md) register and reply here.
        dictation_handshake::listen();
    });
}

fn main() {
    run();
}

#[cfg(test)]
mod tests {
    use super::display_containing;

    #[test]
    fn picks_the_display_holding_the_point() {
        // Main 1512x982 display, with a 2560x1440 display to its left and
        // raised above it, the way macOS lays out an external monitor.
        let bounds = [(0.0, 0.0, 1512.0, 982.0), (-2560.0, -300.0, 2560.0, 1440.0)];
        assert_eq!(display_containing(&bounds, (700.0, 500.0)), Some(0));
        assert_eq!(display_containing(&bounds, (-1280.0, 0.0)), Some(1));
        assert_eq!(display_containing(&bounds, (-1.0, -299.0)), Some(1));
        assert_eq!(display_containing(&bounds, (1512.0, 10.0)), None);
    }
}
