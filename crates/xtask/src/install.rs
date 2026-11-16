//! cargo xtask install: build Kass from this checkout and install it to /Applications.

use std::env;
use std::fs;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::util::{
    BUILT_APP, Result, SERVER_DIR, all_output, cmd, home, is_executable, no_more, on_path, output,
    path_with, run, style, succeeds, take_flag,
};
use crate::{build, python, sign};

pub const HELP: &str = "\
Build Kass from this checkout and install it to /Applications.

  cargo xtask install [--no-pull] [--rebuild-server] [--no-launch]

For working on Kass. Everyone else installs the DMG from GitHub Releases.
Checks what the build needs and says how to install anything missing, pulls
the latest code, builds the server (only when it changed) and the app, and
replaces /Applications/Kass.app. Every build is signed with the same
identity, so updates keep the app's privacy permissions.

Options:
  --no-pull          build the checkout as it is
  --rebuild-server   rebuild the Python server even if it hasn't changed
  --no-launch        don't open Kass afterwards";

/// Always /Applications: macOS privacy permissions (Input Monitoring,
/// Accessibility, Microphone) don't work reliably for an app anywhere else.
const APP: &str = "/Applications/Kass.app";
const BUNDLE_ID: &str = "com.mrgnhnt.kass";
const SERVICES: [&str; 4] = ["Microphone", "Accessibility", "ListenEvent", "PostEvent"];
const PYTHON_STAMP: &str = "backend/venv/.kass-install-stamp";
const SERVER_STAMP: &str = "tauri/src-tauri/binaries/.kass-server-stamp";

fn step(text: &str) {
    let s = style();
    println!();
    println!("{}==> {text}{}", s.bold, s.reset);
}

fn ok(text: &str) {
    let s = style();
    println!("  {}✓{} {text}", s.green, s.reset);
}

fn warn(text: &str) {
    let s = style();
    println!("  {}!{} {text}", s.yellow, s.reset);
}

/// A digest of the inputs, to skip steps whose inputs haven't changed.
fn stamp(parts: &[Vec<u8>]) -> String {
    let mut hasher = Sha256::new();
    parts.iter().for_each(|part| hasher.update(part));
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read(path: &str) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
}

fn git_bytes(args: &[&str]) -> Vec<u8> {
    all_output(&mut cmd("git", args)).into_bytes()
}

fn stamp_matches(file: &str, stamp: &str) -> bool {
    fs::read_to_string(file).is_ok_and(|saved| saved.trim() == stamp)
}

fn save_stamp(file: &str, stamp: &str) -> Result {
    fs::write(file, format!("{stamp}\n")).map_err(|error| format!("Couldn't write {file}: {error}"))
}

