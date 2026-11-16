//! The app host: GPUI owns the macOS loop, and the React screens run in wry
//! web views inside GPUI windows (docs/plans/GPUI_HOST.md).
//!
//! This module gives the rest of the crate the small part of Tauri's API it
//! used (`AppHandle`, managed state, events, paths, the async runtime), so
//! moving off Tauri didn't touch dictation, hotkeys or text insertion.

mod dispatch;
pub mod ipc;
pub mod menu;
pub mod webview;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

pub use dispatch::{main_async, main_sync, with_app};
pub use webview::WebviewWindow;

/// The bundle identifier, which also names the app's data folders.
pub const IDENTIFIER: &str = "com.mrgnhnt.kass";

/// A host error. Shown with `{e}` or `to_string()`, as Tauri's were.
#[derive(Debug, Clone)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A handle to the running app. Everything it reaches is global, so it is
/// free to copy and to send to any thread.
#[derive(Clone, Debug, Default)]
pub struct AppHandle;

/// Managed state: one value per type, alive for the whole process.
pub struct State<'a, T: 'static>(&'a T);

impl<T: 'static> Clone for State<'_, T> {
    fn clone(&self) -> Self {
        State(self.0)
    }
}

impl<T: 'static> Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

type StateMap = Mutex<HashMap<TypeId, &'static (dyn Any + Send + Sync)>>;

fn states() -> &'static StateMap {
    static STATES: OnceLock<StateMap> = OnceLock::new();
    STATES.get_or_init(Default::default)
}

pub struct PackageInfo {
    pub version: String,
}

/// The app's folders, where Tauri put them, so nothing saved moves.
pub struct PathResolver;

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error("No home folder".into()))
}

impl PathResolver {
    pub fn app_data_dir(&self) -> Result<PathBuf> {
        Ok(home()?.join("Library/Application Support").join(IDENTIFIER))
    }

    pub fn app_config_dir(&self) -> Result<PathBuf> {
        self.app_data_dir()
    }

    pub fn app_cache_dir(&self) -> Result<PathBuf> {
        Ok(home()?.join("Library/Caches").join(IDENTIFIER))
    }

    /// `Contents/Resources` in the app bundle; next to the binary in a
    /// development build.
    pub fn resource_dir(&self) -> Result<PathBuf> {
        let exe = std::env::current_exe().map_err(|e| Error(e.to_string()))?;
        let dir = exe
            .parent()
            .ok_or_else(|| Error("The app has no folder".into()))?;
        if dir.ends_with("Contents/MacOS") {
            if let Some(contents) = dir.parent() {
                return Ok(contents.join("Resources"));
            }
        }
        Ok(dir.to_path_buf())
    }
}

pub trait Manager {
    /// The managed `T`. Panics when none was managed, as Tauri's did.
    fn state<T: Send + Sync + 'static>(&self) -> State<'static, T> {
        self.try_state()
            .unwrap_or_else(|| panic!("state {} is not managed", std::any::type_name::<T>()))
    }

    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'static, T>> {
        let value = *states().lock().unwrap().get(&TypeId::of::<T>())?;
        value.downcast_ref::<T>().map(State)
    }

    /// Manage `value`, unless a `T` is managed already.
    fn manage<T: Send + Sync + 'static>(&self, value: T) -> bool {
        let mut map = states().lock().unwrap();
        if map.contains_key(&TypeId::of::<T>()) {
            return false;
        }
        map.insert(TypeId::of::<T>(), Box::leak(Box::new(value)));
        true
    }

    fn path(&self) -> PathResolver {
        PathResolver
    }

    fn package_info(&self) -> PackageInfo {
        PackageInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn get_webview_window(&self, label: &str) -> Option<WebviewWindow> {
        webview::get(label)
    }

    fn webview_windows(&self) -> HashMap<String, WebviewWindow> {
        webview::all()
            .into_iter()
            .map(|window| (window.label().to_string(), window))
            .collect()
    }
}

impl Manager for AppHandle {}

/// An event as a Rust listener sees it.
pub struct Event {
    payload: String,
    target: Option<String>,
}

