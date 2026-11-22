//! Where the app runs from, and moving it into `/Applications`.
//!
//! macOS ties privacy grants (Microphone, Accessibility, Input Monitoring)
//! to the app it recorded them for. Run from somewhere else — the build
//! folder, Downloads, a disk image — Kass can be missing from the
//! Privacy panes, or its switches are on but don't work. So the app asks to
//! be moved when it isn't in `/Applications`, and can move itself.
//!
//! A dev build runs a bare binary from `target/`, not a bundle, and is left
//! alone.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use objc::runtime::{Object, BOOL, YES};
use objc::{class, msg_send, sel, sel_impl};
use serde::Serialize;

use crate::focus_capture::{ns_string_to_rust, AutoreleasePool};

type Id = *mut Object;

pub const APPLICATIONS: &str = "/Applications";
pub const INSTALLED_APP: &str = "/Applications/Kass.app";

/// Kass was Herga, installed as `/Applications/Herga.app`.
const OLD_INSTALLED_APP: &str = "/Applications/Herga.app";

/// The `.app` bundle whose `Contents/MacOS/` holds `exe`, if any.
fn bundle_of(exe: &Path) -> Option<&Path> {
    let in_contents_macos = exe.parent().and_then(Path::file_name) == Some("MacOS".as_ref())
        && exe.ancestors().nth(2).and_then(Path::file_name) == Some("Contents".as_ref());
    let bundle = exe.ancestors().nth(3)?;
    (in_contents_macos && bundle.extension() == Some("app".as_ref())).then_some(bundle)
}

/// Whether `exe` is the binary of a bundle directly in `/Applications`.
pub fn is_installed_executable(exe: &Path) -> bool {
    bundle_of(exe).and_then(Path::parent) == Some(Path::new(APPLICATIONS))
}

/// The bundle this process runs from, or None for a dev build.
fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    bundle_of(&exe).map(Path::to_path_buf)
}

pub fn is_installed() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_installed_executable(&exe))
}

#[derive(Debug, Clone, Serialize)]
pub struct AppLocation {
    /// A bundle outside `/Applications`. False for a dev build.
    pub needs_move: bool,
    /// Where the running bundle is.
    pub path: Option<String>,
    /// Moving would put an existing `/Applications/Kass.app` in the Trash.
    pub replaces_existing: bool,
}

pub fn location() -> AppLocation {
    let bundle = running_bundle();
    let needs_move = bundle.is_some() && !is_installed();
    AppLocation {
        needs_move,
        path: bundle.map(|b| b.display().to_string()),
        replaces_existing: needs_move && Path::new(INSTALLED_APP).exists(),
    }
}

/// Copy the running bundle to `/Applications/Kass.app`, putting any app
/// already there in the Trash, then put the old copy in the Trash too.
/// Returns the new bundle's path; the caller relaunches from it.
pub fn copy_into_applications() -> Result<PathBuf, String> {
    let source = running_bundle().ok_or("Kass isn't running from an app bundle")?;
    let dest = PathBuf::from(INSTALLED_APP);
    if source == dest {
        return Err("Kass is already in Applications".into());
    }
    if dest.exists() {
        trash(&dest)
            .map_err(|e| format!("Couldn't move the Kass in Applications to the Trash: {e}"))?;
    }
    // ditto keeps the code signature and extended attributes intact.
    let status = Command::new("/usr/bin/ditto")
        .arg(&source)
        .arg(&dest)
        .status()
        .map_err(|e| format!("Couldn't copy Kass into Applications: {e}"))?;
    if !status.success() {
        return Err(format!(
            "Couldn't copy Kass into Applications (ditto {status})"
        ));
    }
    // A copy on a disk image or in a read-only location stays where it is.
    if let Err(e) = trash(&source) {
        eprintln!(
            "[app-location] left the old copy at {}: {e}",
            source.display()
        );
    }
    Ok(dest)
}

/// What to do with the Herga bundle in `/Applications`, if any.
#[derive(Debug, PartialEq)]
enum OldBundle {
    /// An update installed Kass in place, so this app runs from Herga.app:
    /// give it Kass's name.
    Rename,
    /// Herga itself is still installed beside Kass. Left there, it would
    /// open at login and answer the same hotkey.
    Retire,
    Leave,
}

fn old_bundle(old: &Path, running: Option<&Path>, installed_exists: bool) -> OldBundle {
    if !old.is_dir() {
        OldBundle::Leave
    } else if running == Some(old) {
        if installed_exists {
            OldBundle::Leave
        } else {
            OldBundle::Rename
        }
    } else if old.join("Contents/MacOS/herga").exists() {
        OldBundle::Retire
    } else {
        OldBundle::Leave
    }
}