pub fn install(mut args: Vec<String>) -> Result {
    if take_flag(&mut args, "-h") || take_flag(&mut args, "--help") {
        println!("{HELP}");
        return Ok(());
    }
    let pull = !take_flag(&mut args, "--no-pull");
    let rebuild_server = take_flag(&mut args, "--rebuild-server");
    let launch = !take_flag(&mut args, "--no-launch");
    no_more(&args)?;
    let s = style();

    // Tools installed by rustup's installer and Homebrew live here, and a
    // fresh shell may not have them on PATH yet.
    let path = path_with(&[home().join(".cargo/bin"), "/opt/homebrew/bin".into()]);
    // SAFETY: set before this process starts any thread.
    unsafe { env::set_var("PATH", path) };

    step("Checking requirements");
    if all_output(&mut cmd("uname", ["-sm"])).trim() != "Darwin arm64" {
        return Err("Kass only runs on Apple Silicon Macs.".into());
    }
    let mut missing: Vec<(&str, String)> = Vec::new();
    let brew_first = if on_path("brew") {
        String::new()
    } else {
        "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"\n      ".into()
    };
    if succeeds(&mut cmd("xcode-select", ["-p"])) {
        ok("Xcode Command Line Tools");
    } else {
        missing.push((
            "Xcode Command Line Tools (compilers, git, codesign)",
            "xcode-select --install".into(),
        ));
    }
    if on_path("git") {
        ok("git");
    }
    if python::python_ready() {
        ok("Python 3.12");
    } else {
        missing.push((
            "Python 3.12 (the server's ML packages don't support 3.13 yet)",
            format!("{brew_first}brew install python@3.12"),
        ));
    }
    if on_path("cargo") && on_path("rustc") {
        let version = output(&mut cmd("rustc", ["--version"])).unwrap_or_default();
        ok(&format!("Rust {}", version.split(' ').nth(1).unwrap_or("")));
    } else {
        missing.push((
            "Rust (builds the app)",
            "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh".into(),
        ));
    }
    let free_gb: u64 = all_output(&mut cmd("df", ["-g", "."]))
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|free| free.parse().ok())
        .unwrap_or(0);
    if free_gb < 15 {
        warn(&format!(
            "Only {free_gb} GB free. A first build needs about 15 GB (Python packages, Rust build)."
        ));
    }
    if !missing.is_empty() {
        println!();
        println!(
            "{}{}Some things Kass needs to build are missing.{} Install them, then run this again:",
            s.red, s.bold, s.reset
        );
        for (what, how) in &missing {
            println!();
            println!("  {}{what}{}", s.bold, s.reset);
            println!("      {how}");
        }
        println!();
        println!(
            "{}Open a new terminal after installing, so the new commands are on your PATH.{}",
            s.dim, s.reset
        );
        return Err("Missing requirements.".into());
    }
    if !succeeds(&mut cmd("test", ["-w", "/Applications"])) {
        let user = output(&mut cmd("whoami", [] as [&str; 0])).unwrap_or_default();
        return Err(format!(
            "/Applications isn't writable by {user}. Run this from an administrator account."
        ));
    }

    if pull
        && Path::new(".git").exists()
        && succeeds(&mut cmd("git", ["rev-parse", "--abbrev-ref", "@{u}"]))
    {
        step("Updating the code");
        if !output(&mut cmd(
            "git",
            ["status", "--porcelain", "--untracked-files=no"],
        ))?
        .is_empty()
        {
            warn("This checkout has local changes, so it's built as it is.");
        } else if succeeds(&mut cmd("git", ["pull", "--ff-only", "--quiet"])) {
            let branch = output(&mut cmd("git", ["rev-parse", "--abbrev-ref", "HEAD"]))?;
            let head = output(&mut cmd("git", ["log", "-1", "--format=%h %s"]))?;
            ok(&format!("{branch} at {head}"));
        } else {
            warn("Couldn't fast-forward to the latest code; building what's here.");
        }
    }

    step("Setting up the Python environment");
    let python_stamp = stamp(&[
        read("backend/requirements.txt"),
        read("crates/xtask/src/python.rs"),
    ]);
    if stamp_matches(PYTHON_STAMP, &python_stamp) {
        ok("Up to date");
    } else {
        python::setup_python(vec![])?;
        save_stamp(PYTHON_STAMP, &python_stamp)?;
        ok("Python packages");
    }

    step("Building the server");
    let server_stamp = stamp(&[
        git_bytes(&["rev-parse", "HEAD:backend"]),
        git_bytes(&["diff", "HEAD", "--", "backend"]),
        git_bytes(&["ls-files", "--others", "--exclude-standard", "backend"]),
        read("crates/xtask/src/build.rs"),
        read(PYTHON_STAMP),
    ]);
    if !rebuild_server
        && is_executable(&Path::new(SERVER_DIR).join("kass-server"))
        && stamp_matches(SERVER_STAMP, &server_stamp)
    {
        ok("Unchanged since the last build");
    } else {
        println!("  {}This takes a few minutes.{}", s.dim, s.reset);
        build::server(vec![])?;
        save_stamp(SERVER_STAMP, &server_stamp)?;
        ok("Server built");
    }

    step("Building the app");
    println!(
        "  {}The first build compiles the Rust app and takes a while.{}",
        s.dim, s.reset
    );
    build::app(vec![])?;
    ok(&format!(
        "Built and signed with \"{}\"",
        sign::authority(Path::new(BUILT_APP)).unwrap_or_default()
    ));

    step(&format!("Installing to {APP}"));
    let had_app = Path::new(APP).is_dir();
    let old_requirement = if had_app {
        requirement(APP)
    } else {
        String::new()
    };
    let new_requirement = requirement(BUILT_APP);

    let running = format!("{APP}/Contents/MacOS/");
    if succeeds(&mut cmd("pgrep", ["-f", &running])) {
        quit(BUNDLE_ID);
        for _ in 0..30 {
            if !succeeds(&mut cmd("pgrep", ["-f", &running])) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = succeeds(&mut cmd("pkill", ["-f", &format!("{APP}/Contents/")]));
        ok("Quit the running app");
    }
    if had_app {
        fs::remove_dir_all(APP).map_err(|error| format!("Couldn't remove {APP}: {error}"))?;
    }
    run(&mut cmd("ditto", [BUILT_APP, APP]))?;
    run(&mut cmd(
        "codesign",
        ["--verify", "--deep", "--strict", APP],
    ))?;
    ok("Installed");

    // Builds from a checkout are internal builds: internal features run
    // (backend/internal.py). The release DMG never writes this file.
    let data_dir = home().join("Library/Application Support").join(BUNDLE_ID);
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("Couldn't create {}: {error}", data_dir.display()))?;
    let internal = data_dir.join("internal");
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&internal)
        .map_err(|error| format!("Couldn't write {}: {error}", internal.display()))?;
    ok(&format!(
        "Internal features on (delete {} to turn them off)",
        internal.display()
    ));

    retire_herga();

    // The old grants belong to the old signature and no longer apply. Clear
    // them so System Settings asks again cleanly instead of showing switches
    // that are on but don't work. An unreadable old requirement counts as a
    // change, so it resets instead of keeping grants that may not apply.
    if had_app && old_requirement != new_requirement {
        reset_grants(BUNDLE_ID);
        warn("This build is signed differently from the one it replaced, so macOS");
        warn("will ask for Microphone, Accessibility and Input Monitoring again.");
        warn("It's signed the same way from now on, so updates keep them.");
    }

    if launch {
        run(&mut cmd("open", [APP]))?;
        ok("Opened Kass");
    }
    println!();
    println!(
        "{}{}Kass is installed.{} Run cargo xtask install again to update.",
        s.green, s.bold, s.reset
    );
    Ok(())
}

