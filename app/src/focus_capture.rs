//! Captures the focused-UI snapshot at chord-start so auto-paste can land
//! in the user's original text field even after focus drifts during
//! transcription / refinement.
//!
//! We don't try to re-focus a specific sub-element on restore — many apps
//! expose complex focus hierarchies that don't respond consistently to
//! programmatic focus pokes. Bringing the owning *window* to the
//! foreground is enough: the window's own focus manager restores its
//! last-focused field, which is what every well-behaved paste-buffer tool
//! does and what users expect.
//!
//! Capture uses `AXUIElementCopyAttributeValue(kAXFocusedUIElement)` +
//! `AXUIElementGetPid`; restore uses NSRunningApplication activation.
//! Activation uses the cooperative-activation pattern on macOS 14+ (the
//! caller `yieldActivationToApplication:`s, then the target `activate`s)
//! and falls back to the pre-Sonoma `activateWithOptions:` on 11–13. See
//! `activate_pid` for the rationale.
//!
//! PID + bundle id + role are all captured for diagnostics — the bundle
//! id lets step 6 (internal direct injection) detect "focus was inside
//! Kass itself" and short-circuit the synthetic-paste path. The bundle
//! id and app name are also saved with the capture, so Captures can show
//! which app each dictation went to.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FocusSnapshot {
    pub pid: i32,
    pub bundle_id: Option<String>,
    /// The app's display name, e.g. "Slack".
    #[serde(default)]
    pub app_name: Option<String>,
    pub role: Option<String>,
}

use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease};
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringCreateWithCString, CFStringGetCString, CFStringGetLength,
    CFStringRef,
};
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};

type Id = *mut Object;

mod ffi {
    use core_foundation_sys::base::CFTypeRef;
    use core_foundation_sys::string::CFStringRef;

    pub type AXError = i32;
    pub const AX_ERROR_SUCCESS: AXError = 0;
    pub type AXUIElementRef = *const std::ffi::c_void;
    pub type Pid = i32;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        pub fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        pub fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut Pid) -> AXError;
        pub fn AXUIElementCreateApplication(pid: Pid) -> AXUIElementRef;
        pub fn AXUIElementSetMessagingTimeout(
            element: AXUIElementRef,
            timeout_in_seconds: f32,
        ) -> AXError;
        pub fn AXValueGetValue(
            value: CFTypeRef,
            the_type: u32,
            value_ptr: *mut std::ffi::c_void,
        ) -> core_foundation_sys::base::Boolean;
    }

    pub const AX_VALUE_CG_POINT: u32 = 1;
    pub const AX_VALUE_CG_SIZE: u32 = 2;

    #[repr(C)]
    #[derive(Default)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventCreate(source: *const std::ffi::c_void) -> CFTypeRef;
        pub fn CGEventGetLocation(event: CFTypeRef) -> CGPoint;
    }
    // AX attribute keys are exposed as C macros that expand to CFSTR(...)
    // literals, not as linkable symbols — build the CFStrings at runtime
    // instead (see `cf_string_const` in focus_capture.rs).
}

pub(crate) struct AutoreleasePool {
    pool: Id,
}

impl AutoreleasePool {
    pub(crate) unsafe fn new() -> Self {
        let pool: Id = msg_send![class!(NSAutoreleasePool), alloc];
        let pool: Id = msg_send![pool, init];
        Self { pool }
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.pool, drain];
        }
    }
}

pub(crate) unsafe fn ns_string_to_rust(s: Id) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let bytes: *const i8 = msg_send![s, UTF8String];
    if bytes.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(bytes)
        .to_str()
        .ok()
        .map(|x| x.to_owned())
}

/// Build a `+1` retained CFString from an ASCII constant. Caller owns the
/// returned reference and must `CFRelease` it. Used for AX attribute keys
/// (`"AXFocusedUIElement"`, `"AXRole"`) because those aren't exported as
/// linker symbols — Apple ships them as `CFSTR(...)` macros.
pub(crate) unsafe fn cf_string_const(s: &str) -> Option<CFStringRef> {
    let cstr = std::ffi::CString::new(s).ok()?;
    let result =
        CFStringCreateWithCString(kCFAllocatorDefault, cstr.as_ptr(), kCFStringEncodingUTF8);
    if result.is_null() {
        None
    } else {
        Some(result)
    }
}

