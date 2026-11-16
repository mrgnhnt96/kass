//! Chords: keycaps, and the picker that records one from the keyboard.
//!
//! The chord engine tells left and right modifiers apart, and GPUI's key
//! events don't, so the picker reads AppKit's events itself through a local
//! monitor while it's open. Keys are saved under the chord engine's names
//! ("MetaRight", "AltGr", "KeyA"), the same names the React picker saved.

use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use futures_util::StreamExt as _;
use gpui_kit::prelude::*;
use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
use kass_ui::{close_dialog, dialog_footer, dialog_header, metrics, open_dialog, theme, Button};

/// The chords dictation starts with: the right ⌘ and ⌥, plus Space for
/// hands-free.
pub fn default_chord(toggle: bool) -> Vec<String> {
    let mut keys = vec!["MetaRight".to_string(), "AltGr".to_string()];
    if toggle {
        keys.push("Space".to_string());
    }
    keys
}

/// Where a key sorts in a chord: modifiers first (⌃ ⌥ ⇧ ⌘), the rest after.
fn rank(key: &str) -> u8 {
    match key {
        "ControlLeft" | "ControlRight" => 0,
        "Alt" | "AltGr" => 1,
        "ShiftLeft" | "ShiftRight" => 2,
        "MetaLeft" | "MetaRight" => 3,
        "Function" => 4,
        "CapsLock" => 5,
        _ => 99,
    }
}

/// `keys` in the order shortcuts are written: modifiers, then the key.
pub fn sorted(keys: &[String]) -> Vec<String> {
    let mut keys = keys.to_vec();
    keys.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    keys
}

/// Whether two chords are the same keys, in any order.
pub fn same_chord(a: &[String], b: &[String]) -> bool {
    sorted(a) == sorted(b)
}

/// What a key's cap says.
pub fn label(key: &str) -> String {
    let glyph = match key {
        "MetaLeft" | "MetaRight" => "⌘",
        "Alt" | "AltGr" => "⌥",
        "ControlLeft" | "ControlRight" => "⌃",
        "ShiftLeft" | "ShiftRight" => "⇧",
        "CapsLock" => "⇪",
        "Function" => "fn",
        "Tab" => "⇥",
        "Return" => "↵",
        "Backspace" => "⌫",
        "Delete" => "⌦",
        "Escape" => "Esc",
        "UpArrow" => "↑",
        "DownArrow" => "↓",
        "LeftArrow" => "←",
        "RightArrow" => "→",
        _ => {
            for prefix in ["Key", "Digit", "Num"] {
                if let Some(rest) = key.strip_prefix(prefix) {
                    if rest.len() == 1 {
                        return rest.to_string();
                    }
                }
            }
            return key.to_string();
        }
    };
    glyph.to_string()
}

/// "L" or "R" for a modifier that has two keys.
pub fn side(key: &str) -> Option<&'static str> {
    match key {
        "MetaRight" | "AltGr" | "ControlRight" | "ShiftRight" => Some("R"),
        "MetaLeft" | "Alt" | "ControlLeft" | "ShiftLeft" => Some("L"),
        _ => None,
    }
}

fn is_modifier(key: &str) -> bool {
    rank(key) < 99
}

/// A chord as keycaps, small for a settings row or large for the picker,
/// each modifier badged with its side.
pub fn chord_keys(keys: &[String], large: bool, cx: &App) -> AnyElement {
    let t = theme(cx);
    if keys.is_empty() {
        return div()
            .font_family(t.mono_font.clone())
            .text_size(metrics::TEXT_XS)
            .text_color(t.muted_foreground)
            .child(kass_i18n::t("captures-chord-not-set"))
            .into_any_element();
    }
    div()
        .flex()
        .items_center()
        .gap(px(if large { 8. } else { 4. }))
        .children(keys.iter().map(|key| {
            let cap = div()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(t.input)
                .bg(t.secondary)
                .font_family(t.mono_font.clone())
                .text_color(t.foreground);
            let cap = if large {
                cap.min_w(px(40.))
                    .rounded(metrics::RADIUS_LG)
                    .border_b(px(3.))
                    .px(px(14.))
                    .py(px(8.))
                    .text_size(metrics::TEXT_LG)
                    .line_height(metrics::LINE_LG)
            } else {
                cap.min_w(px(28.))
                    .rounded(px(5.))
                    .border_b(px(2.))
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(metrics::TEXT_XS)
                    .line_height(metrics::LINE_XS)
            };
            cap.child(label(key)).when_some(side(key), |cap, side| {
                let offset = px(if large { -7. } else { -6. });
                cap.child(
                    div()
                        .absolute()
                        .top(offset)
                        .right(offset)
                        .px(px(if large { 4. } else { 2. }))
                        .py(px(1.))
                        .rounded(metrics::RADIUS_SM)
                        .bg(t.accent)
                        .text_color(t.accent_foreground)
                        .text_size(px(if large { 9. } else { 8. }))
                        .line_height(px(if large { 9. } else { 8. }))
                        .child(side),
                )
            })
        }))
        .into_any_element()
}

