//! The handshake's distributed notifications: their names, posting one with
//! its userInfo, and listening for replies.
//!
//! Nothing here depends on the rest of the crate, so the round-trip example
//! (`examples/handshake_roundtrip.rs`) uses this same file.

use std::ffi::c_void;
use std::sync::OnceLock;

use core_foundation_sys::base::{kCFAllocatorDefault, CFGetTypeID, CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreate,
    CFDictionaryGetValue, CFDictionaryRef,
};
use core_foundation_sys::notification_center::{
    kCFNotificationDeliverImmediately, CFNotificationCenterAddObserver,
    CFNotificationCenterGetDistributedCenter, CFNotificationCenterPostNotificationWithOptions,
    CFNotificationCenterRef, CFNotificationSuspensionBehaviorDeliverImmediately,
};
use core_foundation_sys::number::{
    kCFNumberSInt64Type, CFNumberCreate, CFNumberGetTypeID, CFNumberGetValue,
};
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringCreateWithBytes, CFStringGetCString, CFStringGetLength,
    CFStringRef,
};

/// Kass → app, at chord-down: `{pid, mode}`.
pub const WILL_BEGIN: &str = "com.mrgnhnt.kass.dictationWillBegin";
/// App → Kass, once its field is focused: `{pid}`.
pub const READY: &str = "com.mrgnhnt.kass.dictationReady";
/// Kass → app, when the take is over: `{pid, outcome}`.
pub const DID_END: &str = "com.mrgnhnt.kass.dictationDidEnd";
/// App → Kass, any time: `{pid}` takes part in the handshake.
pub const SUPPORTED: &str = "com.mrgnhnt.kass.handshakeSupported";

/// A userInfo value.
pub enum Value<'a> {
    Int(i64),
    Text(&'a str),
}

/// Called on the main thread with a notification's name and its
/// userInfo `pid`, when it has one.
pub type Handler = fn(name: &str, pid: Option<i64>);

static HANDLER: OnceLock<Handler> = OnceLock::new();
/// The observer's identity; only its address matters.
static OBSERVER: u8 = 0;

/// Post `name` to every app, delivered at once even to apps in the
/// background.
pub fn post(name: &str, info: &[(&str, Value)]) {
    unsafe {
        let Some(name) = cf_string(name) else {
            return;
        };
        let mut keys: Vec<CFTypeRef> = Vec::with_capacity(info.len());
        let mut values: Vec<CFTypeRef> = Vec::with_capacity(info.len());
        for (key, value) in info {
            let value: CFTypeRef = match value {
                Value::Int(n) => CFNumberCreate(
                    kCFAllocatorDefault,
                    kCFNumberSInt64Type,
                    n as *const i64 as *const c_void,
                ) as CFTypeRef,
                Value::Text(text) => match cf_string(text) {
                    Some(s) => s as CFTypeRef,
                    None => continue,
                },
            };
            match cf_string(key) {
                Some(key) => {
                    keys.push(key as CFTypeRef);
                    values.push(value);
                }
                None => CFRelease(value),
            }
        }
        let user_info = CFDictionaryCreate(
            kCFAllocatorDefault,
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        CFNotificationCenterPostNotificationWithOptions(
            CFNotificationCenterGetDistributedCenter(),
            name,
            std::ptr::null(),
            user_info,
            kCFNotificationDeliverImmediately,
        );
        // The dictionary retained its own copies.
        for object in keys.into_iter().chain(values) {
            CFRelease(object);
        }
        if !user_info.is_null() {
            CFRelease(user_info as CFTypeRef);
        }
        CFRelease(name as CFTypeRef);
    }
}

/// Observe `names` and hand each one to `handler`. Call on the main
/// thread: the distributed center delivers on the main run loop, whatever
/// thread registered. Only the first call listens.
pub fn listen(names: &[&str], handler: Handler) {
    if HANDLER.set(handler).is_err() {
        return;
    }
    unsafe {
        let center = CFNotificationCenterGetDistributedCenter();
        for name in names {
            observe(center, name);
        }
    }
}

unsafe fn observe(center: CFNotificationCenterRef, name: &str) {
    let Some(name) = cf_string(name) else {
        return;
    };
    CFNotificationCenterAddObserver(
        center,
        &OBSERVER as *const u8 as *const c_void,
        on_notification,
        name,
        std::ptr::null(),
        CFNotificationSuspensionBehaviorDeliverImmediately,
    );
    CFRelease(name as CFTypeRef);
}

extern "C" fn on_notification(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    name: CFStringRef,
    _object: *const c_void,
    user_info: CFDictionaryRef,
) {
    let Some(handler) = HANDLER.get() else {
        return;
    };
    let (name, pid) = unsafe { (rust_string(name), pid_of(user_info)) };
    if let Some(name) = name {
        handler(&name, pid);
    }
}

/// The `pid` number in a userInfo dictionary.
unsafe fn pid_of(user_info: CFDictionaryRef) -> Option<i64> {
    if user_info.is_null() {
        return None;
    }
    let key = cf_string("pid")?;
    let value = CFDictionaryGetValue(user_info, key as *const c_void);
    CFRelease(key as CFTypeRef);
    if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
        return None;
    }
    let mut pid: i64 = 0;
    let ok = CFNumberGetValue(
        value as _,
        kCFNumberSInt64Type,
        &mut pid as *mut i64 as *mut c_void,
    );
    ok.then_some(pid)
}

/// A `+1` CFString the caller releases.
unsafe fn cf_string(s: &str) -> Option<CFStringRef> {
    let result = CFStringCreateWithBytes(
        kCFAllocatorDefault,
        s.as_ptr(),
        s.len() as isize,
        kCFStringEncodingUTF8,
        0,
    );
    (!result.is_null()).then_some(result)
}

unsafe fn rust_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // UTF-16 units to UTF-8 bytes, plus the trailing NUL.
    let max_bytes = (CFStringGetLength(s) * 4 + 1) as usize;
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
    std::ffi::CStr::from_ptr(buf.as_ptr() as *const i8)
        .to_str()
        .ok()
        .map(str::to_owned)
}
