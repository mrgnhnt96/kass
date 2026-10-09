mod accessibility;
mod app_hide;
mod app_icon;
mod app_location;
mod clipboard;
mod deep_link;
#[cfg(desktop)]
mod dictation;
#[cfg(desktop)]
mod dictation_handshake;
mod focus_capture;
#[cfg(desktop)]
mod hotkey_monitor;
mod identifier_move;
mod input_monitoring;
#[cfg(test)]
mod insert_bench;
mod insert_chain;
mod join;
#[cfg(desktop)]
mod key_codes;
mod keyboard_layout;
mod keystroke_insert;
mod login_item;
mod overlap;
mod report;
mod server_process;
mod server_version;
mod sound_cues;
mod synthetic_keys;
mod text_insert;
mod updater;

use std::sync::Mutex;
use tauri::{
    command, Emitter, Listener, Manager, PhysicalPosition, RunEvent, State, WebviewUrl,
    WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_shell::ShellExt;

pub const DICTATE_WINDOW_LABEL: &str = "dictate";
const MAIN_WINDOW_LABEL: &str = "main";
pub const ONBOARDING_WINDOW_LABEL: &str = "onboarding";
const ONBOARDING_WINDOW_WIDTH: f64 = 880.0;
const ONBOARDING_WINDOW_HEIGHT: f64 = 600.0;
const DICTATE_WINDOW_WIDTH: f64 = 420.0;
const DICTATE_WINDOW_HEIGHT: f64 = 64.0;
const DICTATE_BOTTOM_PADDING: f64 = 24.0;
/// Room above the pill for the style chip (22 pt, its 8 pt gap, its glow and
/// its drift). Only added while the chip shows: the window takes every click
/// over it, so the rest of the time it stays the pill's size.
const DICTATE_CHIP_SPACE: f64 = 40.0;

/// Create the floating dictate webview hidden. The HotkeyMonitor shows it on
/// chord-start; the frontend hides it when the capture pipeline finishes.
/// Building it at setup avoids a race where the first chord event fires
/// before the webview subscribes to the `dictate:*` events.
#[cfg(desktop)]
fn build_dictate_window(app: &tauri::AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let window = WebviewWindowBuilder::new(
        app,
        DICTATE_WINDOW_LABEL,
        WebviewUrl::App("?view=dictate".into()),
    )
    .title("Kass Dictate")
    .inner_size(DICTATE_WINDOW_WIDTH, DICTATE_WINDOW_HEIGHT)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    // Follow the user across macOS Spaces / virtual desktops instead of
    // being pinned to the Space where the window was first created.
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    // The pill never becomes key (see `pill_panel_can_become_key`), so every
    // click on it is a first click; without this WebKit would swallow it.
    .accept_first_mouse(true)
    .visible(false)
    .build()?;

    position_dictate_window(&window)?;

    // Make the pill able to float over other apps' native fullscreen Spaces.
    apply_fullscreen_overlay_behavior(&window);

    Ok(window)
}

/// Show the first-run onboarding window (docs/plans/ONBOARDING.md), building
/// it the first time, and hide the main window behind it.
#[cfg(desktop)]
#[command]
fn open_onboarding(app: tauri::AppHandle) -> Result<(), String> {
    let window = match app.get_webview_window(ONBOARDING_WINDOW_LABEL) {
        Some(window) => window,
        None => build_onboarding_window(&app).map_err(|e| e.to_string())?,
    };
    let _ = window.show();
    let _ = window.set_focus();
    if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main.hide();
    }
    Ok(())
}

#[cfg(desktop)]
fn build_onboarding_window(app: &tauri::AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let builder = WebviewWindowBuilder::new(
        app,
        ONBOARDING_WINDOW_LABEL,
        WebviewUrl::App("?view=onboarding".into()),
    )
    .title("Kass")
    .inner_size(ONBOARDING_WINDOW_WIDTH, ONBOARDING_WINDOW_HEIGHT)
    .resizable(false)
    .center();
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    builder.build()
}

/// Close the onboarding window and bring the main window back, telling it
/// with `onboarding:finished { show }` (a route to open, or null).
#[cfg(desktop)]
#[command]
fn finish_onboarding(app: tauri::AppHandle, show: Option<String>) {
    if let Some(window) = app.get_webview_window(ONBOARDING_WINDOW_LABEL) {
        // Not `close()`: that would come back through CloseRequested.
        let _ = window.destroy();
    }
    release_onboarding(&app);
    show_main_after_onboarding(&app, show);
}

/// Give back what the onboarding window held: the microphone test and
/// practice mode. Its React cleanup may never run, so this runs whenever the
/// window goes away. The dictation gate is left to the main window, which
/// sets it again after `onboarding:finished`.
#[cfg(desktop)]
fn release_onboarding(app: &tauri::AppHandle) {
    dictation::mic::stop_preview(&app.state::<dictation::DictationState>());
    if let Some(mode) = app.try_state::<hotkey_monitor::ChordMode>() {
        mode.end_practice();
    }
}