pub(crate) unsafe fn cfstring_to_rust(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let len = CFStringGetLength(s);
    if len == 0 {
        return Some(String::new());
    }
    // CFStringGetLength is in UTF-16 code units; UTF-8 can need up to 4
    // bytes per unit plus the trailing NUL.
    let max_bytes = (len * 4 + 1) as usize;
    let mut buf = vec![0u8; max_bytes];
    let ok = CFStringGetCString(
        s,
        buf.as_mut_ptr() as *mut i8,
        max_bytes as isize,
        kCFStringEncodingUTF8,
    );
    if ok == 0 {
        return None;
    }
    let cstr = std::ffi::CStr::from_ptr(buf.as_ptr() as *const i8);
    cstr.to_str().ok().map(|x| x.to_owned())
}

/// Bundle id and display name of the app running as `pid`.
unsafe fn app_for_pid(pid: i32) -> (Option<String>, Option<String>) {
    let _pool = AutoreleasePool::new();
    let app: Id = msg_send![
        class!(NSRunningApplication),
        runningApplicationWithProcessIdentifier: pid
    ];
    if app.is_null() {
        return (None, None);
    }
    let bundle: Id = msg_send![app, bundleIdentifier];
    let name: Id = msg_send![app, localizedName];
    (ns_string_to_rust(bundle), ns_string_to_rust(name))
}

/// Bundle id and display name of the app running as `pid`.
pub fn app_identity(pid: i32) -> (Option<String>, Option<String>) {
    unsafe { app_for_pid(pid) }
}

/// Filesystem path of the app bundle running as `pid`.
pub fn app_bundle_path(pid: i32) -> Option<String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let app: Id = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return None;
        }
        let url: Id = msg_send![app, bundleURL];
        if url.is_null() {
            return None;
        }
        let path: Id = msg_send![url, path];
        ns_string_to_rust(path)
    }
}

/// A snapshot of `pid` with no focused element to describe.
unsafe fn app_snapshot(pid: i32) -> FocusSnapshot {
    let (bundle_id, app_name) = app_for_pid(pid);
    FocusSnapshot {
        pid,
        bundle_id,
        app_name,
        role: None,
    }
}

