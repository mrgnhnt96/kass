//! Running commands, the repo's paths and printing.

use std::borrow::BorrowMut;
use std::env;
use std::ffi::OsStr;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What went wrong, already worded for the person running the command.
pub type Result<T = ()> = std::result::Result<T, String>;

pub const BACKEND: &str = "backend";
pub const VENV: &str = "backend/venv";
pub const APP_DIR: &str = "tauri/src-tauri";
pub const SERVER_DIR: &str = "tauri/src-tauri/binaries/kass-server";
pub const BUILT_APP: &str = "tauri/src-tauri/target/release/bundle/macos/Kass.app";
/// The port the Python model worker listens on; the app serves 17493 itself
/// and passes model requests on to it.
pub const WORKER_PORT: u16 = 17494;

/// The repo root: crates/xtask/../..
pub fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    root.canonicalize().unwrap_or(root)
}

/// A tool in the backend's virtual environment.
pub fn venv_bin(tool: &str) -> PathBuf {
    Path::new(VENV).join("bin").join(tool)
}

pub fn ensure_venv() -> Result {
    if Path::new(VENV).is_dir() {
        Ok(())
    } else {
        Err("Python venv not found. Run: cargo xtask setup".into())
    }
}

pub fn cmd<S: AsRef<OsStr>>(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = S>,
) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

fn describe(command: &Command) -> String {
    let mut parts = vec![command.get_program().to_string_lossy().into_owned()];
    parts.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    parts.join(" ")
}

/// Runs the command with this terminal's input and output; fails unless it exits 0.
pub fn run(mut command: impl BorrowMut<Command>) -> Result {
    let command = command.borrow_mut();
    let status = command
        .status()
        .map_err(|error| format!("Couldn't run {}: {error}", describe(command)))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} failed ({status})", describe(command)))
    }
}

/// The command's stdout, trimmed; its stderr goes to the terminal. Fails unless it exits 0.
pub fn output(mut command: impl BorrowMut<Command>) -> Result<String> {
    let command = command.borrow_mut();
    let result = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("Couldn't run {}: {error}", describe(command)))?;
    if result.status.success() {
        Ok(String::from_utf8_lossy(&result.stdout).trim().to_string())
    } else {
        Err(format!("{} failed ({})", describe(command), result.status))
    }
}

/// stdout and stderr together, whether or not the command succeeded; empty
/// when it can't start.
pub fn all_output(mut command: impl BorrowMut<Command>) -> String {
    let command = command.borrow_mut();
    match command.stdin(Stdio::null()).output() {
        Ok(result) => {
            let mut text = String::from_utf8_lossy(&result.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&result.stderr));
            text
        }
        Err(_) => String::new(),
    }
}

/// Whether the command exits 0, with its output hidden.
pub fn succeeds(mut command: impl BorrowMut<Command>) -> bool {
    let command = command.borrow_mut();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Where a program by this name is on PATH.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

pub fn on_path(program: &str) -> bool {
    which(program).is_some()
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// PATH with these directories in front, for child processes.
pub fn path_with(dirs: &[PathBuf]) -> std::ffi::OsString {
    let mut all: Vec<PathBuf> = dirs.to_vec();
    if let Some(path) = env::var_os("PATH") {
        all.extend(env::split_paths(&path));
    }
    env::join_paths(all).unwrap_or_default()
}

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// A fresh directory under the system's temp folder, removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Result<Self> {
        let path = output(&mut cmd(
            "mktemp",
            ["-d", &format!("{}/kass-{name}.XXXXXX", temp_root())],
        ))?;
        Ok(Self(PathBuf::from(path)))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_root() -> String {
    env::var("TMPDIR")
        .map(|dir| dir.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "/tmp".into())
}

/// Bold, dim and colours on a terminal; plain text in a pipe or a log.
pub struct Style {
    pub bold: &'static str,
    pub dim: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub reset: &'static str,
}

pub fn style() -> Style {
    if std::io::stdout().is_terminal() {
        Style {
            bold: "\x1b[1m",
            dim: "\x1b[2m",
            red: "\x1b[31m",
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            reset: "\x1b[0m",
        }
    } else {
        Style {
            bold: "",
            dim: "",
            red: "",
            green: "",
            yellow: "",
            reset: "",
        }
    }
}

/// Takes `--flag` out of the arguments, saying whether it was there.
pub fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|arg| arg != flag);
    args.len() != before
}

/// Fails on anything left after the command's own arguments.
pub fn no_more(args: &[String]) -> Result {
    match args.first() {
        None => Ok(()),
        Some(arg) => Err(format!(
            "Unexpected argument: {arg} (see cargo xtask --help)"
        )),
    }
}