/// Show and focus the main window and send it `onboarding:finished`.
#[cfg(desktop)]
fn show_main_after_onboarding(app: &tauri::AppHandle, show: Option<String>) {
    if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main.show();
        let _ = main.set_focus();
    }
    let payload = serde_json::json!({ "show": show });
    if let Err(e) = app.emit_to(MAIN_WINDOW_LABEL, "onboarding:finished", payload) {
        eprintln!("Failed to emit onboarding:finished: {e}");
    }
}

/// Whether the onboarding window exists.
#[command]
fn onboarding_window_open(app: tauri::AppHandle) -> bool {
    app.get_webview_window(ONBOARDING_WINDOW_LABEL).is_some()
}

/// Center the pill above the usable screen edge of the display the user is
/// working on, leaving room for the Dock.
#[cfg(desktop)]
pub(crate) fn position_dictate_window(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let Some(monitor) = dictate_monitor(window)? else {
        return Ok(());
    };
    // Work in points: tao reports each display in its own scale, so mixed-DPI
    // setups have no single physical space to place the pill in.
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let size = window
        .outer_size()?
        .to_logical::<f64>(window.scale_factor()?);
    let x = area.position.x as f64 / scale
        + ((area.size.width as f64 / scale - size.width).max(0.0) / 2.0);
    let y = area.position.y as f64 / scale
        + (area.size.height as f64 / scale - size.height - DICTATE_BOTTOM_PADDING).max(0.0);
    window.set_position(tauri::LogicalPosition::new(x, y))
}

/// Make room above the pill for the style chip, or give it back. The bottom
/// edge stays put, so the pill (drawn at the window's bottom) doesn't move:
/// AppKit frames grow up from their origin, and one `setFrame:` changes the
/// height without a frame drawn in between.
#[cfg(desktop)]
fn set_dictate_chip_space(window: &tauri::WebviewWindow, show: bool) {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }
    let height = DICTATE_WINDOW_HEIGHT + if show { DICTATE_CHIP_SPACE } else { 0.0 };
    let w = window.clone();
    let _ = window.run_on_main_thread(move || {
        use objc::runtime::{Object, YES};
        use objc::{msg_send, sel, sel_impl};
        let Ok(ptr) = w.ns_window() else { return };
        let ns_window = ptr as *mut Object;
        if ns_window.is_null() {
            return;
        }
        // SAFETY: a valid NSWindow owned by Tauri, on the main thread.
        unsafe {
            let mut frame: NSRect = msg_send![ns_window, frame];
            if frame.height != height {
                frame.height = height;
                let _: () = msg_send![ns_window, setFrame: frame display: YES];
            }
        }
    });
}

/// Called by the HUD around the style chip's animation.
#[cfg(desktop)]
#[command]
fn dictate_chip_space(app: tauri::AppHandle, show: bool) {
    if let Some(window) = app.get_webview_window(DICTATE_WINDOW_LABEL) {
        set_dictate_chip_space(&window, show);
    }
}

/// The display with the focused window, else the one under the cursor, else
/// the main display. Not `current_monitor()`: the hide path parks the pill
/// off-screen, where it has no monitor.
#[cfg(desktop)]
fn dictate_monitor(window: &tauri::WebviewWindow) -> tauri::Result<Option<tauri::Monitor>> {
    let monitors = window.available_monitors()?;
    let bounds: Vec<_> = monitors
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            let position = m.position();
            let size = m.size();
            (
                position.x as f64 / scale,
                position.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
            )
        })
        .collect();
    let target = [
        focus_capture::focused_window_center,
        focus_capture::cursor_location,
    ]
    .iter()
    .filter_map(|locate| locate())
    .find_map(|point| display_containing(&bounds, point));
    match target {
        Some(index) => Ok(monitors.into_iter().nth(index)),
        None => window.primary_monitor(),
    }
}

/// Index of the display whose `(x, y, width, height)` bounds hold `point`.
fn display_containing(bounds: &[(f64, f64, f64, f64)], point: (f64, f64)) -> Option<usize> {
    let (px, py) = point;
    bounds
        .iter()
        .position(|&(x, y, w, h)| px >= x && px < x + w && py >= y && py < y + h)
}

// `object_setClass` — reclass a live object. Not re-exported by `objc`.
extern "C" {
    fn object_setClass(
        obj: *mut objc::runtime::Object,
        cls: *const objc::runtime::Class,
    ) -> *const objc::runtime::Class;
}

/// `canBecomeKeyWindow` override for the pill panel. The pill must never take
/// keyboard focus: it floats over the app being dictated into, and while it is
/// key that app's window looks focused but every keystroke (the Enter after a
/// paste, say) lands in the pill instead. Audio capture is native, so nothing
/// in the webview needs key status.
extern "C" fn pill_panel_can_become_key(
    _this: &objc::runtime::Object,
    _sel: objc::runtime::Sel,
) -> objc::runtime::BOOL {
    objc::runtime::NO
}