/// One key event from the picker's monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Down(&'static str),
    Up(&'static str),
    /// A key chords can't use, by what it types or its code.
    Unsupported(String),
}

/// The chord name of a key, by its macOS virtual key code (ANSI layout
/// positions, so a key keeps its name whatever the layout types).
pub fn key_for_code(code: u16) -> Option<&'static str> {
    Some(match code {
        0x00 => "KeyA",
        0x01 => "KeyS",
        0x02 => "KeyD",
        0x03 => "KeyF",
        0x04 => "KeyH",
        0x05 => "KeyG",
        0x06 => "KeyZ",
        0x07 => "KeyX",
        0x08 => "KeyC",
        0x09 => "KeyV",
        0x0B => "KeyB",
        0x0C => "KeyQ",
        0x0D => "KeyW",
        0x0E => "KeyE",
        0x0F => "KeyR",
        0x10 => "KeyY",
        0x11 => "KeyT",
        0x12 => "Digit1",
        0x13 => "Digit2",
        0x14 => "Digit3",
        0x15 => "Digit4",
        0x16 => "Digit6",
        0x17 => "Digit5",
        0x18 => "Equal",
        0x19 => "Digit9",
        0x1A => "Digit7",
        0x1B => "Minus",
        0x1C => "Digit8",
        0x1D => "Digit0",
        0x1E => "RightBracket",
        0x1F => "KeyO",
        0x20 => "KeyU",
        0x21 => "LeftBracket",
        0x22 => "KeyI",
        0x23 => "KeyP",
        0x24 => "Return",
        0x25 => "KeyL",
        0x26 => "KeyJ",
        0x27 => "Quote",
        0x28 => "KeyK",
        0x29 => "SemiColon",
        0x2A => "BackSlash",
        0x2B => "Comma",
        0x2C => "Slash",
        0x2D => "KeyN",
        0x2E => "KeyM",
        0x2F => "Dot",
        0x30 => "Tab",
        0x31 => "Space",
        0x32 => "BackQuote",
        0x33 => "Backspace",
        0x35 => "Escape",
        0x36 => "MetaRight",
        0x37 => "MetaLeft",
        0x38 => "ShiftLeft",
        0x39 => "CapsLock",
        0x3A => "Alt",
        0x3B => "ControlLeft",
        0x3C => "ShiftRight",
        0x3D => "AltGr",
        0x3E => "ControlRight",
        0x3F => "Function",
        0x60 => "F5",
        0x61 => "F6",
        0x62 => "F7",
        0x63 => "F3",
        0x64 => "F8",
        0x65 => "F9",
        0x67 => "F11",
        0x6D => "F10",
        0x6F => "F12",
        0x72 => "Insert",
        0x73 => "Home",
        0x74 => "PageUp",
        0x75 => "Delete",
        0x76 => "F4",
        0x77 => "End",
        0x78 => "F2",
        0x79 => "PageDown",
        0x7A => "F1",
        0x7B => "LeftArrow",
        0x7C => "RightArrow",
        0x7D => "DownArrow",
        0x7E => "UpArrow",
        _ => return None,
    })
}

/// The device-dependent flag bit that says whether a modifier key is down
/// (`NX_DEVICE*KEYMASK`), so the left and right keys read apart.
fn modifier_bit(key: &str) -> Option<usize> {
    Some(match key {
        "ControlLeft" => 0x0001,
        "ShiftLeft" => 0x0002,
        "ShiftRight" => 0x0004,
        "MetaLeft" => 0x0008,
        "MetaRight" => 0x0010,
        "Alt" => 0x0020,
        "AltGr" => 0x0040,
        "ControlRight" => 0x2000,
        "Function" => NSEventModifierFlags::Function.bits(),
        _ => return None,
    })
}

