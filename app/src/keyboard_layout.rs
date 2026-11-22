//! Layout-aware resolution of the keycode whose current-layout translation
//! is `'v'`. Drives [`crate::synthetic_keys::send_paste`] so the synthetic
//! Cmd+V it posts is interpreted as Paste by the focused app regardless of
//! the user's active keyboard layout (Dvorak, Colemak, AZERTY, …).
//!
//! macOS apps process Cmd+V via NSMenu key equivalents, which match against
//! `[NSEvent charactersIgnoringModifiers]` — i.e. the layout-translated
//! character, not the raw keycode. Posting `kVK_ANSI_V` (= 9, the QWERTY V
//! position) on Dvorak therefore produces Cmd+. and never triggers Paste.
//!
//! All TIS calls happen on the main thread: once at startup via [`init`]
//! from the app's launch in `main`, and again from the
//! `kTISNotifySelectedKeyboardInputSourceChanged` distributed notification
//! (delivered to the main runloop). The hot path ([`paste_keycode_v`])
//! only reads an [`AtomicU16`], so paste latency is unchanged.
//!
//! The copy key (`'c'`) is resolved the same way, for the ⌘C that reads a
//! selection Accessibility can't (Command Mode, docs/plans/COMMAND_MODE.md).
//!
//! The same main-thread refresh also caches whether the selected keyboard
//! input source is ASCII-capable ([`input_source_is_ascii_capable`]), which
//! the keystroke inserter reads off the main thread: with an IME such as
//! Japanese or Chinese active, typed text is routed through the IME and can
//! come out converted.

use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};

/// `kVK_ANSI_V` — the keycode for the physical V key on a US QWERTY
/// layout. Used as the fallback whenever live resolution can't produce a
/// better answer (no Unicode key layout data, lookup failure).
const FALLBACK_V_KEYCODE: u16 = 9;

static V_KEYCODE: AtomicU16 = AtomicU16::new(FALLBACK_V_KEYCODE);

/// `kVK_ANSI_C`, the fallback for [`copy_keycode_c`].
const FALLBACK_C_KEYCODE: u16 = 8;

static C_KEYCODE: AtomicU16 = AtomicU16::new(FALLBACK_C_KEYCODE);

/// Returns the keycode whose current-layout translation is `'v'`. Falls
/// back to `kVK_ANSI_V` when resolution hasn't run, the active input
/// source carries no Unicode key layout data, or no keycode in the layout
/// produces `v`.
pub fn paste_keycode_v() -> u16 {
    V_KEYCODE.load(Ordering::Relaxed)
}

/// [`paste_keycode_v`] for `'c'`, falling back to `kVK_ANSI_C`.
pub fn copy_keycode_c() -> u16 {
    C_KEYCODE.load(Ordering::Relaxed)
}

/// Whether the selected keyboard input source is ASCII-capable: 0 unknown
/// (never resolved), 1 no, 2 yes.
static ASCII_CAPABLE: AtomicU8 = AtomicU8::new(0);