/// Lazily-registered NSPanel subclass for the dictate pill: never key (so it
/// never takes keystrokes) while remaining a panel (for fullscreen-Space join).
fn pill_panel_class() -> &'static objc::runtime::Class {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel, BOOL};
    use objc::{class, sel, sel_impl};
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let superclass = class!(NSPanel);
        let mut decl = ClassDecl::new("KassPillPanel", superclass).expect("register KassPillPanel");
        unsafe {
            decl.add_method(
                sel!(canBecomeKeyWindow),
                pill_panel_can_become_key as extern "C" fn(&Object, Sel) -> BOOL,
            );
        }
        decl.register();
    });
    Class::get("KassPillPanel").expect("KassPillPanel registered")
}

/// Convert the dictate pill's NSWindow into a never-key NSPanel and set the
/// collection behavior + window level required to appear over another app's
/// native macOS fullscreen Space.
///
/// A regular (Dock-icon) app's plain NSWindow is never admitted to a foreign
/// fullscreen Space regardless of collection-behavior flags or window level;
/// an NSPanel with the same flags is. NSPanel adds no instance variables over
/// NSWindow, so re-classing the live object is safe (the tauri-nspanel plugin
/// uses the same technique). Idempotent via an `isKindOfClass` guard. Runs on
/// the main thread because AppKit window mutation is main-thread-only.
pub fn apply_fullscreen_overlay_behavior(window: &tauri::WebviewWindow) {
    let w = window.clone();
    let dispatched = window.run_on_main_thread(move || {
        use objc::runtime::{Object, NO, YES};
        use objc::{class, msg_send, sel, sel_impl};

        // NSWindowCollectionBehavior bit flags.
        const CAN_JOIN_ALL_SPACES: u64 = 1 << 0;
        const STATIONARY: u64 = 1 << 4;
        const FULL_SCREEN_AUXILIARY: u64 = 1 << 8; // the flag stock Tauri never sets
        const NONACTIVATING_PANEL: u64 = 1 << 7; // NSWindowStyleMaskNonactivatingPanel
                                                 // NSScreenSaverWindowLevel — floats above fullscreen app content.
        const OVERLAY_WINDOW_LEVEL: i64 = 1000;

        let ns_window = match w.ns_window() {
            Ok(ptr) => ptr as *mut Object,
            Err(e) => {
                eprintln!("apply_fullscreen_overlay_behavior: ns_window() failed: {e}");
                return;
            }
        };
        if ns_window.is_null() {
            return;
        }
        // SAFETY: ns_window is a valid, non-null NSWindow* owned by Tauri for
        // the lifetime of the webview window; all selectors are standard AppKit
        // calls and we are on the main thread.
        unsafe {
            let is_panel: objc::runtime::BOOL =
                msg_send![ns_window, isKindOfClass: class!(NSPanel)];
            if is_panel == NO {
                object_setClass(ns_window, pill_panel_class());
                let style: u64 = msg_send![ns_window, styleMask];
                let _: () = msg_send![ns_window, setStyleMask: style | NONACTIVATING_PANEL];
                let _: () = msg_send![ns_window, setHidesOnDeactivate: NO];
                let _: () = msg_send![ns_window, setBecomesKeyOnlyIfNeeded: YES];
                let _: () = msg_send![ns_window, setFloatingPanel: YES];
            }
            // Preserve behavior bits installed by Tauri/Tao instead of
            // replacing them wholesale when adding the fullscreen flags.
            let current_behavior: u64 = msg_send![ns_window, collectionBehavior];
            let behavior =
                current_behavior | CAN_JOIN_ALL_SPACES | FULL_SCREEN_AUXILIARY | STATIONARY;
            let _: () = msg_send![ns_window, setCollectionBehavior: behavior];
            let _: () = msg_send![ns_window, setLevel: OVERLAY_WINDOW_LEVEL];
        }
    });
    if let Err(e) = dispatched {
        eprintln!("apply_fullscreen_overlay_behavior: main-thread dispatch failed: {e}");
    }
}

/// Show the pill over whatever Space is active without taking keyboard focus.
/// This replaces `window.show()`, which calls `makeKeyAndOrderFront:`.
/// `orderFrontRegardless` works even though the app is inactive (it always is
/// mid-dictation — the user is typing in some other app).
pub fn force_order_front(window: &tauri::WebviewWindow) {
    let w = window.clone();
    let _ = window.run_on_main_thread(move || {
        use objc::runtime::Object;
        use objc::{msg_send, sel, sel_impl};
        if let Ok(ptr) = w.ns_window() {
            let ns_window = ptr as *mut Object;
            if !ns_window.is_null() {
                unsafe {
                    let _: () = msg_send![ns_window, orderFrontRegardless];
                }
            }
        }
    });
}

/// Build the pill webview if it doesn't exist yet. Idempotent — called at
/// setup so the pill's listeners are registered before the first chord.
#[cfg(desktop)]
pub fn ensure_dictate_window(app: &tauri::AppHandle) {
    if app.get_webview_window(DICTATE_WINDOW_LABEL).is_none() {
        if let Err(e) = build_dictate_window(app) {
            eprintln!("ensure_dictate_window: failed to build pill: {e}");
        }
    }
}

pub(crate) const SERVER_PORT: u16 = 17493;

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
    child: Mutex<Option<tauri_plugin_shell::process::CommandChild>>,
    server_pid: Mutex<Option<u32>>,
    models_dir: Mutex<Option<String>>,
}