/// What a modifier change means: the key went down or up. Caps Lock only
/// says it toggled, so it counts as a tap.
pub fn flags_events(code: u16, flags: usize) -> Vec<KeyEvent> {
    let Some(key) = key_for_code(code) else {
        return Vec::new();
    };
    if key == "CapsLock" {
        return vec![KeyEvent::Down(key), KeyEvent::Up(key)];
    }
    match modifier_bit(key) {
        Some(bit) if flags & bit != 0 => vec![KeyEvent::Down(key)],
        Some(_) => vec![KeyEvent::Up(key)],
        None => Vec::new(),
    }
}

/// The picker's record of a session: what's held and the biggest chord
/// held so far, which Save keeps once the keys are let go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capture {
    pressed: Vec<String>,
    captured: Vec<String>,
    unsupported: Option<String>,
    pressed_any: bool,
}

impl Capture {
    pub fn new(initial: Vec<String>) -> Self {
        Self {
            captured: initial,
            ..Default::default()
        }
    }

    pub fn apply(&mut self, event: KeyEvent) {
        match event {
            KeyEvent::Down(key) => self.press(key),
            KeyEvent::Up(key) => self.release(key),
            KeyEvent::Unsupported(key) => self.unsupported = Some(key),
        }
    }

    fn press(&mut self, key: &str) {
        self.unsupported = None;
        if self.pressed.iter().any(|k| k == key) {
            return;
        }
        // The first key of a fresh press starts over, so a shorter chord can
        // replace a longer saved one.
        let fresh = self.pressed.is_empty();
        self.pressed.push(key.to_string());
        self.pressed_any = true;
        let candidate = sorted(&self.pressed);
        if fresh || candidate.len() >= self.captured.len() {
            self.captured = candidate;
        }
    }

    fn release(&mut self, key: &str) {
        self.pressed.retain(|k| k != key);
        // macOS sends no key-up for a key let go while ⌘ is down, so once ⌘
        // is up the other keys can't be told apart from held ones.
        if key == "MetaLeft" || key == "MetaRight" {
            let meta_down = self
                .pressed
                .iter()
                .any(|k| k == "MetaLeft" || k == "MetaRight");
            if !meta_down {
                self.pressed.retain(|k| is_modifier(k));
            }
        }
    }

    /// The keys to show: what's held, or else what was captured.
    pub fn shown(&self) -> Vec<String> {
        if self.pressed.is_empty() {
            self.captured.clone()
        } else {
            sorted(&self.pressed)
        }
    }

    pub fn captured(&self) -> &[String] {
        &self.captured
    }

    pub fn status(&self) -> &'static str {
        if !self.pressed.is_empty() {
            "captures-chord-capturing"
        } else if self.pressed_any && !self.captured.is_empty() {
            "captures-chord-captured"
        } else {
            "captures-chord-press-shortcut"
        }
    }

    pub fn unsupported(&self) -> Option<&str> {
        self.unsupported.as_deref()
    }
}

/// An AppKit local monitor on the app's key events, removed when dropped.
/// Key presses it records don't reach the window; Escape and Tab pass
/// through so the dialog still closes and moves focus.
struct KeyMonitor(Option<Retained<AnyObject>>);

impl KeyMonitor {
    fn start(send: impl Fn(KeyEvent) + 'static) -> Self {
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the monitor a valid event for the call.
            let ns = unsafe { event.as_ref() };
            let ty = ns.r#type();
            if ty == NSEventType::FlagsChanged {
                for e in flags_events(ns.keyCode(), ns.modifierFlags().bits()) {
                    send(e);
                }
                return event.as_ptr();
            }
            let code = ns.keyCode();
            let key = key_for_code(code);
            if matches!(key, Some("Escape" | "Tab")) {
                return event.as_ptr();
            }
            let down = ty == NSEventType::KeyDown;
            match key {
                Some(key) if down => send(KeyEvent::Down(key)),
                Some(key) => send(KeyEvent::Up(key)),
                None if down => {
                    let typed = ns
                        .charactersIgnoringModifiers()
                        .map(|s| s.to_string())
                        .filter(|s| !s.trim().is_empty() && !s.chars().any(char::is_control));
                    send(KeyEvent::Unsupported(typed.unwrap_or_else(|| format!("0x{code:02X}"))));
                }
                None => {}
            }
            std::ptr::null_mut()
        });
        let mask = NSEventMask::KeyDown | NSEventMask::KeyUp | NSEventMask::FlagsChanged;
        // SAFETY: the handler returns the event it was given or null.
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        Self(monitor)
    }
}

