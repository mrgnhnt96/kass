//! The Reports tab's button: run the shipped `kass-report.sh`, which zips
//! the logs, crash reports and system info, then show the zip in Finder.
//!
//! The script, not this code, decides what goes in the zip, so the button,
//! a user in Terminal and an AI agent all produce the same report. It runs
//! without the server, so a report still works after the server crashed.

use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::{command, AppHandle, Manager};

const SCRIPT: &str = "kass-report.sh";

/// The bundled script, or the repo's in a dev build (which bundles nothing).
fn script_path(resource_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(script) = resource_dir.map(|dir| dir.join(SCRIPT)) {
        if script.is_file() {
            return Ok(script);
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts")
        .join(SCRIPT);
    if cfg!(debug_assertions) && repo.is_file() {
        return Ok(repo);
    }
    Err(format!("{SCRIPT} is missing from the app"))
}

/// The script prints the zip's path as its last line.
fn zip_path(stdout: &str) -> Option<PathBuf> {
    let line = stdout.lines().rev().find(|line| !line.trim().is_empty())?;
    let path = PathBuf::from(line.trim());
    (path.extension()? == "zip").then_some(path)
}

fn run(script: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    let output = Command::new("/bin/bash")
        .arg(script)
        .env("KASS_DATA_DIR", data_dir)
        .output()
        .map_err(|e| format!("Couldn't run {SCRIPT}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{SCRIPT} failed: {}", stderr.trim()));
    }
    zip_path(&String::from_utf8_lossy(&output.stdout))
        .filter(|path| path.is_file())
        .ok_or_else(|| format!("{SCRIPT} finished without writing a zip"))
}

/// Show `path` selected in a Finder window.
#[command]
pub fn reveal_report(path: String) -> Result<(), String> {
    let status = Command::new("/usr/bin/open")
        .args(["-R", &path])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Couldn't show {path} in Finder"))
    }
}

/// Make a report zip in Downloads, show it in Finder, and return its path.
#[command]
pub async fn create_report(app: AppHandle) -> Result<String, String> {
    let script = script_path(app.path().resource_dir().ok())?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let zip = tauri::async_runtime::spawn_blocking(move || run(&script, &data_dir))
        .await
        .map_err(|e| e.to_string())??;
    let path = zip.to_string_lossy().into_owned();
    reveal_report(path.clone())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_zip_from_the_last_line() {
        let stdout = "note\n/Users/me/Downloads/Kass-report-2026-10-08-090000.zip\n\n";
        assert_eq!(
            zip_path(stdout),
            Some(PathBuf::from(
                "/Users/me/Downloads/Kass-report-2026-10-08-090000.zip"
            ))
        );
    }

    #[test]
    fn rejects_output_that_is_not_a_zip() {
        assert_eq!(zip_path(""), None);
        assert_eq!(zip_path("something went wrong\n"), None);
    }

    #[test]
    fn the_repo_script_makes_a_zip() {
        let script = script_path(None).expect("the repo script");
        let dir = std::env::temp_dir().join(format!("kass-report-test-{}", std::process::id()));
        let data = dir.join("data");
        std::fs::create_dir_all(data.join("logs")).unwrap();
        std::fs::write(data.join("logs/server.log"), "INFO ready\n").unwrap();
        let output = Command::new("/bin/bash")
            .arg(&script)
            .args(["--output", dir.join("out").to_str().unwrap()])
            .env("KASS_DATA_DIR", &data)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let zip = zip_path(&String::from_utf8_lossy(&output.stdout)).unwrap();
        assert!(zip.is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
