//! Chord keys as the Settings shortcut recorder names them, shown as keycaps.

/// What a keycap shows for a key code.
pub fn key_label(code: &str) -> String {
    let label = match code {
        "MetaLeft" | "MetaRight" => "⌘",
        "Alt" | "AltGr" => "⌥",
        "ControlLeft" | "ControlRight" => "⌃",
        "ShiftLeft" | "ShiftRight" => "⇧",
        "CapsLock" => "⇪",
        "Function" => "fn",
        "Space" => "Space",
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
            if let Some(letter) = code.strip_prefix("Key").filter(|k| !k.is_empty()) {
                return letter.to_string();
            }
            if let Some(digit) = code.strip_prefix("Num").filter(|k| !k.is_empty()) {
                return digit.to_string();
            }
            code
        }
    };
    label.to_string()
}

/// Which side of the keyboard a modifier is on, when the chord cares.
pub fn key_side(code: &str) -> Option<&'static str> {
    match code {
        "MetaRight" | "AltGr" | "ControlRight" | "ShiftRight" => Some("R"),
        "MetaLeft" | "Alt" | "ControlLeft" | "ShiftLeft" => Some("L"),
        _ => None,
    }
}

/// The push-to-talk chord when settings haven't loaded.
pub fn default_chord() -> Vec<String> {
    vec!["MetaRight".into(), "AltGr".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(key_label("MetaRight"), "⌘");
        assert_eq!(key_label("AltGr"), "⌥");
        assert_eq!(key_label("KeyD"), "D");
        assert_eq!(key_label("Num5"), "5");
        assert_eq!(key_label("F5"), "F5");
    }

    #[test]
    fn sides() {
        assert_eq!(key_side("MetaRight"), Some("R"));
        assert_eq!(key_side("Alt"), Some("L"));
        assert_eq!(key_side("KeyD"), None);
    }
}
