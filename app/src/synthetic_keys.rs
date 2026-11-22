//! Synthetic keyboard event posting for the auto-paste pipeline.
//!
//! `send_paste` fires the four-event paste sequence onto the OS input
//! pipeline so the focused app performs its native paste action against
//! whatever the clipboard module has just staged.
//!
//! - **macOS** — Cmd down with Cmd flag, V down with Cmd flag, V up with
//!   Cmd flag, Cmd up via `CGEventPost` at `kCGHIDEventTap`. The Cmd-down
//!   event carries the Command flag so its `flagsChanged` representation
//!   matches hardware — Electron/Chromium tracks modifier state from that
//!   flag and drops the paste otherwise (see the note on the event table).
//!   Accessibility permission is load-bearing: without it the system
//!   swallows the events silently, so callers must gate on
//!   [`crate::accessibility::is_trusted`].
//!
//! The V keycode is resolved per-layout by
//! [`crate::keyboard_layout`] — Cmd+V is matched against the layout-
//! translated character via NSMenu key equivalents, so hardcoding
//! `kVK_ANSI_V` (the QWERTY V position) would fire Cmd+. on Dvorak. The
//! resolved keycode is read once per paste from an atomic; the cache is
//! primed at startup and refreshed on layout change.
//!
//! [`KeySource`] posts the text-carrying key events of the keystroke
//! inserter (`crate::keystroke_insert`). Every event it posts is tagged with
//! [`SYNTHETIC_EVENT_TAG`] in `kCGEventSourceUserData`, so an event tap can
//! tell Kass's typing apart from the user's.

use std::ffi::c_void;

/// Value written to `kCGEventSourceUserData` (field 42) on every event
/// [`KeySource`] posts: ASCII "Kass". An event tap that reads the field
/// can skip Kass's own typing.
pub const SYNTHETIC_EVENT_TAG: i64 = 0x566F_6963_6562_6F78;

mod ffi {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct CGEvent {
        _opaque: [u8; 0],
    }
    pub type CGEventRef = *mut CGEvent;

    #[repr(C)]
    pub struct CGEventSource {
        _opaque: [u8; 0],
    }
    pub type CGEventSourceRef = *mut CGEventSource;

    pub type CGEventTapLocation = u32;
    pub type CGKeyCode = u16;
    pub type CGEventFlags = u64;
    pub type CGEventSourceStateID = i32;
    pub type CGEventField = u32;

    /// `kCGEventSourceUserData`.
    pub const K_CG_EVENT_SOURCE_USER_DATA: CGEventField = 42;

    /// `kCGHIDEventTap` — posted events enter at the HID level so every
    /// downstream tap (including the target app) sees them exactly as if the
    /// hardware had produced them.
    pub const K_CG_HID_EVENT_TAP: CGEventTapLocation = 0;

    /// `kCGEventSourceStateHIDSystemState` — mimics hardware, which is what
    /// we want: modifier bookkeeping inside target apps stays consistent.
    pub const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: CGEventSourceStateID = 1;

    /// `kCGEventFlagMaskCommand` — the Cmd modifier bit inside `CGEventFlags`.
    pub const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x00100000;

    /// `kVK_Command` (left Cmd).
    pub const KEYCODE_LEFT_CMD: CGKeyCode = 0x37;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventSourceCreate(state_id: CGEventSourceStateID) -> CGEventSourceRef;
        pub fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            virtual_key: CGKeyCode,
            key_down: bool,
        ) -> CGEventRef;
        pub fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
        pub fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
        pub fn CGEventKeyboardSetUnicodeString(
            event: CGEventRef,
            length: std::ffi::c_ulong,
            string: *const u16,
        );
        pub fn CGEventSetIntegerValueField(event: CGEventRef, field: CGEventField, value: i64);
        pub fn CGEventSourceSetUserData(source: CGEventSourceRef, user_data: i64);
        pub fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> CGEventFlags;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: *const c_void);
    }
}

/// Post the four-event Cmd+V sequence to the HID event tap.
///
/// Returns after the events are queued — there's no completion callback,
/// so callers should sleep briefly afterwards to let the target app
/// process the paste before any follow-up (e.g. clipboard restore).
pub fn send_paste() -> Result<(), String> {
    send_command_key(crate::keyboard_layout::paste_keycode_v())
}

/// Post Cmd+C the same way as [`send_paste`]: the copy that reads a
/// selection Accessibility can't (Command Mode). Every event sets its flags
/// explicitly, so the command chord's keys, still held, can't turn it into
/// another shortcut.
pub fn send_copy() -> Result<(), String> {
    send_command_key(crate::keyboard_layout::copy_keycode_c())
}