/// The app's designated requirement. An ad-hoc signature prints it as a
/// comment ("# designated => …").
fn requirement(app: &str) -> String {
    all_output(&mut cmd("codesign", ["-d", "-r-", app]))
        .lines()
        .find_map(|line| {
            line.strip_prefix("# ")
                .unwrap_or(line)
                .strip_prefix("designated => ")
        })
        .unwrap_or("")
        .to_string()
}

fn quit(bundle_id: &str) {
    let _ = succeeds(&mut cmd(
        "osascript",
        ["-e", &format!("quit app id \"{bundle_id}\"")],
    ));
}

fn reset_grants(bundle_id: &str) {
    for service in SERVICES {
        let _ = succeeds(&mut cmd("tccutil", ["reset", service, bundle_id]));
    }
}

/// Kass was Herga. Retire the old app, so it doesn't start at login and
/// answer the same hotkey.
fn retire_herga() {
    let app = "/Applications/Herga.app";
    let id = "com.mrgnhnt.herga";
    if !Path::new(app).is_dir() {
        return;
    }
    let found = all_output(&mut cmd(
        "plutil",
        [
            "-extract",
            "CFBundleIdentifier",
            "raw",
            &format!("{app}/Contents/Info.plist"),
        ],
    ));
    if found.trim() != id {
        return;
    }
    quit(id);
    let _ = succeeds(&mut cmd("pkill", ["-f", &format!("{app}/Contents/")]));
    reset_grants(id);
    let when = output(&mut cmd("date", ["+%Y-%m-%d %H.%M.%S"])).unwrap_or_default();
    let trash = home().join(".Trash").join(format!("Herga {when}.app"));
    if fs::rename(app, trash).is_ok() {
        ok("Moved Herga.app, Kass's old name, to the Trash");
    } else {
        let _ = fs::remove_dir_all(app);
        ok("Removed Herga.app, Kass's old name");
    }
    warn("Kass keeps your captures and settings, but macOS will ask for");
    warn("Microphone, Accessibility and Input Monitoring again under the new name.");
}
