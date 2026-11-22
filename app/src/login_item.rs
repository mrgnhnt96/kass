//! Launch at login, through `SMAppService.mainAppService` (macOS 13+).
//!
//! The system's registration is the only record of whether Kass starts
//! at login: the Settings toggle reads `status` and writes with
//! `register` / `unregister`, and the user can also turn it off in System
//! Settings › General › Login Items, which shows up here as
//! "requires approval".
//!
//! The first launch of an installed build registers once, so launching at
//! login is on by default. A marker file in the app config dir records that
//! this happened, so turning it off afterwards sticks.
//!
//! Only the installed app registers (a `.app` in `/Applications`, where
//! `cargo xtask install` puts it; see `app_location`). A dev build runs a bare binary from
//! `target/`, and registering that would leave a stale login item behind.
//!
//! A login launch keeps the main window hidden (`launched_at_login`); the
//! Dock icon brings it back through `RunEvent::Reopen`.

use crate::host::{AppHandle, Manager};
use objc::runtime::{Class, Object, BOOL, YES};
use objc::{class, msg_send, sel, sel_impl};
use serde::Serialize;

use crate::app_location::is_installed;
use crate::focus_capture::{ns_string_to_rust, AutoreleasePool};

type Id = *mut Object;

/// Written once the first-launch default has run.
const DEFAULTED_MARKER: &str = "launch-at-login-defaulted";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Enabled,
    Disabled,
    /// Registered, but turned off in System Settings › Login Items.
    RequiresApproval,
    /// Not the installed app (a dev build), or macOS is older than 13.
    Unavailable,
}

/// Map `SMAppServiceStatus`: NotRegistered = 0, Enabled = 1,
/// RequiresApproval = 2, NotFound = 3.
fn status_from_raw(raw: isize) -> Status {
    match raw {
        1 => Status::Enabled,
        2 => Status::RequiresApproval,
        _ => Status::Disabled,
    }
}

/// `+[SMAppService mainAppService]`, or None before macOS 13 or from a dev
/// build.
unsafe fn main_app_service() -> Option<Id> {
    if !is_installed() {
        return None;
    }
    let class = Class::get("SMAppService")?;
    let service: Id = msg_send![class, mainAppService];
    (!service.is_null()).then_some(service)
}

pub fn status() -> Status {
    unsafe {
        let _pool = AutoreleasePool::new();
        match main_app_service() {
            Some(service) => {
                let raw: isize = msg_send![service, status];
                status_from_raw(raw)
            }
            None => Status::Unavailable,
        }
    }
}

/// Register or unregister, then report the status the system settled on.
/// Registering while the user has Kass turned off in Login Items fails
/// and leaves it at `RequiresApproval`, which is reported, not an error.
pub fn set_enabled(enabled: bool) -> Result<Status, String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let Some(service) = main_app_service() else {
            return Err("Only the installed app can launch at login".into());
        };
        let mut error: Id = std::ptr::null_mut();
        let ok: BOOL = if enabled {
            msg_send![service, registerAndReturnError: &mut error]
        } else {
            msg_send![service, unregisterAndReturnError: &mut error]
        };
        let raw: isize = msg_send![service, status];
        let status = status_from_raw(raw);
        let settled = (status == Status::Enabled) == enabled;
        if ok == YES || settled || status == Status::RequiresApproval {
            return Ok(status);
        }
        let message = if error.is_null() {
            None
        } else {
            ns_string_to_rust(msg_send![error, localizedDescription])
        };
        Err(message.unwrap_or_else(|| "The system refused the login item change".into()))
    }
}

/// Four-char codes from `<CoreServices/AE/AERegistry.h>`.
const K_AE_OPEN_APPLICATION: u32 = u32::from_be_bytes(*b"oapp");
const KEY_AE_PROP_DATA: u32 = u32::from_be_bytes(*b"prdt");
const K_AE_LAUNCHED_AS_LOG_IN_ITEM: u32 = u32::from_be_bytes(*b"lgit");

fn is_login_launch_event(event_id: u32, launch_kind: u32) -> bool {
    event_id == K_AE_OPEN_APPLICATION && launch_kind == K_AE_LAUNCHED_AS_LOG_IN_ITEM
}

/// Whether macOS launched Kass as a login item. Reads the "open
/// application" Apple event that is current only while
/// `applicationDidFinishLaunching` runs, so call it from the app's launch
/// callback in `main`, which runs inside it.
pub fn launched_at_login() -> bool {
    unsafe {
        let _pool = AutoreleasePool::new();
        let manager: Id = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let event: Id = msg_send![manager, currentAppleEvent];
        if event.is_null() {
            return false;
        }
        let event_id: u32 = msg_send![event, eventID];
        let param: Id = msg_send![event, paramDescriptorForKeyword: KEY_AE_PROP_DATA];
        if param.is_null() {
            return false;
        }
        let launch_kind: u32 = msg_send![param, enumCodeValue];
        is_login_launch_event(event_id, launch_kind)
    }
}

/// Turn launching at login on, once, the first time the installed app runs.
/// The marker is written before registering, so a failed write can only
/// skip the default, never undo the user turning it off.
pub fn register_by_default(app: &AppHandle) {
    if !is_installed() {
        return;
    }
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let marker = dir.join(DEFAULTED_MARKER);
    if marker.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&marker, b"")) {
        eprintln!("[login-item] could not record the launch-at-login default: {e}");
        return;
    }
    std::thread::spawn(|| {
        if let Err(e) = set_enabled(true) {
            eprintln!("[login-item] could not turn on launch at login: {e}");
        }
    });
}

pub fn launch_at_login_status() -> Status {
    status()
}

pub fn set_launch_at_login(enabled: bool) -> Result<Status, String> {
    set_enabled(enabled)
}

/// Open System Settings › General › Login Items, where the user approves
/// Kass after turning it off there.
pub fn open_login_items_settings() -> Result<(), String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let class = Class::get("SMAppService").ok_or("Login Items needs macOS 13 or later")?;
        let _: () = msg_send![class, openSystemSettingsLoginItems];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_system_statuses() {
        assert_eq!(status_from_raw(0), Status::Disabled);
        assert_eq!(status_from_raw(1), Status::Enabled);
        assert_eq!(status_from_raw(2), Status::RequiresApproval);
        assert_eq!(status_from_raw(3), Status::Disabled);
    }

    #[test]
    fn recognizes_a_login_launch() {
        assert!(is_login_launch_event(
            u32::from_be_bytes(*b"oapp"),
            u32::from_be_bytes(*b"lgit")
        ));
        assert!(!is_login_launch_event(u32::from_be_bytes(*b"oapp"), 0));
        assert!(!is_login_launch_event(
            u32::from_be_bytes(*b"odoc"),
            u32::from_be_bytes(*b"lgit")
        ));
    }
}
