//! ⌘H hides Kass's windows, not Kass.
//!
//! macOS unhides a hidden app, every window of it, as soon as it orders any
//! window front, so the dictation pill brought the main window back with it.
//! Instead, when Kass is hidden, the windows it showed are taken off screen
//! and the app is unhidden without activating: the pill then shows alone.
//! They come back when Kass is next activated (Dock click, ⌘Tab), as they
//! would from a real unhide.

pub fn init() {
    #[cfg(target_os = "macos")]
    macos::init();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_void, CString};
    use std::ptr;
    use std::sync::Mutex;

    use core_foundation_sys::dictionary::CFDictionaryRef;
    use core_foundation_sys::notification_center::{
        CFNotificationCenterAddObserver, CFNotificationCenterGetLocalCenter,
        CFNotificationCenterRef, CFNotificationName,
        CFNotificationSuspensionBehaviorDeliverImmediately,
    };
    use core_foundation_sys::string::CFStringRef;
    use objc::runtime::{Object, NO};
    use objc::{class, msg_send, sel, sel_impl};

    type Id = *mut Object;

    const WILL_HIDE: &str = "NSApplicationWillHideNotification";
    const DID_HIDE: &str = "NSApplicationDidHideNotification";
    const DID_BECOME_ACTIVE: &str = "NSApplicationDidBecomeActiveNotification";

    /// Windows Kass showed when it was hidden, retained until they're back.
    /// Touched only on the main thread, where AppKit posts these.
    static HIDDEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    pub fn init() {
        for (name, callback) in [
            (WILL_HIDE, will_hide as Callback),
            (DID_HIDE, did_hide as Callback),
            (DID_BECOME_ACTIVE, did_become_active as Callback),
        ] {
            observe(name, callback);
        }
    }

    type Callback = extern "C" fn(
        CFNotificationCenterRef,
        *mut c_void,
        CFNotificationName,
        *const c_void,
        CFDictionaryRef,
    );

    fn observe(name: &str, callback: Callback) {
        let name = CString::new(name).expect("notification name");
        unsafe {
            let center = CFNotificationCenterGetLocalCenter();
            if center.is_null() {
                eprintln!("app_hide: no local notification center");
                return;
            }
            let name: Id = msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
            CFNotificationCenterAddObserver(
                center,
                ptr::null(),
                callback,
                name as CFStringRef,
                ptr::null(),
                CFNotificationSuspensionBehaviorDeliverImmediately,
            );
        }
    }

    /// Kass's visible windows, other than panels (the pill stays as it was).
    fn visible_windows() -> Vec<Id> {
        unsafe {
            let app: Id = msg_send![class!(NSApplication), sharedApplication];
            // Front to back.
            let windows: Id = msg_send![app, orderedWindows];
            let count: usize = msg_send![windows, count];
            (0..count)
                .map(|i| -> Id { msg_send![windows, objectAtIndex: i] })
                .filter(|&window| {
                    let visible: objc::runtime::BOOL = msg_send![window, isVisible];
                    let panel: objc::runtime::BOOL =
                        msg_send![window, isKindOfClass: class!(NSPanel)];
                    visible != NO && panel == NO
                })
                .collect()
        }
    }

    extern "C" fn will_hide(
        _: CFNotificationCenterRef,
        _: *mut c_void,
        _: CFNotificationName,
        _: *const c_void,
        _: CFDictionaryRef,
    ) {
        // Once hidden, no window reads as visible.
        let windows = visible_windows();
        let mut hidden = HIDDEN.lock().unwrap_or_else(|e| e.into_inner());
        for window in windows {
            let _: Id = unsafe { msg_send![window, retain] };
            hidden.push(window as usize);
        }
    }

    extern "C" fn did_hide(
        _: CFNotificationCenterRef,
        _: *mut c_void,
        _: CFNotificationName,
        _: *const c_void,
        _: CFDictionaryRef,
    ) {
        let hidden = HIDDEN.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            // Ordered out while hidden, a window isn't brought back by the unhide.
            for &window in hidden.iter() {
                let _: () = msg_send![window as Id, orderOut: ptr::null::<Object>()];
            }
            let app: Id = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, unhideWithoutActivation];
        }
    }

    extern "C" fn did_become_active(
        _: CFNotificationCenterRef,
        _: *mut c_void,
        _: CFNotificationName,
        _: *const c_void,
        _: CFDictionaryRef,
    ) {
        let windows = std::mem::take(&mut *HIDDEN.lock().unwrap_or_else(|e| e.into_inner()));
        unsafe {
            // Back in their order, the frontmost one last and key.
            for &window in windows.iter().rev() {
                let _: () = msg_send![window as Id, makeKeyAndOrderFront: ptr::null::<Object>()];
                let _: () = msg_send![window as Id, release];
            }
        }
    }
}