/// Whether the selected keyboard input source can type ASCII directly (a
/// plain layout, or an IME in its Roman mode). `None` until [`init`] has run
/// or when the property could not be read.
pub fn input_source_is_ascii_capable() -> Option<bool> {
    match ASCII_CAPABLE.load(Ordering::Relaxed) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

pub fn init() {
    macos::init();
}

mod macos {
    use super::{ASCII_CAPABLE, C_KEYCODE, FALLBACK_C_KEYCODE, FALLBACK_V_KEYCODE, V_KEYCODE};
    use core_foundation_sys::base::CFRelease;
    use core_foundation_sys::data::{CFDataGetBytePtr, CFDataRef};
    use core_foundation_sys::dictionary::CFDictionaryRef;
    use core_foundation_sys::notification_center::{
        CFNotificationCenterAddObserver, CFNotificationCenterGetDistributedCenter,
        CFNotificationCenterRef, CFNotificationName,
        CFNotificationSuspensionBehaviorDeliverImmediately,
    };
    use core_foundation_sys::number::{CFBooleanGetValue, CFBooleanRef};
    use core_foundation_sys::string::CFStringRef;
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::Ordering;

    type TISInputSourceRef = *mut c_void;

    /// `kUCKeyActionDown`.
    const K_UC_KEY_ACTION_DOWN: u16 = 0;
    /// `kUCKeyTranslateNoDeadKeysMask` — collapse dead-key state machine so
    /// a single call gives us the bare character. V is never a dead key on
    /// any layout we care about, but the flag costs nothing and removes
    /// any chance of ambiguous output.
    const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK: u32 = 1;
    /// Standard US-style virtual keycodes occupy 0..0x7F. We iterate the
    /// full range so non-US-extended layouts (ISO, JIS) can still be
    /// resolved if their `v` lives outside the ANSI range.
    const MAX_KEYCODE: u16 = 127;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> TISInputSourceRef;
        fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
        fn TISGetInputSourceProperty(source: TISInputSourceRef, key: CFStringRef) -> *mut c_void;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            keyboard_layout: *const u8,
            virtual_key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            key_translate_options: u32,
            dead_key_state: *mut u32,
            max_string_length: usize,
            actual_string_length: *mut usize,
            unicode_string: *mut u16,
        ) -> i32;

        static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
        static kTISPropertyInputSourceIsASCIICapable: CFStringRef;
        static kTISNotifySelectedKeyboardInputSourceChanged: CFStringRef;
    }

    pub fn init() {
        resolve_into_cache();
        register_layout_change_observer();
    }

    fn resolve_into_cache() {
        let kc = resolve_keycode(b'v').unwrap_or(FALLBACK_V_KEYCODE);
        V_KEYCODE.store(kc, Ordering::Relaxed);
        let kc = resolve_keycode(b'c').unwrap_or(FALLBACK_C_KEYCODE);
        C_KEYCODE.store(kc, Ordering::Relaxed);
        let ascii = match resolve_ascii_capable() {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        };
        ASCII_CAPABLE.store(ascii, Ordering::Relaxed);
    }

    /// Read `kTISPropertyInputSourceIsASCIICapable` of the selected input
    /// source (the IME itself when one is active, not its underlying layout).
    fn resolve_ascii_capable() -> Option<bool> {
        unsafe {
            let source = TISCopyCurrentKeyboardInputSource();
            if source.is_null() {
                return None;
            }
            let _src_guard = scopeguard::guard(source, |s| CFRelease(s as *const c_void));
            let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceIsASCIICapable);
            if value.is_null() {
                return None;
            }
            Some(CFBooleanGetValue(value as CFBooleanRef))
        }
    }

    /// The keycode whose current-layout translation is `target`.
    fn resolve_keycode(target: u8) -> Option<u16> {
        unsafe {
            let source = TISCopyCurrentKeyboardLayoutInputSource();
            if source.is_null() {
                return None;
            }
            let _src_guard = scopeguard::guard(source, |s| CFRelease(s as *const c_void));

            let layout_data_ptr =
                TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            if layout_data_ptr.is_null() {
                return None;
            }
            let layout_bytes = CFDataGetBytePtr(layout_data_ptr as CFDataRef);
            if layout_bytes.is_null() {
                return None;
            }

            let kbd_type = LMGetKbdType() as u32;

            for keycode in 0..=MAX_KEYCODE {
                let mut dead_key_state: u32 = 0;
                let mut chars: [u16; 4] = [0; 4];
                let mut actual_len: usize = 0;
                let status = UCKeyTranslate(
                    layout_bytes,
                    keycode,
                    K_UC_KEY_ACTION_DOWN,
                    0, // no modifiers
                    kbd_type,
                    K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK,
                    &mut dead_key_state,
                    chars.len(),
                    &mut actual_len,
                    chars.as_mut_ptr(),
                );
                if status == 0 && actual_len == 1 && chars[0] == target as u16 {
                    return Some(keycode);
                }
            }
            None
        }
    }

    extern "C" fn layout_changed(
        _center: CFNotificationCenterRef,
        _observer: *mut c_void,
        _name: CFNotificationName,
        _object: *const c_void,
        _user_info: CFDictionaryRef,
    ) {
        resolve_into_cache();
    }

    fn register_layout_change_observer() {
        unsafe {
            let center = CFNotificationCenterGetDistributedCenter();
            if center.is_null() {
                return;
            }
            CFNotificationCenterAddObserver(
                center,
                ptr::null(),
                layout_changed,
                kTISNotifySelectedKeyboardInputSourceChanged,
                ptr::null(),
                CFNotificationSuspensionBehaviorDeliverImmediately,
            );
        }
    }
}
