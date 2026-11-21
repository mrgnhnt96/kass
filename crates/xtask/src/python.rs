//! Setup, checks, tests and the database: the Python backend and the app's Rust.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::{
    APP_DIR, BACKEND, RUST_WORKSPACES, Result, VENV, all_output, cmd, ensure_venv, no_more,
    on_path, output, run, take_flag, venv_bin, which,
};

/// cargo xtask setup: the Python venv and the pre-push hook.
pub fn setup(args: Vec<String>) -> Result {
    no_more(&args)?;
    setup_python(vec![])?;
    setup_hooks(vec![])?;
    println!();
    println!("Setup complete! Run: cargo xtask dev");
    Ok(())
}

/// cargo xtask setup-hooks: the pre-push hook (scripts/hooks/pre-push) that
/// lints and formats pushed files.
pub fn setup_hooks(args: Vec<String>) -> Result {
    no_more(&args)?;
    run(&mut cmd(
        "git",
        ["config", "core.hooksPath", "scripts/hooks"],
    ))
}

fn is_312(python: &Path) -> bool {
    python.is_file()
        && output(&mut cmd(
            python,
            ["-c", "import sys; print(sys.version_info[:2] == (3, 12))"],
        ))
        .is_ok_and(|said| said == "True")
}

/// A Python 3.12 to build the venv with: on PATH, Homebrew's, or uv's.
fn find_python() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = which("python3.12").into_iter().collect();
    candidates.push(PathBuf::from(
        "/opt/homebrew/opt/python@3.12/bin/python3.12",
    ));
    if on_path("uv")
        && let Ok(found) = output(&mut cmd("uv", ["python", "find", "3.12"]))
    {
        candidates.push(PathBuf::from(found));
    }
    candidates.into_iter().find(|candidate| is_312(candidate))
}

/// Whether the venv can be set up, without changing anything.
pub fn python_ready() -> bool {
    is_312(&venv_bin("python")) || find_python().is_some()
}

/// cargo xtask setup-python [--check]: backend/venv with Python 3.12 and the
/// server's dependencies. Python 3.12 is required: numba (pinned below 0.61
/// for librosa) has no wheels for 3.13. --check only says whether it can.
pub fn setup_python(mut args: Vec<String>) -> Result {
    let check = take_flag(&mut args, "--check");
    no_more(&args)?;
    if all_output(&mut cmd("uname", ["-sm"])).trim() != "Darwin arm64" {
        return Err("Kass requires an Apple Silicon Mac (arm64 macOS).".into());
    }
    if check {
        return if python_ready() {
            Ok(())
        } else {
            Err("Python 3.12 not found.".into())
        };
    }

    if !is_312(&venv_bin("python")) {
        let python = find_python()
            .ok_or("Python 3.12 not found. Install it with: brew install python@3.12")?;
        if Path::new(VENV).is_dir() {
            println!("Recreating {VENV} with Python 3.12...");
            fs::remove_dir_all(VENV).map_err(|error| format!("Couldn't remove {VENV}: {error}"))?;
        }
        println!("Creating Python virtual environment...");
        run(cmd(python, ["-m", "venv", VENV]))?;
    }

    // Through python, not bin/pip: a venv's scripts keep the path it was made
    // at, so they break when the checkout moves.
    let pip = |args: &[&str]| {
        let mut command = cmd(venv_bin("python"), ["-m", "pip", "install"]);
        command.args(args);
        command
    };
    println!("Installing Python dependencies...");
    run(&mut pip(&["--upgrade", "pip", "-q"]))?;
    run(&mut pip(&["-r", "backend/requirements.txt"]))?;
    // mlx-lm and mlx-audio declare transformers>=5.x, which conflicts with our
    // transformers<=4.57.x cap, so install them --no-deps (their runtime deps
    // are covered by requirements.txt; see the note there).
    run(&mut pip(&[
        "--no-deps",
        "mlx-lm==0.31.1",
        "mlx-audio==0.4.1",
    ]))?;
    // ruff is pinned: another version formats and lints differently, so
    // cargo xtask check would pass on one Mac and fail on the next.
    run(&mut pip(&[
        "pyinstaller",
        "ruff==0.16.9",
        "pytest",
        "pytest-asyncio",
        "-q",
    ]))?;
    println!("Python environment ready.");
    Ok(())
}