#[command]
async fn start_server(
    app: tauri::AppHandle,
    state: State<'_, ServerState>,
    models_dir: Option<String>,
) -> Result<String, String> {
    // Store models_dir for use on restart (empty string means reset to default)
    if let Some(ref dir) = models_dir {
        if dir.is_empty() {
            *state.models_dir.lock().unwrap() = None;
        } else {
            *state.models_dir.lock().unwrap() = Some(dir.clone());
        }
    }
    // Check if server is already running (managed by this app instance)
    if state.child.lock().unwrap().is_some() {
        return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
    }

    // Check if a kass server is already running on our port (e.g. one left
    // over from a previous session, or started by hand via `python`/`uvicorn`).
    // It's reused only when it's this version's; after an update, a server
    // left over from the old version is stopped and a new one started.
    let app_version = app.package_info().version.to_string();
    {
        use std::process::Command;
        if let Ok(output) = Command::new("lsof")
            .args(["-i", &format!(":{}", SERVER_PORT), "-sTCP:LISTEN"])
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
                            let running = server_version::running(SERVER_PORT);
                            if !server_version::is_current(running.as_deref(), &app_version) {
                                println!(
                                    "Found kass-server {} on port {} (PID: {}), but this is {}; replacing it",
                                    running.as_deref().unwrap_or("of unknown version"),
                                    SERVER_PORT,
                                    pid,
                                    app_version
                                );
                                server_process::stop(pid)?;
                                wait_for_server_exit().await?;
                                break;
                            }
                            println!(
                                "Found existing kass-server on port {} (PID: {}), reusing it",
                                SERVER_PORT, pid
                            );
                            // Store the PID so we can kill it on exit if needed
                            *state.server_pid.lock().unwrap() = Some(pid);
                            return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
                        }
                    } else {
                        // Process name doesn't contain "kass" — could be an external
                        // Python/uvicorn/Docker server. Verify via HTTP health check.
                        println!(
                            "Port {} in use by '{}' (PID: {}), checking if it's a Kass server...",
                            SERVER_PORT, command, pid_str
                        );
                        if check_health(SERVER_PORT) {
                            let running = server_version::running(SERVER_PORT);
                            // A release build replaces an old server, such as
                            // one still named voicebox-server. A dev build keeps
                            // the server started by hand, whatever its version.
                            if !cfg!(debug_assertions)
                                && !server_version::is_current(running.as_deref(), &app_version)
                            {
                                if let Ok(pid) = pid_str.parse::<u32>() {
                                    println!(
                                        "Server on port {} is {}, but this is {}; replacing it",
                                        SERVER_PORT,
                                        running.as_deref().unwrap_or("of unknown version"),
                                        app_version
                                    );
                                    server_process::stop(pid)?;
                                    wait_for_server_exit().await?;
                                    break;
                                }
                            }
                            println!(
                                "Health check passed — reusing external server on port {}",
                                SERVER_PORT
                            );
                            return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
                        }
                        println!("Health check failed — port is occupied by a non-Kass process");
                        return Err(format!(
                            "Port {} is already in use by another application ({}). \
                             Close it or change the Kass server port.",
                            SERVER_PORT, command
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
        Ok(path) => app.shell().command(path),
        Err(e) => {
            eprintln!("Failed to get sidecar: {}", e);

            // In dev mode, check if the server is already running (started manually)
            #[cfg(debug_assertions)]
            {
                eprintln!(
                    "Dev mode: Checking if server is already running on port {}...",
                    SERVER_PORT
                );

                // Try to connect to the server port
                use std::net::TcpStream;
                if TcpStream::connect_timeout(
                    &format!("127.0.0.1:{}", SERVER_PORT).parse().unwrap(),
                    std::time::Duration::from_secs(1),
                )
                .is_ok()
                {
                    println!("Found server already running on port {}", SERVER_PORT);
                    return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
                }

                eprintln!();
                eprintln!("=================================================================");
                eprintln!("DEV MODE: No server found on port {}", SERVER_PORT);
                eprintln!();
                eprintln!("Start the Python server in a separate terminal:");
                eprintln!("  bun run dev:server");
                eprintln!("=================================================================");
                eprintln!();
            }

            return Err("Failed to start server. In dev mode, run 'bun run dev:server' in a separate terminal.".to_string());
        }
    };

    println!("Sidecar command created successfully");

    // Build common args
    let data_dir_str = data_dir
        .to_str()
        .ok_or_else(|| "Invalid data dir path".to_string())?
        .to_string();
    let port_str = SERVER_PORT.to_string();
    let parent_pid_str = std::process::id().to_string();

    // Resolve the custom models directory from the parameter or stored state
    let effective_models_dir = models_dir.or_else(|| state.models_dir.lock().unwrap().clone());
    if let Some(ref dir) = effective_models_dir {
        println!("Custom models directory: {}", dir);
    }

    sidecar = sidecar.args([
        "--data-dir",
        &data_dir_str,
        "--port",
        &port_str,
        "--parent-pid",
        &parent_pid_str,
    ]);
    if let Some(ref dir) = effective_models_dir {
        sidecar = sidecar.env("KASS_MODELS_DIR", dir);
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
                    &format!("127.0.0.1:{}", SERVER_PORT).parse().unwrap(),
                    std::time::Duration::from_secs(1),
                )
                .is_ok()
                {
                    println!("Found manually-started server on port {}", SERVER_PORT);
                    return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
                }

                eprintln!();
                eprintln!("=================================================================");
                eprintln!("DEV MODE: Server binary failed to start");
                eprintln!();
                eprintln!("Start the Python server in a separate terminal:");
                eprintln!("  bun run dev:server");
                eprintln!("=================================================================");
                eprintln!();
                return Err("Dev mode: Start server manually with 'bun run dev:server'".to_string());
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
                    &format!("127.0.0.1:{}", SERVER_PORT).parse().unwrap(),
                    std::time::Duration::from_secs(1),
                )
                .is_ok()
                {
                    // Kill the placeholder process
                    let _ = state.child.lock().unwrap().take();
                    println!("Found manually-started server on port {}", SERVER_PORT);
                    return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
                }
            }

            return Err("Server startup timeout - check Console.app for detailed logs".to_string());
        }

        match tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => {
                match event {
                    tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
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
                    tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
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
                    _ => {}
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
                        &format!("127.0.0.1:{}", SERVER_PORT).parse().unwrap(),
                        std::time::Duration::from_secs(1),
                    )
                    .is_ok()
                    {
                        // Clean up state
                        let _ = state.child.lock().unwrap().take();
                        let _ = state.server_pid.lock().unwrap().take();
                        println!("Found manually-started server on port {}", SERVER_PORT);
                        return Ok(format!("http://127.0.0.1:{}", SERVER_PORT));
                    }

                    eprintln!();
                    eprintln!("=================================================================");
                    eprintln!("DEV MODE: No bundled server binary available");
                    eprintln!();
                    eprintln!("Start the Python server in a separate terminal:");
                    eprintln!("  bun run dev:server");
                    eprintln!("=================================================================");
                    eprintln!();
                    return Err(
                        "Dev mode: Start server manually with 'bun run dev:server'".to_string()
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
                tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
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
                tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
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
                _ => {}
            }
        }
    });

    Ok(format!("http://127.0.0.1:{}", SERVER_PORT))
}