impl Drop for KeyMonitor {
    fn drop(&mut self) {
        if let Some(monitor) = self.0.take() {
            // SAFETY: `monitor` is what addLocalMonitor… returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    }
}

type OnSave = Rc<dyn Fn(Vec<String>, &mut Window, &mut App)>;

/// The picker's dialog body. It lives as long as the dialog shows it, and
/// its monitor with it.
pub struct ChordPicker {
    title: SharedString,
    description: SharedString,
    capture: Capture,
    on_save: OnSave,
    _monitor: KeyMonitor,
    _events: Task<()>,
}

impl ChordPicker {
    fn new(
        title: SharedString,
        description: SharedString,
        initial: Vec<String>,
        on_save: OnSave,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, mut rx) = futures_channel::mpsc::unbounded();
        let monitor = KeyMonitor::start(move |event| {
            let _ = tx.unbounded_send(event);
        });
        let events = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Some(event) = rx.next().await {
                let applied = this.update(cx, |this, cx| {
                    this.capture.apply(event);
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        Self {
            title,
            description,
            capture: Capture::new(initial),
            on_save,
            _monitor: monitor,
            _events: events,
        }
    }
}

/// Open the picker on `initial`; `on_save` gets the chord when it's saved.
pub fn open_chord_picker(
    title: String,
    description: String,
    initial: Vec<String>,
    on_save: impl Fn(Vec<String>, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let on_save: OnSave = Rc::new(on_save);
    let picker = cx.new(|cx| ChordPicker::new(title.into(), description.into(), initial, on_save, cx));
    open_dialog(window, cx, move |_, _| picker.clone().into_any_element());
}

impl Render for ChordPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let shown = self.capture.shown();
        let captured = self.capture.captured().to_vec();
        let on_save = self.on_save.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(dialog_header(self.title.clone(), Some(self.description.clone()), cx))
            .child(
                div()
                    .min_h(px(110.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .px_4()
                    .py_5()
                    .rounded(px(10.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.accent)
                    .bg(t.accent.opacity(0.05))
                    .child(if shown.is_empty() {
                        div()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_SM)
                            .text_color(t.muted_foreground)
                            .child(kass_i18n::t("captures-chord-no-keys"))
                            .into_any_element()
                    } else {
                        chord_keys(&shown, true, cx)
                    })
                    .child(
                        div()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_CAPTION)
                            .text_color(t.accent)
                            .child(kass_i18n::t(self.capture.status())),
                    )
                    .when_some(self.capture.unsupported(), |area, key| {
                        area.child(
                            div()
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.destructive)
                                .child(kass_i18n::tr!("captures-chord-unsupported", key = key)),
                        )
                    }),
            )
            .child(
                dialog_footer()
                    .child(
                        Button::new("chord-cancel")
                            .outline()
                            .label(kass_i18n::t("common-cancel"))
                            .on_click(|_, window, cx| close_dialog(window, cx)),
                    )
                    .child(
                        Button::new("chord-save")
                            .label(kass_i18n::t("captures-chord-save"))
                            .disabled(captured.is_empty())
                            .on_click(move |_, window, cx| {
                                on_save(captured.clone(), window, cx);
                                close_dialog(window, cx);
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn sorts_modifiers_first() {
        assert_eq!(
            sorted(&keys(&["Space", "MetaRight", "AltGr", "ControlLeft"])),
            keys(&["ControlLeft", "AltGr", "MetaRight", "Space"])
        );
        assert_eq!(sorted(&keys(&["KeyB", "KeyA"])), keys(&["KeyA", "KeyB"]));
    }

    #[test]
    fn same_chord_ignores_order() {
        assert!(same_chord(&keys(&["AltGr", "MetaRight"]), &default_chord(false)));
        assert!(!same_chord(&keys(&["Alt", "MetaRight"]), &default_chord(false)));
        assert!(!same_chord(&default_chord(true), &default_chord(false)));
    }

    #[test]
    fn labels_and_sides() {
        assert_eq!(label("MetaRight"), "⌘");
        assert_eq!(label("AltGr"), "⌥");
        assert_eq!(label("KeyK"), "K");
        assert_eq!(label("Digit7"), "7");
        assert_eq!(label("Num7"), "7");
        assert_eq!(label("F5"), "F5");
        assert_eq!(label("Space"), "Space");
        assert_eq!(side("AltGr"), Some("R"));
        assert_eq!(side("Alt"), Some("L"));
        assert_eq!(side("Space"), None);
    }

    #[test]
    fn key_codes_name_both_sides() {
        assert_eq!(key_for_code(0x37), Some("MetaLeft"));
        assert_eq!(key_for_code(0x36), Some("MetaRight"));
        assert_eq!(key_for_code(0x3A), Some("Alt"));
        assert_eq!(key_for_code(0x3D), Some("AltGr"));
        assert_eq!(key_for_code(0x31), Some("Space"));
        assert_eq!(key_for_code(0x00), Some("KeyA"));
        assert_eq!(key_for_code(0x7A), Some("F1"));
        assert_eq!(key_for_code(0x0A), None);
    }

    #[test]
    fn every_key_name_is_one_the_engine_reads() {
        for code in 0..=0x7F {
            if let Some(key) = key_for_code(code) {
                assert!(
                    crate::key_codes::key_from_str(key).is_some(),
                    "{key} (0x{code:02X})"
                );
            }
        }
    }

    #[test]
    fn modifier_flags_tell_sides_apart() {
        // Right ⌘ down: its device bit is set.
        assert_eq!(flags_events(0x36, 0x0010 | 0x10_0000), vec![KeyEvent::Down("MetaRight")]);
        // Left ⌘ let go while the right one is still held.
        assert_eq!(flags_events(0x37, 0x0010 | 0x10_0000), vec![KeyEvent::Up("MetaLeft")]);
        assert_eq!(
            flags_events(0x39, 0),
            vec![KeyEvent::Down("CapsLock"), KeyEvent::Up("CapsLock")]
        );
        assert_eq!(flags_events(0x00, 0), vec![]);
    }

    #[test]
    fn capture_keeps_the_peak() {
        let mut capture = Capture::new(default_chord(false));
        assert_eq!(capture.status(), "captures-chord-press-shortcut");
        capture.apply(KeyEvent::Down("MetaRight"));
        assert_eq!(capture.captured(), keys(&["MetaRight"]));
        capture.apply(KeyEvent::Down("AltGr"));
        capture.apply(KeyEvent::Down("Space"));
        assert_eq!(capture.status(), "captures-chord-capturing");
        capture.apply(KeyEvent::Up("Space"));
        assert_eq!(capture.shown(), keys(&["AltGr", "MetaRight"]));
        capture.apply(KeyEvent::Up("AltGr"));
        capture.apply(KeyEvent::Up("MetaRight"));
        assert_eq!(capture.shown(), keys(&["AltGr", "MetaRight", "Space"]));
        assert_eq!(capture.status(), "captures-chord-captured");
    }

    #[test]
    fn a_fresh_press_can_be_shorter() {
        let mut capture = Capture::new(default_chord(true));
        capture.apply(KeyEvent::Down("ControlRight"));
        capture.apply(KeyEvent::Up("ControlRight"));
        assert_eq!(capture.captured(), keys(&["ControlRight"]));
    }

    #[test]
    fn letting_go_of_command_clears_keys_without_key_up() {
        let mut capture = Capture::new(Vec::new());
        capture.apply(KeyEvent::Down("MetaLeft"));
        capture.apply(KeyEvent::Down("KeyK"));
        capture.apply(KeyEvent::Up("MetaLeft"));
        assert_eq!(capture.shown(), keys(&["MetaLeft", "KeyK"]));
        assert_eq!(capture.status(), "captures-chord-captured");
    }

    #[test]
    fn unsupported_clears_on_the_next_key() {
        let mut capture = Capture::new(Vec::new());
        capture.apply(KeyEvent::Unsupported("§".into()));
        assert_eq!(capture.unsupported(), Some("§"));
        capture.apply(KeyEvent::Down("KeyA"));
        assert_eq!(capture.unsupported(), None);
    }
}
