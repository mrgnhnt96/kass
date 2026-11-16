//! Platform permission gate for the auto-paste pipeline.
//!
//! On macOS, posting synthetic keyboard events and reading focused-UI state
//! via the AX API both require the host process to be listed under System
//! Settings → Privacy & Security → Accessibility. Without that trust,
//! `CGEventPost` silently drops events and `AXUIElementCopyAttributeValue`
//! returns an error. We surface a boolean check up front so the paste
//! pipeline can short-circuit with a clear "grant permission" message
//! instead of running through the full save → write → post → restore dance
//! with nothing to show for it.

mod ffi {
    use core_foundation_sys::dictionary::CFDictionaryRef;
    use core_foundation_sys::string::CFStringRef;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        /// Returns true when the current process is listed in Accessibility.
        /// No prompt side-effect.
        pub fn AXIsProcessTrusted() -> bool;

        /// Like `AXIsProcessTrusted`, but with `kAXTrustedCheckOptionPrompt`
        /// set it also shows the system prompt and adds the process to the
        /// Accessibility pane (toggle off) when it isn't trusted yet.
        pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;

        pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    }
}

pub fn is_trusted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() }
}

/// Fire the Accessibility prompt if not already trusted, which also lists
/// Kass in the Accessibility pane so the user has a toggle to flip.
/// Returns the current trust state. macOS shows the prompt at most once per
/// grant, so calling this repeatedly is harmless.
pub fn request() -> bool {
    use core_foundation_sys::base::{CFRelease, CFTypeRef};
    use core_foundation_sys::dictionary::{
        kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreate,
    };
    use core_foundation_sys::number::kCFBooleanTrue;
    use std::ffi::c_void;

    unsafe {
        let keys = [ffi::kAXTrustedCheckOptionPrompt as *const c_void];
        let values = [kCFBooleanTrue as *const c_void];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if options.is_null() {
            return ffi::AXIsProcessTrusted();
        }
        let trusted = ffi::AXIsProcessTrustedWithOptions(options);
        CFRelease(options as CFTypeRef);
        trusted
    }
}

/// Marker in the app config directory: Kass has asked for Accessibility.
const ASKED_MARKER: &str = "accessibility-asked";

/// Whether Kass has asked for Accessibility before, which lists it in the
/// Accessibility pane. macOS has no call that says whether it is listed.
pub fn was_asked(app: &crate::host::AppHandle) -> bool {
    use crate::host::Manager;
    app.path()
        .app_config_dir()
        .is_ok_and(|dir| dir.join(ASKED_MARKER).exists())
}

/// Record that Kass asked, so the next time opens the pane instead.
pub fn remember_asked(app: &crate::host::AppHandle) {
    use crate::host::Manager;
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(ASKED_MARKER), b""))
    {
        eprintln!("[accessibility] could not record asking: {e}");
    }
}