/// Read the system-wide focused UI element's PID, bundle id, and AX role.
///
/// Returns an error when no element is focused (e.g. Dock has focus) or
/// when Accessibility permission is missing — `AXUIElementCopyAttributeValue`
/// returns `-25204 kAXErrorAPIDisabled` in that case.
pub fn capture_focus() -> Result<FocusSnapshot, String> {
    use ffi::*;
    unsafe {
        let system_wide = AXUIElementCreateSystemWide();
        if system_wide.is_null() {
            return Err("AXUIElementCreateSystemWide returned null".into());
        }
        let _sys_guard = scopeguard::guard(system_wide, |e| CFRelease(e));

        let focused_attr = cf_string_const("AXFocusedUIElement")
            .ok_or("Failed to build AXFocusedUIElement CFString")?;
        let _focused_attr_guard =
            scopeguard::guard(focused_attr, |s| CFRelease(s as *const std::ffi::c_void));

        let mut focused: *const std::ffi::c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(system_wide, focused_attr, &mut focused as *mut _);
        if err != AX_ERROR_SUCCESS || focused.is_null() {
            // Some apps (terminals, some Electron windows) expose no
            // system-wide focused element. Rather than drop the dictation,
            // fall back to the frontmost app so the transcript still injects
            // there via activate + ⌘V.
            if let Some(fp) = frontmost_pid() {
                return Ok(app_snapshot(fp));
            }
            return Err(format!(
                "No focused element (AXError {}). Verify Accessibility permission is granted and a focused text field exists.",
                err
            ));
        }
        let _focus_guard = scopeguard::guard(focused, |e| CFRelease(e));

        let focused_elem = focused as AXUIElementRef;

        let mut pid: Pid = 0;
        let err = AXUIElementGetPid(focused_elem, &mut pid);
        if err != AX_ERROR_SUCCESS {
            return Err(format!("AXUIElementGetPid failed (AXError {})", err));
        }

        // If the focused element belongs to our OWN process while another app
        // is frontmost, the system-wide AXFocusedUIElement has resolved to the
        // dictate pill instead of the user's real target, which would make us
        // paste into ourselves (a no-op) or drop the text entirely. The pill
        // is a non-activating, never-key panel so it never becomes the
        // frontmost application; remap to the frontmost app, which is always
        // the real dictation target.
        let our_pid = std::process::id() as Pid;
        if pid == our_pid {
            if let Some(fp) = frontmost_pid() {
                if fp != our_pid {
                    return Ok(app_snapshot(fp));
                }
            }
        }

        let role = {
            let role_attr = cf_string_const("AXRole");
            match role_attr {
                Some(role_attr) => {
                    let _role_attr_guard =
                        scopeguard::guard(role_attr, |s| CFRelease(s as *const std::ffi::c_void));
                    let mut role_value: *const std::ffi::c_void = std::ptr::null();
                    let err = AXUIElementCopyAttributeValue(
                        focused_elem,
                        role_attr,
                        &mut role_value as *mut _,
                    );
                    if err == AX_ERROR_SUCCESS && !role_value.is_null() {
                        let _role_guard = scopeguard::guard(role_value, |e| CFRelease(e));
                        cfstring_to_rust(role_value as CFStringRef)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };

        let (bundle_id, app_name) = app_for_pid(pid);

        Ok(FocusSnapshot {
            pid,
            bundle_id,
            app_name,
            role,
        })
    }
}

/// The app whose window is frontmost apart from Kass's own: where the
/// user was before opening Kass's window (Command Mode's palette runs
/// there). Reads the on-screen window list front to back and takes the first
/// normal-level window another process owns.
pub fn app_behind_kass() -> Option<FocusSnapshot> {
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt32Type, CFNumberGetValue, CFNumberRef};

    /// `kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements`.
    const ON_SCREEN_WITHOUT_DESKTOP: u32 = 1 | 16;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
        static kCGWindowOwnerPID: CFStringRef;
        static kCGWindowLayer: CFStringRef;
    }

    unsafe fn number(dict: CFDictionaryRef, key: CFStringRef) -> Option<i32> {
        let value = CFDictionaryGetValue(dict, key as *const std::ffi::c_void);
        if value.is_null() {
            return None;
        }
        let mut n: i32 = 0;
        CFNumberGetValue(
            value as CFNumberRef,
            kCFNumberSInt32Type,
            &mut n as *mut i32 as *mut std::ffi::c_void,
        )
        .then_some(n)
    }

    let ours = std::process::id() as i32;
    unsafe {
        let windows = CGWindowListCopyWindowInfo(ON_SCREEN_WITHOUT_DESKTOP, 0);
        if windows.is_null() {
            return None;
        }
        let _guard = scopeguard::guard(windows, |w| CFRelease(w as *const std::ffi::c_void));
        (0..CFArrayGetCount(windows))
            .map(|i| CFArrayGetValueAtIndex(windows, i) as CFDictionaryRef)
            .filter(|dict| !dict.is_null())
            .find_map(|dict| {
                let pid = number(dict, kCGWindowOwnerPID)?;
                // Layer 0 holds app windows; the menu bar, Dock and panels
                // like the dictation pill sit above it.
                (number(dict, kCGWindowLayer) == Some(0) && pid != ours).then_some(pid)
            })
            .map(|pid| app_snapshot(pid))
    }
}

/// Bring the app owning `pid` to the foreground, re-activating its
/// last-focused window. Paired with [`capture_focus`] at chord-start so a
/// post-transcription synthetic ⌘V lands where the user started, not
/// wherever focus drifted to during the transcribe / refine window.
///
/// macOS 14 (Sonoma) deprecated `activateWithOptions:` in favour of a
/// cooperative-activation pattern: the caller first invokes
/// `yieldActivationToApplication:` on its own `NSRunningApplication` to
/// grant the target activation rights, then the target's `activate`
/// succeeds against the tightened Sonoma foreground rules. Without the
/// yield, `activate` on 14+ sometimes silently fails or only bounces the
/// dock icon — exactly the "paste lands in the wrong app" symptom we're
/// trying to prevent. The yield is discovered at runtime via
/// `respondsToSelector:` so we don't need an operatingSystemVersion probe
/// and the pre-Sonoma path stays identical.
///
/// The BOOL return of both `activate` and `activateWithOptions:` is now
/// propagated — if the system refuses activation (target quit mid-
/// transcription, trust revoked, cooperative-activation refused) the
/// caller aborts before clobbering the clipboard.
pub fn activate_pid(pid: i32) -> Result<(), String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let target: Id = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if target.is_null() {
            return Err(format!("No running application for PID {}", pid));
        }

        let activated: bool = if can_yield_activation() {
            let current: Id = msg_send![class!(NSRunningApplication), currentApplication];
            if !current.is_null() {
                let _: () = msg_send![current, yieldActivationToApplication: target];
            }
            msg_send![target, activate]
        } else {
            // NSApplicationActivateIgnoringOtherApps = 1 << 1 = 2.
            msg_send![target, activateWithOptions: 2u64]
        };

        if !activated {
            return Err(format!(
                "NSRunningApplication activate returned false for PID {} — the target may have quit mid-transcription, Accessibility is no longer trusted, or the system refused cooperative activation.",
                pid
            ));
        }
        Ok(())
    }
}

