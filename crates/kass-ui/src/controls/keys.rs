//! Keyboard shortcuts as key caps. Keys go by the chord engine's names
//! ("MetaRight", "AltGr", "KeyA"), which tell left and right apart.

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::theme::{metrics, theme};

/// What a key's cap says: "⌘", "Space", "A".
pub fn key_label(name: &str) -> String {
    let label = match name {
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
            return ["Key", "Digit", "Num"]
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix))
                .filter(|rest| rest.chars().count() == 1)
                .unwrap_or(name)
                .to_string();
        }
    };
    label.to_string()
}

/// "L" or "R" for a modifier that has a key on each side.
pub fn key_side(name: &str) -> Option<&'static str> {
    match name {
        "MetaRight" | "AltGr" | "ControlRight" | "ShiftRight" => Some("R"),
        "MetaLeft" | "Alt" | "ControlLeft" | "ShiftLeft" => Some("L"),
        _ => None,
    }
}

fn order(name: &str) -> u8 {
    match name {
        "ControlLeft" | "ControlRight" => 0,
        "Alt" | "AltGr" => 1,
        "ShiftLeft" | "ShiftRight" => 2,
        "MetaLeft" | "MetaRight" => 3,
        "Function" => 4,
        "CapsLock" => 5,
        _ => 99,
    }
}

/// Whether `name` is a modifier (or Fn, Caps Lock) rather than a main key.
pub fn is_modifier(name: &str) -> bool {
    order(name) < 99
}

/// Modifiers first (⌃ ⌥ ⇧ ⌘), the main key last, as macOS lists them.
pub fn sort_chord(keys: &[String]) -> Vec<String> {
    let mut keys = keys.to_vec();
    keys.sort_by(|a, b| order(a).cmp(&order(b)).then_with(|| a.cmp(b)));
    keys
}

/// Whether two chords are the same keys, in any order.
pub fn same_chord(a: &[String], b: &[String]) -> bool {
    sort_chord(a) == sort_chord(b)
}

/// A chord as key caps, small for a settings row or large for a picker,
/// each modifier badged with its side. Draws nothing for no keys.
pub fn keycaps(keys: &[String], large: bool, cx: &App) -> Div {
    let t = theme(cx);
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
            cap.child(key_label(key))
                .when_some(key_side(key), |cap, side| {
                    let offset = px(if large { -7. } else { -6. });
                    let size = px(if large { 9. } else { 8. });
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
                            .text_size(size)
                            .line_height(size)
                            .child(side),
                    )
                })
        }))
}

#[cfg(test)]
mod tests {
    use super::{key_label, key_side, same_chord, sort_chord};

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn labels() {
        assert_eq!(key_label("MetaRight"), "⌘");
        assert_eq!(key_label("AltGr"), "⌥");
        assert_eq!(key_label("KeyK"), "K");
        assert_eq!(key_label("Digit7"), "7");
        assert_eq!(key_label("Num7"), "7");
        assert_eq!(key_label("F5"), "F5");
        assert_eq!(key_label("Space"), "Space");
        assert_eq!(key_label("Keypad"), "Keypad");
    }

    #[test]
    fn sides() {
        assert_eq!(key_side("AltGr"), Some("R"));
        assert_eq!(key_side("Alt"), Some("L"));
        assert_eq!(key_side("Space"), None);
    }

    #[test]
    fn sorts_modifiers_first() {
        assert_eq!(
            sort_chord(&keys(&["Space", "MetaRight", "AltGr", "ControlLeft"])),
            keys(&["ControlLeft", "AltGr", "MetaRight", "Space"])
        );
        assert_eq!(
            sort_chord(&keys(&["KeyB", "KeyA"])),
            keys(&["KeyA", "KeyB"])
        );
    }

    #[test]
    fn same_chord_ignores_order() {
        assert!(same_chord(
            &keys(&["AltGr", "MetaRight"]),
            &keys(&["MetaRight", "AltGr"])
        ));
        assert!(!same_chord(
            &keys(&["Alt", "MetaRight"]),
            &keys(&["AltGr", "MetaRight"])
        ));
    }
}