/// Carries an install over from Herga. Runs first thing at launch; when it
/// renames the running bundle it relaunches from the new path and exits.
/// A dev build or a copy outside `/Applications` leaves Herga alone.
pub fn carry_over_old_bundle() {
    if !is_installed() {
        return;
    }
    let old = Path::new(OLD_INSTALLED_APP);
    let running = running_bundle();
    match old_bundle(old, running.as_deref(), Path::new(INSTALLED_APP).exists()) {
        OldBundle::Rename => match std::fs::rename(old, INSTALLED_APP) {
            Ok(()) => {
                println!("[app-location] renamed {OLD_INSTALLED_APP} to {INSTALLED_APP}");
                if relaunch_after_exit(std::process::id(), Path::new(INSTALLED_APP)).is_ok() {
                    std::process::exit(0);
                }
            }
            Err(e) => eprintln!("[app-location] couldn't rename {OLD_INSTALLED_APP}: {e}"),
        },
        OldBundle::Retire => {
            // Quit Herga and its server before its data folder moves.
            let _ = Command::new("/usr/bin/pkill")
                .arg("-f")
                .arg(format!("{OLD_INSTALLED_APP}/Contents/"))
                .status();
            match trash(old) {
                Ok(()) => println!("[app-location] moved {OLD_INSTALLED_APP} to the Trash"),
                Err(e) => eprintln!("[app-location] left {OLD_INSTALLED_APP}: {e}"),
            }
        }
        OldBundle::Leave => {}
    }
}

/// Open `bundle` once this process (`pid`) has exited. Opening it sooner
/// would just bring the running copy forward, since both share a bundle id.
pub fn relaunch_after_exit(pid: u32, bundle: &Path) -> Result<(), String> {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$2""#)
        .arg("sh")
        .arg(pid.to_string())
        .arg(bundle)
        .spawn()
        .map_err(|e| format!("Couldn't schedule the relaunch: {e}"))?;
    Ok(())
}

/// Move `path` to the Trash, so nothing is deleted outright.
fn trash(path: &Path) -> Result<(), String> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    unsafe {
        let _pool = AutoreleasePool::new();
        let ns_path: Id = msg_send![class!(NSString), stringWithUTF8String: c_path.as_ptr()];
        let url: Id = msg_send![class!(NSURL), fileURLWithPath: ns_path];
        let manager: Id = msg_send![class!(NSFileManager), defaultManager];
        let mut error: Id = std::ptr::null_mut();
        let ok: BOOL = msg_send![manager, trashItemAtURL: url
                                          resultingItemURL: std::ptr::null_mut::<Id>()
                                          error: &mut error];
        if ok == YES {
            return Ok(());
        }
        if error.is_null() {
            return Err("unknown error".into());
        }
        let description: Id = msg_send![error, localizedDescription];
        Err(ns_string_to_rust(description).unwrap_or_else(|| "unknown error".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bundle_in_applications_counts() {
        let installed = |p: &str| is_installed_executable(Path::new(p));
        assert!(installed("/Applications/Kass.app/Contents/MacOS/kass"));
        assert!(!installed(
            "/Users/me/Applications/Kass.app/Contents/MacOS/kass"
        ));
        assert!(!installed(
            "/Users/me/Downloads/Kass.app/Contents/MacOS/kass"
        ));
        assert!(!installed(
            "/Users/me/kass/app/target/release/bundle/macos/Kass.app/Contents/MacOS/kass"
        ));
        assert!(!installed("/Users/me/kass/app/target/debug/kass"));
        assert!(!installed("/Applications/Kass.app/Contents/Resources/kass"));
        assert!(!installed("/Applications/Kass/Contents/MacOS/kass"));
        assert!(!installed("/kass"));
    }

    #[test]
    fn a_dev_binary_has_no_bundle() {
        assert_eq!(
            bundle_of(Path::new("/Users/me/kass/app/target/debug/kass")),
            None
        );
        assert_eq!(
            bundle_of(Path::new(
                "/Users/me/Downloads/Kass.app/Contents/MacOS/kass"
            )),
            Some(Path::new("/Users/me/Downloads/Kass.app"))
        );
    }

    #[test]
    fn an_old_herga_bundle_is_renamed_when_kass_runs_from_it_and_retired_otherwise() {
        let dir = std::env::temp_dir().join(format!("kass-old-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let old = dir.join("Herga.app");

        assert_eq!(old_bundle(&old, None, false), OldBundle::Leave);

        std::fs::create_dir_all(old.join("Contents/MacOS")).unwrap();
        std::fs::write(old.join("Contents/MacOS/kass"), "").unwrap();
        assert_eq!(old_bundle(&old, Some(&old), false), OldBundle::Rename);
        assert_eq!(old_bundle(&old, Some(&old), true), OldBundle::Leave);
        // Not Herga, and not this app: someone else's app of the same name.
        assert_eq!(old_bundle(&old, None, true), OldBundle::Leave);

        std::fs::write(old.join("Contents/MacOS/herga"), "").unwrap();
        assert_eq!(
            old_bundle(&old, Some(&dir.join("Kass.app")), true),
            OldBundle::Retire
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