impl Event {
    /// The payload as JSON text.
    pub fn payload(&self) -> &str {
        &self.payload
    }

    /// The window label it was sent to with `emit_to`; `None` for everyone.
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn new(payload: &str, target: Option<&str>) -> Self {
        Self {
            payload: payload.to_string(),
            target: target.map(str::to_string),
        }
    }
}

pub type EventId = u32;

type Handler = Arc<dyn Fn(Event) + Send + Sync>;

type Listeners = Mutex<HashMap<String, Vec<(EventId, Handler)>>>;

fn listeners() -> &'static Listeners {
    static LISTENERS: OnceLock<Listeners> = OnceLock::new();
    LISTENERS.get_or_init(Default::default)
}

/// Send `event` to the Rust listeners and to every web view. With a
/// `target`, a web view's listeners for one window only hear it in that
/// window; its listeners for any window hear it everywhere, as in Tauri.
pub(crate) fn emit_value(event: &str, payload: serde_json::Value, target: Option<String>) {
    let handlers: Vec<Handler> = listeners()
        .lock()
        .unwrap()
        .get(event)
        .map(|list| list.iter().map(|(_, h)| h.clone()).collect())
        .unwrap_or_default();
    if !handlers.is_empty() {
        let text = payload.to_string();
        for handler in handlers {
            handler(Event {
                payload: text.clone(),
                target: target.clone(),
            });
        }
    }
    let event = event.to_string();
    main_async(move || webview::broadcast(&event, &payload, target.as_deref()));
}

pub trait Emitter {
    fn emit<S: Serialize>(&self, event: &str, payload: S) -> Result<()> {
        let payload = serde_json::to_value(payload).map_err(|e| Error(e.to_string()))?;
        emit_value(event, payload, None);
        Ok(())
    }

    fn emit_to<S: Serialize>(&self, target: &str, event: &str, payload: S) -> Result<()> {
        let payload = serde_json::to_value(payload).map_err(|e| Error(e.to_string()))?;
        emit_value(event, payload, Some(target.to_string()));
        Ok(())
    }
}

impl Emitter for AppHandle {}

pub trait Listener {
    fn listen<F>(&self, event: impl Into<String>, handler: F) -> EventId
    where
        F: Fn(Event) + Send + Sync + 'static,
    {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        listeners()
            .lock()
            .unwrap()
            .entry(event.into())
            .or_default()
            .push((id, Arc::new(handler)));
        id
    }

    fn unlisten(&self, id: EventId) {
        for list in listeners().lock().unwrap().values_mut() {
            list.retain(|(each, _)| *each != id);
        }
    }
}

impl Listener for AppHandle {}

/// The process's tokio runtime, where async commands and background work run.
pub mod async_runtime {
    use std::future::Future;
    use std::sync::OnceLock;
    use tokio::runtime::Runtime;
    use tokio::task::JoinHandle;

    pub fn runtime() -> &'static Runtime {
        static RUNTIME: OnceLock<Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("start the async runtime")
        })
    }

    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        runtime().spawn(future)
    }

    pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        runtime().spawn_blocking(f)
    }
}

/// Open `target` (a URL or a path) the way Finder would.
pub fn open_external(target: &str) -> Result<()> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(target)
        .status()
        .map_err(|e| Error(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error(format!("Could not open {target}")))
    }
}

impl AppHandle {
    /// Show a system notification from Kass.
    pub fn notify(&self, title: &str, body: &str) {
        notify(title, body);
    }
}

fn notify(title: &str, body: &str) {
    static APP: std::sync::Once = std::sync::Once::new();
    APP.call_once(|| {
        // A development build has no bundle of its own to post from.
        let id = if cfg!(debug_assertions) {
            "com.apple.Terminal"
        } else {
            IDENTIFIER
        };
        let _ = notify_rust::set_application(id);
    });
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .show()
        {
            eprintln!("Notification failed: {e}");
        }
    });
}

/// Start the host on GPUI's loop: `with_app` jobs and the menu bar.
pub fn install(cx: &mut gpui_kit::App) {
    dispatch::install(cx);
    menu::install(cx);
}
