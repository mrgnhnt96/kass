//! The dictation shortcut's keys as the empty screen shows them.

/// A key as a key cap reads: "⌘", "Space", "A".
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
            return name
                .strip_prefix("Key")
                .or_else(|| name.strip_prefix("Num"))
                .filter(|rest| rest.chars().count() == 1)
                .unwrap_or(name)
                .to_string();
        }
    };
    label.to_string()
}

/// Which side a modifier is on, when the chord names one.
pub fn side_hint(name: &str) -> Option<&'static str> {
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

/// Modifiers first (⌃ ⌥ ⇧ ⌘), the main key last, as macOS lists them.
pub fn sort_chord(keys: &[String]) -> Vec<String> {
    let mut keys = keys.to_vec();
    keys.sort_by(|a, b| order(a).cmp(&order(b)).then_with(|| a.cmp(b)));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_as_key_caps() {
        assert_eq!(key_label("MetaRight"), "⌘");
        assert_eq!(key_label("KeyA"), "A");
        assert_eq!(key_label("Num7"), "7");
        assert_eq!(key_label("Space"), "Space");
        assert_eq!(key_label("F5"), "F5");
        assert_eq!(side_hint("MetaRight"), Some("R"));
        assert_eq!(side_hint("Space"), None);
    }

    #[test]
    fn modifiers_come_first() {
        let keys = ["Space", "MetaLeft", "Alt"].map(String::from);
        assert_eq!(sort_chord(&keys), ["Alt", "MetaLeft", "Space"]);
    }
}