fn ruff(args: &[&str]) -> Command {
    let mut command = cmd(venv_bin("ruff"), args);
    command.arg(BACKEND);
    command
}

/// cargo xtask check: everything check-python and check-rust do.
pub fn check(args: Vec<String>) -> Result {
    no_more(&args)?;
    check_python(vec![])?;
    check_rust(vec![])
}

/// cargo xtask check-python: ruff lint and format check.
pub fn check_python(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    run(&mut ruff(&["check"]))?;
    run(&mut ruff(&["format", "--check"]))
}

/// cargo xtask check-rust: rustfmt check and clippy, warnings failing, in
/// both Rust workspaces.
pub fn check_rust(args: Vec<String>) -> Result {
    no_more(&args)?;
    for workspace in RUST_WORKSPACES {
        run(cmd("cargo", ["fmt", "--all", "--check"]).current_dir(workspace))?;
    }
    for workspace in RUST_WORKSPACES {
        run(cmd(
            "cargo",
            [
                "clippy",
                "--workspace",
                "--all-targets",
                "--quiet",
                "--",
                "-D",
                "warnings",
            ],
        )
        .current_dir(workspace))?;
    }
    Ok(())
}

/// cargo xtask lint: ruff check on the backend.
pub fn lint(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    run(&mut ruff(&["check"]))
}

/// cargo xtask format: ruff format on the backend.
pub fn format(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    run(&mut ruff(&["format"]))
}

/// cargo xtask fix: ruff's lint fixes, then its formatting.
pub fn fix(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    run(ruff(&["check"]).arg("--fix"))?;
    run(&mut ruff(&["format"]))
}

/// cargo xtask test: the backend's pytest suite, then the Rust tests.
pub fn test(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    run(&mut cmd(
        venv_bin("python"),
        ["-m", "pytest", "backend/tests", "-v"],
    ))?;
    test_rust(vec![])
}

/// cargo xtask test-rust: the tests of both Rust workspaces.
pub fn test_rust(args: Vec<String>) -> Result {
    no_more(&args)?;
    for workspace in RUST_WORKSPACES {
        run(cmd("cargo", ["test", "--workspace", "--quiet"]).current_dir(workspace))?;
    }
    Ok(())
}

/// The kass.db a worker started by hand from the repo root uses, with no
/// --data-dir or KASS_DATA_DIR (backend/config.py). cargo xtask dev and
/// dev-backend use the app's own instead (`util::app_data_dir`).
pub const DEV_DATABASE: &str = "data/kass.db";

/// cargo xtask db-init: creates data/kass.db, or brings it up to date.
pub fn db_init(args: Vec<String>) -> Result {
    no_more(&args)?;
    let database = fs::canonicalize(".")
        .map_err(|error| format!("Couldn't find the repo root: {error}"))?
        .join(DEV_DATABASE);
    prepare_db(&database)
}

/// Creates `database`, or brings it up to date, as the app does before it
/// starts the worker (server/src/db.rs).
pub fn prepare_db(database: &Path) -> Result {
    run(cmd(
        "cargo",
        [
            "run",
            "--quiet",
            "-p",
            "kass-server",
            "--example",
            "prepare_db",
            "--",
        ],
    )
    .arg(database)
    .current_dir(APP_DIR))
}

/// cargo xtask db-reset: deletes the database and creates it again.
pub fn db_reset(args: Vec<String>) -> Result {
    no_more(&args)?;
    let db = Path::new(DEV_DATABASE);
    match fs::remove_file(db) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Couldn't delete {}: {error}", db.display())),
    }
    db_init(vec![])
}

/// cargo xtask py <script> [args]: a repo Python script (model training,
/// benchmarks, evals) with the backend venv's Python, from the repo root.
pub fn py(args: Vec<String>) -> Result {
    let Some(script) = args.first() else {
        return Err("Usage: cargo xtask py <script.py> [args]   e.g. cargo xtask py scripts/shared-adapters/publish.py --check".into());
    };
    if !Path::new(script).is_file() {
        return Err(format!(
            "No script at {script} (paths are from the repo root)."
        ));
    }
    ensure_venv()?;
    run(cmd(venv_bin("python"), &args))
}