/// Cmd down, `key_code` down and up with Cmd, Cmd up.
fn send_command_key(key_code: u16) -> Result<(), String> {
    use ffi::*;

    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE);
        if source.is_null() {
            return Err("CGEventSourceCreate returned null".into());
        }
        let _source_guard = scopeguard::guard(source, |s| CFRelease(s as *const c_void));

        let events = [
            // The Cmd-down event must carry the Command flag itself. On real
            // hardware the Cmd keyDown is a flagsChanged event whose flags
            // already include Command; Chromium/Electron builds its tracked
            // modifier state from that flag. Posting Cmd-down with flags = 0
            // leaves that tracker showing "Command up", so the following V —
            // even though its own flags carry Command — matches neither the
            // Cmd+V accelerator (tracker says no modifier) nor plain-text
            // insertion (event flags say Command), and Electron drops it
            // silently. AppKit reads the V event's own flags and pastes
            // regardless, which is why native apps worked but Electron
            // targets (Slack, VS Code) silently no-op'd.
            (KEYCODE_LEFT_CMD, true, K_CG_EVENT_FLAG_MASK_COMMAND),
            (key_code, true, K_CG_EVENT_FLAG_MASK_COMMAND),
            (key_code, false, K_CG_EVENT_FLAG_MASK_COMMAND),
            (KEYCODE_LEFT_CMD, false, 0),
        ];

        // Build the four events up front so CFRelease happens after all posts.
        // Posting in a loop that interleaved create → post → release would
        // work, but keeping the events alive for the full sequence matches
        // the pattern CGEventPost's docs show and is easier to reason about.
        let mut guards = Vec::with_capacity(events.len());
        let mut created = Vec::with_capacity(events.len());

        for (key, down, flags) in events {
            let event = CGEventCreateKeyboardEvent(source, key, down);
            if event.is_null() {
                return Err(format!(
                    "CGEventCreateKeyboardEvent(key={}, down={}) returned null",
                    key, down
                ));
            }
            let guard = scopeguard::guard(event, |e| CFRelease(e as *const c_void));
            if flags != 0 {
                CGEventSetFlags(event, flags);
            }
            created.push(event);
            guards.push(guard);
        }

        for event in created {
            CGEventPost(K_CG_HID_EVENT_TAP, event);
        }

        drop(guards);
        Ok(())
    }
}

/// Modifier state of the keyboard hardware right now (`CGEventFlags` bits),
/// independent of any app's view of it.
pub fn hardware_modifier_flags() -> u64 {
    unsafe { ffi::CGEventSourceFlagsState(ffi::K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE) }
}

/// A dedicated event source for typed text. Its events carry
/// [`SYNTHETIC_EVENT_TAG`].
pub struct KeySource(ffi::CGEventSourceRef);

impl KeySource {
    pub fn new() -> Result<Self, String> {
        use ffi::*;
        unsafe {
            // HID system state, like `send_paste`: posted events look like
            // hardware to the target app.
            let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE);
            if source.is_null() {
                return Err("CGEventSourceCreate returned null".into());
            }
            CGEventSourceSetUserData(source, SYNTHETIC_EVENT_TAG);
            Ok(Self(source))
        }
    }

    /// Create and post one key event at the HID tap. `flags` is always set,
    /// even when 0, so held-modifier state cannot leak into the event.
    /// `text`, when given, replaces what the keycode would have typed; macOS
    /// honours at most 20 UTF-16 units per event.
    pub fn post(
        &self,
        keycode: u16,
        down: bool,
        flags: u64,
        text: Option<&[u16]>,
    ) -> Result<(), String> {
        use ffi::*;
        unsafe {
            let event = CGEventCreateKeyboardEvent(self.0, keycode, down);
            if event.is_null() {
                return Err(format!(
                    "CGEventCreateKeyboardEvent(key={keycode}, down={down}) returned null"
                ));
            }
            let _guard = scopeguard::guard(event, |e| CFRelease(e as *const c_void));
            CGEventSetFlags(event, flags);
            if let Some(units) = text {
                CGEventKeyboardSetUnicodeString(
                    event,
                    units.len() as std::ffi::c_ulong,
                    units.as_ptr(),
                );
            }
            CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_TAG);
            CGEventPost(K_CG_HID_EVENT_TAP, event);
            Ok(())
        }
    }
}

impl Drop for KeySource {
    fn drop(&mut self) {
        unsafe { ffi::CFRelease(self.0 as *const c_void) }
    }
}