#[command]
async fn stop_server(state: State<'_, ServerState>) -> Result<(), String> {
    stop_managed_server(&state)
}

fn stop_managed_server(state: &ServerState) -> Result<(), String> {
    let mut pid = state.server_pid.lock().unwrap();
    if let Some(value) = *pid {
        server_process::stop(value)?;
        *pid = None;
        state.child.lock().unwrap().take();
    }
    Ok(())
}

async fn wait_for_server_exit() -> Result<(), String> {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", SERVER_PORT))
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

#[command]
async fn restart_app(app: tauri::AppHandle, state: State<'_, ServerState>) -> Result<(), String> {
    stop_server(state.clone()).await?;
    wait_for_server_exit().await?;
    app.request_restart();
    Ok(())
}

/// Where the app runs from, so the UI can ask to move it into Applications.
#[command]
fn app_location() -> app_location::AppLocation {
    app_location::location()
}

/// Move the app into `/Applications` and relaunch it from there. Stops the
/// server first so the new copy can start its own on the same port.
#[command]
async fn move_to_applications(
    app: tauri::AppHandle,
    state: State<'_, ServerState>,
) -> Result<(), String> {
    let installed = app_location::copy_into_applications()?;
    stop_server(state.clone()).await?;
    wait_for_server_exit().await?;
    app_location::relaunch_after_exit(std::process::id(), &installed)?;
    app.exit(0);
    Ok(())
}

#[command]
async fn restart_server(
    app: tauri::AppHandle,
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
/// windows. Dictation into Kass goes through the main window's DOM
/// instead (`dictation::insert_in_app`).
///
/// Value matches the reverse-DNS bundle id `focus_capture::capture_focus`
/// writes into `FocusSnapshot::bundle_id`, and `identifier` in tauri.conf.json.
const KASS_BUNDLE_ID: &str = "com.mrgnhnt.kass";

/// The icon of the app with `bundle_id` as a PNG data URL, for Captures.
#[command]
fn app_icon(bundle_id: String) -> Option<String> {
    app_icon::icon_data_url(&bundle_id)
}

/// Reports whether the process currently has macOS Accessibility trust.
/// Used by the settings UI and the paste debug harness to decide whether
/// synthetic key events will actually land.
#[command]
fn check_accessibility_permission() -> bool {
    accessibility::is_trusted()
}

/// Reports whether the process can observe global keyboard events. Read by
/// the Captures settings UI to surface a "missing — open Settings" hint
/// beside the hotkey toggle. No prompt side-effect.
#[command]
fn check_input_monitoring_permission() -> bool {
    input_monitoring::is_trusted()
}

/// Holds the lazily-spawned global hotkey monitor. The monitor is `None`
/// until the user opts in via the Captures settings toggle — that opt-in is
/// what triggers the macOS Input Monitoring TCC prompt, so a fresh-install
/// user who never enables the hotkey never sees the prompt.
///
/// Disabling the hotkey clears the monitor's internal `ChordMatcher` so
/// keytap's event tap is released while Tauri still owns this `HotkeyState`
/// for the rest of the process. A subsequent enable re-arms without
/// re-prompting for the Input Monitoring permission.
#[cfg(desktop)]
#[derive(Default)]
pub struct HotkeyState {
    monitor: Mutex<Option<hotkey_monitor::HotkeyMonitor>>,
}

#[cfg(desktop)]
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
#[cfg(desktop)]
#[command]
fn enable_hotkey(
    app: tauri::AppHandle,
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

    // The dictate pill webview must exist before the first chord fires so it
    // can subscribe to `dictate:start`. Build it here (idempotent — Tauri
    // returns the existing window when one with this label already exists).
    if app.get_webview_window(DICTATE_WINDOW_LABEL).is_none() {
        if let Err(e) = build_dictate_window(&app) {
            eprintln!("Failed to build dictate window: {}", e);
        }
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
#[cfg(desktop)]
#[command]
fn disable_hotkey(state: State<'_, HotkeyState>) -> Result<(), String> {
    let mut slot = state.monitor.lock().map_err(|e| e.to_string())?;
    if let Some(monitor) = slot.as_mut() {
        monitor.update_bindings(hotkey_monitor::Bindings::new());
    }
    Ok(())
}

/// Push a new chord configuration into the running `HotkeyMonitor`. Called
/// by the chord-picker UI when the user edits the chord. No-ops when the
/// monitor isn't spawned — the picker is gated behind the enable toggle, so
/// this can only happen if the frontend races; the next `enable_hotkey` will
/// pick up the saved chords.
///
/// Returns an error when a key name doesn't map to a `keytap::Key`, so the
/// picker UI can surface "this key isn't supported" instead of silently
/// dropping it from the chord.
#[cfg(desktop)]
#[command]
fn update_chord_bindings(
    state: State<'_, HotkeyState>,
    push_to_talk: Vec<String>,
    toggle_to_talk: Vec<String>,
    command: Option<Vec<String>>,
) -> Result<(), String> {
    let bindings =
        build_chord_bindings(&push_to_talk, &toggle_to_talk, &command.unwrap_or_default())?;
    let mut slot = state.monitor.lock().map_err(|e| e.to_string())?;
    if let Some(monitor) = slot.as_mut() {
        monitor.update_bindings(bindings);
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
#[command]
fn open_accessibility_settings(app: tauri::AppHandle) -> Result<(), String> {
    if !accessibility::was_asked(&app) {
        accessibility::remember_asked(&app);
        if !accessibility::request() {
            return Ok(());
        }
    }
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("Failed to open Accessibility settings: {e}"))?;
    Ok(())
}

/// Open the Privacy & Security → Input Monitoring pane in System Settings.
/// Used by the Captures settings UI when the toggle is on but the grant
/// is missing, so the user can flip the system toggle without hunting.
///
/// When Kass isn't listed yet, only asks: the request is what lists it, and
/// macOS's prompt has its own button to open the pane.
#[command]
fn open_input_monitoring_settings(app: tauri::AppHandle) -> Result<(), String> {
    if !input_monitoring::is_listed() {
        let _ = input_monitoring::request();
        return Ok(());
    }
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("Failed to open Input Monitoring settings: {e}"))?;
    Ok(())
}

/// Deliver `text` into the UI that had focus when the chord fired.
///
/// Runs the insertion chain (`insert_chain.rs`): a verified Accessibility
/// write first, then the steps that act on the frontmost app, ending with
/// clipboard + ⌘V. Each step falls through only when it inserted nothing, and
/// the log line shows what every step did and how long it took.
///
/// Skips (returns `false`) without touching anything when `focus.bundle_id`
/// is Kass itself — native dictation inserts into our own webview
/// through the DOM (`dictation::insert_in_app`). Errors when Accessibility
/// is not trusted: every step needs it.
#[command]
async fn paste_final_text(
    text: String,
    focus: focus_capture::FocusSnapshot,
) -> Result<bool, String> {
    paste_final_text_with(text, focus, None).await
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

/// Inspect the currently focused UI element. Returns the owning app's PID,
/// bundle id, and AX role. Useful for sanity-checking the focus pipeline
/// before committing to a paste.
#[command]
fn debug_capture_focus() -> Result<focus_capture::FocusSnapshot, String> {
    focus_capture::capture_focus()
}

/// Full auto-paste rehearsal: snapshot the focus target now, sleep
/// `drift_ms` so the user can deliberately switch to a different app
/// (proving we don't paste into whichever window is frontmost when the
/// transcribe finishes), then activate the captured PID, stage `text`,
/// fire ⌘V, and restore the clipboard.
#[command]
async fn debug_focus_roundtrip(
    text: String,
    drift_ms: u64,
    post_paste_delay_ms: u64,
) -> Result<serde_json::Value, String> {
    if !accessibility::is_trusted() {
        return Err(
            "Accessibility permission not granted. Open System Settings → Privacy & Security → Accessibility and enable Kass."
                .into(),
        );
    }

    let snapshot = focus_capture::capture_focus()?;

    tokio::time::sleep(std::time::Duration::from_millis(drift_ms)).await;

    focus_capture::activate_pid(snapshot.pid)?;
    // Give AppKit a beat to process the activation before the synthetic
    // Cmd+V arrives — without this the paste sometimes races ahead of the
    // window-ordering animation and lands in the previous frontmost app.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    let clip = clipboard::save_clipboard()?;
    let after_write = clipboard::write_text(&text)?;
    synthetic_keys::send_paste()?;
    tokio::time::sleep(std::time::Duration::from_millis(post_paste_delay_ms)).await;
    let before_restore = clipboard::current_change_count()?;
    clipboard::restore_clipboard(&clip)?;

    Ok(serde_json::json!({
        "focus": snapshot,
        "change_count_after_write": after_write,
        "change_count_before_restore": before_restore,
        "clobbered_during_paste": before_restore != after_write,
    }))
}

/// End-to-end smoke test for the auto-paste pipeline: save the user's
/// clipboard, stage `text`, optionally wait `pre_paste_delay_ms` so the
/// caller has time to focus the target app, synthesise ⌘V, wait
/// `post_paste_delay_ms` for the target app to consume the event, and put
/// the original clipboard back.
///
/// Short-circuits when Accessibility permission is missing — without it
/// `CGEventPost` silently drops events, so running the full sequence
/// would just clobber the clipboard with nothing to show for it.
#[command]
async fn debug_paste_text(
    text: String,
    pre_paste_delay_ms: u64,
    post_paste_delay_ms: u64,
) -> Result<serde_json::Value, String> {
    if !accessibility::is_trusted() {
        return Err(
            "Accessibility permission not granted. Open System Settings → Privacy & Security → Accessibility and enable Kass, then try again."
                .into(),
        );
    }

    let snapshot = clipboard::save_clipboard()?;
    let before = snapshot.change_count();
    let after_write = clipboard::write_text(&text)?;

    tokio::time::sleep(std::time::Duration::from_millis(pre_paste_delay_ms)).await;

    synthetic_keys::send_paste()?;

    tokio::time::sleep(std::time::Duration::from_millis(post_paste_delay_ms)).await;

    let before_restore = clipboard::current_change_count()?;
    clipboard::restore_clipboard(&snapshot)?;
    let after_restore = clipboard::current_change_count()?;

    Ok(serde_json::json!({
        "change_count_before": before,
        "change_count_after_write": after_write,
        "change_count_before_restore": before_restore,
        "change_count_after_restore": after_restore,
        "clobbered_during_paste": before_restore != after_write,
    }))
}

/// Manual smoke test for the clipboard snapshot/restore primitives used by
/// the auto-paste pipeline. Stages `text` on the pasteboard, waits
/// `hold_ms` so the caller can ⌘V into another app, then puts the original
/// clipboard contents back. The return value reports the change-count deltas
/// so the harness can verify no third party mutated the clipboard mid-paste.
#[command]
async fn debug_clipboard_roundtrip(
    text: String,
    hold_ms: u64,
) -> Result<serde_json::Value, String> {
    let snapshot = clipboard::save_clipboard()?;
    let before = snapshot.change_count();
    let item_count = snapshot.item_count();
    let after_write = clipboard::write_text(&text)?;

    tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;

    let before_restore = clipboard::current_change_count()?;
    clipboard::restore_clipboard(&snapshot)?;
    let after_restore = clipboard::current_change_count()?;

    Ok(serde_json::json!({
        "saved_items": item_count,
        "change_count_before": before,
        "change_count_after_write": after_write,
        "change_count_before_restore": before_restore,
        "change_count_after_restore": after_restore,
        "clobbered_during_hold": before_restore != after_write,
    }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    app_location::carry_over_old_bundle();
    identifier_move::move_from_old_identifier(KASS_BUNDLE_ID);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::UpdaterState::default())
        .manage(ServerState {
            child: Mutex::new(None),
            server_pid: Mutex::new(None),
            models_dir: Mutex::new(None),
        })
        .manage(dictation::DictationState::default())
        .manage(deep_link::DeepLinkState::default())
        .manage(hotkey_monitor::ChordMode::default())
        .setup(|app| {
            // The main window starts hidden (tauri.conf.json) so a login
            // launch stays out of the way until the Dock icon is clicked.
            // Its page still runs while hidden (`backgroundThrottling`
            // disabled there): it starts the server and arms the global keys.
            if !login_item::launched_at_login() {
                if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                    let _ = window.show();
                }
            }
            login_item::register_by_default(app.handle());
            dictation::restore(app.handle());
            sound_cues::init(app.handle());
            updater::start(app.handle().clone());
            #[cfg(desktop)]
            {
                // Resolve the active keyboard layout's V keycode now, on
                // the main thread, and register an observer for layout
                // changes. The synthetic-paste hot path then only reads an
                // atomic. See keyboard_layout.rs for why this matters
                // (Cmd+V is matched by translated character, not keycode,
                // so QWERTY keycode 9 produces Cmd+. on Dvorak).
                keyboard_layout::init();
                // ⌘H hides the windows, so the pill can show without them.
                app_hide::init();
                // Apps that show a text field when dictation starts
                // (docs/DICTATION_HANDSHAKE.md) register and reply here.
                dictation_handshake::listen();

                // HotkeyMonitor is spawned lazily via the `enable_hotkey`
                // command — see HotkeyState. The hidden dictate webview is
                // safe to build up front because it does not create the global
                // keyboard tap or trigger the macOS Input Monitoring prompt.
                app.manage(HotkeyState::default());

                // The frontend emits `dictate:hide` whenever the pill cycle
                // finishes (rest-fade → hidden). `hide()` alone has been
                // unreliable for transparent always-on-top windows on macOS
                // — the NSWindow lingers as an invisible click target that
                // steals focus to the Kass app when the user clicks
                // where it used to be. Park the window off-screen and mark
                // it click-through as well, so even if `hide()` no-ops the
                // user sees and interacts with nothing.
                let handle_for_hide = app.handle().clone();
                app.handle().listen("dictate:hide", move |_event| {
                    if let Some(window) = handle_for_hide.get_webview_window(DICTATE_WINDOW_LABEL) {
                        // A chip cut off by the end of the take gives its room back.
                        set_dictate_chip_space(&window, false);
                        let _ = window.set_ignore_cursor_events(true);
                        let _ = window.set_position(PhysicalPosition::new(-10_000, -10_000));
                        let _ = window.hide();
                    }
                });

                ensure_dictate_window(app.handle());
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dictate_chip_space,
            start_server,
            stop_server,
            restart_server,
            restart_app,
            app_location,
            move_to_applications,
            debug_clipboard_roundtrip,
            debug_paste_text,
            debug_capture_focus,
            debug_focus_roundtrip,
            app_icon,
            check_accessibility_permission,
            check_input_monitoring_permission,
            open_accessibility_settings,
            open_input_monitoring_settings,
            paste_final_text,
            enable_hotkey,
            disable_hotkey,
            update_chord_bindings,
            hotkey_monitor::set_chord_practice,
            hotkey_monitor::set_dictation_gate,
            open_onboarding,
            finish_onboarding,
            onboarding_window_open,
            dictation::mic::microphone_permission,
            dictation::mic::mic_preview_start,
            dictation::mic::mic_preview_stop,
            dictation::dictation_configure,
            dictation::dictation_start,
            dictation::dictation_stop,
            dictation::command_run,
            dictation::corrections::apply_correction,
            dictation::list_input_devices,
            sound_cues::configure_sound_cues,
            sound_cues::preview_sound_cue,
            login_item::launch_at_login_status,
            login_item::set_launch_at_login,
            login_item::open_login_items_settings,
            report::create_report,
            report::reveal_report,
            deep_link::take_deep_link,
            updater::update_status,
            updater::check_for_updates,
            updater::update_channel,
            updater::set_update_channel,
            updater::restart_to_update
        ])
        .on_window_event(|window, event| {
            if window.label() == ONBOARDING_WINDOW_LABEL {
                match event {
                    // Its close button: the same as finish_onboarding(None).
                    WindowEvent::CloseRequested { .. } => {
                        release_onboarding(window.app_handle());
                        show_main_after_onboarding(window.app_handle(), None);
                    }
                    WindowEvent::Destroyed => release_onboarding(window.app_handle()),
                    _ => {}
                }
                return;
            }
            // Closing the main window hides it: Kass keeps running for
            // dictation, and the Dock icon brings it back through
            // `RunEvent::Reopen`, which can only show a window that still
            // exists. The server stops when Kass quits (`RunEvent::Exit`).
            if window.label() == MAIN_WINDOW_LABEL {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            match &event {
                RunEvent::Opened { urls } => {
                    for url in urls {
                        deep_link::open(app, url.as_str());
                    }
                }
                RunEvent::Reopen {
                    has_visible_windows: false,
                    ..
                } => {
                    // Onboarding stands in for the main window while it's open.
                    let window = app
                        .get_webview_window(ONBOARDING_WINDOW_LABEL)
                        .or_else(|| app.get_webview_window(MAIN_WINDOW_LABEL));
                    if let Some(window) = window {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                RunEvent::Exit => {
                    let state = app.state::<ServerState>();
                    if let Err(error) = stop_managed_server(&state) {
                        eprintln!("Failed to stop local server on exit: {error}");
                    }
                    // A downloaded update goes in as Kass quits, with the
                    // server stopped, so the next launch is the new version.
                    if let Err(error) = updater::install_pending(app) {
                        eprintln!("Failed to install the downloaded update: {error}");
                    }
                }
                RunEvent::ExitRequested { .. } => {
                    // Stop descendants before the shell plugin's Exit handler
                    // kills the launcher and reparents its PyInstaller worker.
                    let state = app.state::<ServerState>();
                    if let Err(error) = stop_managed_server(&state) {
                        eprintln!("Failed to stop local server before exit: {error}");
                    }
                }
                _ => {}
            }
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
