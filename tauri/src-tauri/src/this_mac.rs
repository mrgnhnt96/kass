//! Which Mac this is, for a Kass shared over the network (beta `network`).
//!
//! Every request that can save a capture says which Mac sent it
//! (backend/utils/macs.py), so a shared Kass can show where each capture
//! came from. The id is random, made once and kept in the app's data
//! folder; the name is the one in System Settings › General › Sharing.

use std::path::Path;
use std::sync::OnceLock;

use serde::Serialize;
use tauri::Manager;

#[derive(Clone, Debug, Serialize)]
pub struct ThisMac {
    pub id: String,
    pub name: Option<String>,
}

static THIS_MAC: OnceLock<ThisMac> = OnceLock::new();

/// Read (or make) this Mac's id. Call once from setup.
pub fn init(app: &tauri::AppHandle) {
    let id = app
        .path()
        .app_data_dir()
        .ok()
        .and_then(|dir| load_or_make_id(&dir));
    if let Some(id) = id {
        let _ = THIS_MAC.set(ThisMac {
            id,
            name: computer_name(),
        });
    }
}

/// This Mac, unless its id couldn't be read or saved.
pub fn get() -> Option<&'static ThisMac> {
    THIS_MAC.get()
}

/// The headers that say which Mac sent a request (`X-Kass-Mac`, `X-Kass-Mac-Name`).
pub fn headers() -> Vec<(&'static str, String)> {
    let Some(mac) = get() else {
        return Vec::new();
    };
    let mut headers = vec![("X-Kass-Mac", mac.id.clone())];
    if let Some(name) = &mac.name {
        headers.push(("X-Kass-Mac-Name", percent_encode(name)));
    }
    headers
}

#[tauri::command]
pub fn this_mac() -> Option<ThisMac> {
    get().cloned()
}

fn load_or_make_id(dir: &Path) -> Option<String> {
    let path = dir.join("mac-id");
    if let Ok(saved) = std::fs::read_to_string(&path) {
        let saved = saved.trim();
        if is_id(saved) {
            return Some(saved.to_string());
        }
    }
    let mut bytes = [0u8; 16];
    getrandom(&mut bytes)?;
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir).ok()?;
    std::fs::write(&path, &id).ok()?;
    Some(id)
}

fn getrandom(bytes: &mut [u8]) -> Option<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(bytes)
        .ok()
}

fn is_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn computer_name() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// Header values are ASCII, and Mac names often aren't ("Morgan’s MacBook").
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_names_that_arent_ascii() {
        assert_eq!(
            percent_encode("Morgan’s MacBook"),
            "Morgan%E2%80%99s%20MacBook"
        );
    }

    #[test]
    fn makes_an_id_once_and_keeps_it() {
        let dir = std::env::temp_dir().join(format!("kass-this-mac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_make_id(&dir).unwrap();
        assert_eq!(first.len(), 32);
        assert!(is_id(&first));
        assert_eq!(load_or_make_id(&dir).unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
