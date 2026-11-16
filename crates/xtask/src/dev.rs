//! Running Kass for development, and cleaning up after it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;

use crate::util::{
    APP_DIR, BACKEND, Result, VENV, WORKER_PORT, cmd, ensure_venv, home, no_more, path_with, run,
    succeeds, venv_bin,
};

fn worker() -> std::process::Command {
    cmd(
        venv_bin("uvicorn"),
        [
            "backend.main:app",
            "--reload",
            "--port",
            &WORKER_PORT.to_string(),
        ],
    )
}

fn worker_running() -> bool {
    succeeds(&mut cmd(
        "curl",
        ["-sf", &format!("http://127.0.0.1:{WORKER_PORT}/health")],
    ))
}

/// Stops the worker this command started when it ends, however it ends.
struct Worker(Option<Child>);

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// cargo xtask dev: the Python model worker (unless it's already running) and
/// a debug build of the app. The app serves 17493 itself and passes model
/// requests on to the worker on 17494.
pub fn dev(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    let _worker = if worker_running() {
        println!("Model worker already running on http://localhost:{WORKER_PORT}");
        Worker(None)
    } else {
        println!("Starting model worker on http://localhost:{WORKER_PORT} ...");
        let child = worker()
            .spawn()
            .map_err(|error| format!("Couldn't start the model worker: {error}"))?;
        std::thread::sleep(Duration::from_secs(2));
        Worker(Some(child))
    };
    println!("Starting the desktop app...");
    dev_app(vec![])
}

/// cargo xtask dev-backend: only the Python model worker, on the port the app
/// passes requests to.
pub fn dev_backend(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    run(&mut worker())
}

/// cargo xtask dev-app: only a debug build of the app (start the worker
/// separately with cargo xtask dev-backend).
pub fn dev_app(args: Vec<String>) -> Result {
    no_more(&args)?;
    run(cmd("cargo", ["run"])
        .current_dir(APP_DIR)
        .env("PATH", path_with(&[home().join(".cargo/bin")])))
}

/// cargo xtask kill: stops the dev model worker.
pub fn kill(args: Vec<String>) -> Result {
    no_more(&args)?;
    let _ = succeeds(&mut cmd("pkill", ["-f", "uvicorn backend.main:app"]));
    println!("Dev processes killed.");
    Ok(())
}

/// cargo xtask docs: opens the worker's API docs.
pub fn docs(args: Vec<String>) -> Result {
    no_more(&args)?;
    run(&mut cmd(
        "open",
        [format!("http://localhost:{WORKER_PORT}/docs")],
    ))
}

/// cargo xtask logs: follows the backend's log files.
pub fn logs(args: Vec<String>) -> Result {
    no_more(&args)?;
    let dir = Path::new(BACKEND).join("logs");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    if files.is_empty() {
        println!("No log files found");
        return Ok(());
    }
    files.sort();
    run(cmd("tail", ["-f"]).args(files))
}

fn remove_dir(dir: &Path) -> Result {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Couldn't remove {}: {error}", dir.display())),
    }
}

/// cargo xtask clean: the release build.
pub fn clean(args: Vec<String>) -> Result {
    no_more(&args)?;
    remove_dir(&Path::new(APP_DIR).join("target/release"))
}

/// cargo xtask clean-python: the venv and Python's caches.
pub fn clean_python(args: Vec<String>) -> Result {
    no_more(&args)?;
    remove_dir(Path::new(VENV))?;
    let mut pending = vec![PathBuf::from(BACKEND)];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if entry.file_name() == "__pycache__" {
                remove_dir(&entry.path())?;
            } else {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

/// cargo xtask clean-all: clean, clean-python and Cargo's build cache too.
pub fn clean_all(args: Vec<String>) -> Result {
    no_more(&args)?;
    clean(vec![])?;
    clean_python(vec![])?;
    run(cmd("cargo", ["clean"]).current_dir(APP_DIR))
}
