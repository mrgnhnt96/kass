//! Carries an install over from an earlier bundle identifier.
//!
//! Kass was Herga (`com.mrgnhnt.herga`), and before that Voicebox, first
//! `sh.voicebox.app` and then, briefly, `com.mrgnhnt.voicebox`; it is now
//! `com.mrgnhnt.kass`. macOS and Tauri
//! name the app's folders after the identifier, so the first launch under
//! the new one moves them: the data folder (database, captures, settings)
//! and WebKit's folder (the webview's local storage). It runs before any
//! window opens, while nothing has the folders open. Nothing moves when a
//! data folder already exists under the new identifier.
//!
//! Upstream Voicebox (the text-to-speech app) still uses `sh.voicebox.app`,
//! so that folder only moves when it holds a file only this app writes.

use std::path::Path;

/// Earlier identifiers, newest first.
const OLD_IDENTIFIERS: [&str; 3] = [
    "com.mrgnhnt.herga",
    "com.mrgnhnt.voicebox",
    "sh.voicebox.app",
];

/// Files only this app writes to its data folder, never upstream Voicebox.
const OWN_FILES: [&str; 4] = [
    "launch-at-login-defaulted",
    "correction-learning.json",
    "writing-style.json",
    "dictation-device.txt",
];

/// Marker `login_item` writes after turning launch at login on by default.
/// The login item belongs to the old identifier, so dropping the marker
/// lets the new one register again.
const LOGIN_DEFAULTED_MARKER: &str = "launch-at-login-defaulted";

pub fn move_from_old_identifier(new_identifier: &str) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let library = Path::new(&home).join("Library");
    let support = library.join("Application Support");
    let Some(old_identifier) = folder_to_move(&support, new_identifier) else {
        return;
    };
    if move_folder(&support, old_identifier, new_identifier) {
        let _ = std::fs::remove_file(support.join(new_identifier).join(LOGIN_DEFAULTED_MARKER));
        move_folder(&library.join("WebKit"), old_identifier, new_identifier);
    }
}

/// The earlier identifier whose data folder in `support` is this app's, if
/// there is one and the new identifier has no data folder yet.
fn folder_to_move(support: &Path, new_identifier: &str) -> Option<&'static str> {
    if support.join(new_identifier).exists() {
        return None;
    }
    OLD_IDENTIFIERS.into_iter().find(|old| {
        let folder = support.join(old);
        folder.is_dir()
            && (*old != "sh.voicebox.app" || OWN_FILES.iter().any(|f| folder.join(f).exists()))
    })
}

/// Renames `parent/old` to `parent/new`; true when it did.
fn move_folder(parent: &Path, old: &str, new: &str) -> bool {
    let (old, new) = (parent.join(old), parent.join(new));
    if !old.is_dir() || new.exists() {
        return false;
    }
    match std::fs::rename(&old, &new) {
        Ok(()) => {
            println!("Moved {} to {}", old.display(), new.display());
            true
        }
        Err(e) => {
            eprintln!(
                "Couldn't move {} to {}: {}",
                old.display(),
                new.display(),
                e
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-move-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_the_old_folder_once_and_never_over_a_new_one() {
        let parent = scratch("once");
        std::fs::create_dir_all(parent.join("sh.voicebox.app")).unwrap();
        std::fs::write(parent.join("sh.voicebox.app/kass.db"), "data").unwrap();

        assert!(move_folder(&parent, "sh.voicebox.app", "com.example.new"));
        assert_eq!(
            std::fs::read_to_string(parent.join("com.example.new/kass.db")).unwrap(),
            "data"
        );
        assert!(!parent.join("sh.voicebox.app").exists());

        std::fs::create_dir_all(parent.join("sh.voicebox.app")).unwrap();
        assert!(!move_folder(&parent, "sh.voicebox.app", "com.example.new"));
        assert!(parent.join("sh.voicebox.app").exists());

        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn leaves_upstream_voicebox_data_alone() {
        let support = scratch("upstream");
        std::fs::create_dir_all(support.join("sh.voicebox.app/profiles")).unwrap();
        assert_eq!(folder_to_move(&support, "com.mrgnhnt.kass"), None);

        std::fs::write(support.join("sh.voicebox.app/writing-style.json"), "{}").unwrap();
        assert_eq!(
            folder_to_move(&support, "com.mrgnhnt.kass"),
            Some("sh.voicebox.app")
        );
        std::fs::remove_dir_all(&support).unwrap();
    }

    #[test]
    fn prefers_the_newer_identifier_and_skips_when_already_moved() {
        let support = scratch("newer");
        std::fs::create_dir_all(support.join("com.mrgnhnt.voicebox")).unwrap();
        std::fs::create_dir_all(support.join("sh.voicebox.app")).unwrap();
        std::fs::write(support.join("sh.voicebox.app/writing-style.json"), "{}").unwrap();
        assert_eq!(
            folder_to_move(&support, "com.mrgnhnt.kass"),
            Some("com.mrgnhnt.voicebox")
        );

        std::fs::create_dir_all(support.join("com.mrgnhnt.herga")).unwrap();
        assert_eq!(
            folder_to_move(&support, "com.mrgnhnt.kass"),
            Some("com.mrgnhnt.herga")
        );

        std::fs::create_dir_all(support.join("com.mrgnhnt.kass")).unwrap();
        assert_eq!(folder_to_move(&support, "com.mrgnhnt.kass"), None);
        std::fs::remove_dir_all(&support).unwrap();
    }
}