/// PID of the app the user is currently in (NSWorkspace frontmost). Used to
/// skip re-activation when the paste target never lost frontmost status —
/// activating an already-frontmost app is a no-op at best, and on
/// fullscreen Spaces macOS 26's cooperative activation returns NO for it,
/// which previously aborted the whole paste.
pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let ws: Id = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            return None;
        }
        let app: Id = msg_send![ws, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        Some(pid)
    }
}

/// `true` when `NSRunningApplication` responds to
/// `yieldActivationToApplication:` — the macOS 14+ discriminator for the
/// cooperative-activation APIs. Cached since the answer doesn't change
/// over a process's lifetime and the objc_msgSend probe is otherwise
/// repeated on every paste.
fn can_yield_activation() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| unsafe {
        let current: Id = msg_send![class!(NSRunningApplication), currentApplication];
        if current.is_null() {
            return false;
        }
        let responds: bool = msg_send![
            current,
            respondsToSelector: sel!(yieldActivationToApplication:)
        ];
        responds
    })
}

/// Center of the frontmost app's focused window, in global display points
/// (origin at the top-left of the main display, y growing down). `None`
/// when the app exposes no focused window or Accessibility isn't granted.
pub fn focused_window_center() -> Option<(f64, f64)> {
    use ffi::*;
    let pid = frontmost_pid()?;
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        let _app_guard = scopeguard::guard(app, |e| CFRelease(e));
        // This runs on the hotkey press. A hung app would otherwise block
        // for the 6s AX default before the pill appears.
        AXUIElementSetMessagingTimeout(app, 0.05);

        let window = copy_attribute(app, "AXFocusedWindow")?;
        let _window_guard = scopeguard::guard(window, |e| CFRelease(e));

        let position = copy_attribute(window as AXUIElementRef, "AXPosition")?;
        let _position_guard = scopeguard::guard(position, |e| CFRelease(e));
        let size = copy_attribute(window as AXUIElementRef, "AXSize")?;
        let _size_guard = scopeguard::guard(size, |e| CFRelease(e));

        let mut origin = CGPoint::default();
        let mut extent = CGSize::default();
        let read = 0
            != AXValueGetValue(
                position,
                AX_VALUE_CG_POINT,
                &mut origin as *mut CGPoint as *mut std::ffi::c_void,
            )
            && 0 != AXValueGetValue(
                size,
                AX_VALUE_CG_SIZE,
                &mut extent as *mut CGSize as *mut std::ffi::c_void,
            );
        read.then(|| {
            (
                origin.x + extent.width / 2.0,
                origin.y + extent.height / 2.0,
            )
        })
    }
}

/// The mouse cursor, in the same global display points as
/// [`focused_window_center`].
pub fn cursor_location() -> Option<(f64, f64)> {
    use ffi::*;
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        Some((point.x, point.y))
    }
}

/// `+1` retained value of `attribute` on `element`, or `None`.
unsafe fn copy_attribute(
    element: ffi::AXUIElementRef,
    attribute: &str,
) -> Option<core_foundation_sys::base::CFTypeRef> {
    let attr = cf_string_const(attribute)?;
    let _attr_guard = scopeguard::guard(attr, |s| CFRelease(s as *const std::ffi::c_void));
    let mut value: core_foundation_sys::base::CFTypeRef = std::ptr::null();
    let err = ffi::AXUIElementCopyAttributeValue(element, attr, &mut value as *mut _);
    (err == ffi::AX_ERROR_SUCCESS && !value.is_null()).then_some(value)
}
