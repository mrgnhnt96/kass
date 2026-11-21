//! Kass's development commands, run from the repo root as `cargo xtask <command>`
//! (the alias is in .cargo/config.toml). macOS on Apple Silicon only.

mod build;
mod dev;
mod install;
mod python;
mod sign;
mod util;
mod version;

use std::process::ExitCode;

use util::Result;

const HELP: &str = "\
Kass development commands (macOS, Apple Silicon). Run from the repo root.

Usage: cargo xtask <command> [args]

Setup
  setup                     Python venv and the pre-push hook
  setup-python [--check]    Create backend/venv (Python 3.12) and install the server's packages
  setup-hooks               Use scripts/hooks (the pre-push lint and format check)

Development
  dev                       Model worker (unless running) and a debug build of the app
  dev-backend               Only the Python model worker, on port 17494
  dev-app                   Only a debug build of the app
  kill                      Stop the dev model worker
  docs                      Open the worker's API docs
  logs                      Follow the backend's log files

Build and release
  build                     The server, then the app
  build-server              The Python server, where the app bundle picks it up
  build-app                 The app, signed the same way every time so updates keep its permissions
  dmg [app] [out.dmg]       Pack the built app into Kass.dmg with the branded window
  install [--help]          Check requirements, build, and install or update /Applications/Kass.app
  release <version>         Cut a release; run through tb release cut v<version>, never by hand
  set-version <version>     Write the version into every file (--check [version] compares them)
  newest-version <v>...     Print the newest of the versions, ordered like the app's updater
  signing-identity          Print the code-signing identity builds use on this Mac

Code quality
  check                     check-python and check-rust
  check-python              ruff lint and format check on backend/
  check-rust                rustfmt check and clippy (warnings fail) on the app and crates/
  lint | format | fix       ruff check, ruff format, or both with fixes, on backend/
  test                      Backend pytest suite, then test-rust
  test-rust                 The Rust tests of the app and crates/

Database
  db-init                   Create data/kass.db (a worker started by hand), or bring it up to date
  db-reset                  Delete the database and create it again

Assets and tools
  update-icons              Regenerate every icon from tauri/assets/kass.icon
  dmg-background            Render the DMG window's background.tiff
  py <script> [args]        Run a repo Python script (model training, benchmarks, evals) with the venv's Python

Clean
  clean                     The app's release build
  clean-python              The venv and Python caches
  clean-all                 Both, and Cargo's build cache

The website builds on its own: cd site && npm ci && npm run build.";

fn dispatch(command: &str, args: Vec<String>) -> Result {
    match command {
        "setup" => python::setup(args),
        "setup-python" => python::setup_python(args),
        "setup-hooks" => python::setup_hooks(args),
        "dev" => dev::dev(args),
        "dev-backend" => dev::dev_backend(args),
        "dev-app" | "dev-frontend" => dev::dev_app(args),
        "kill" => dev::kill(args),
        "docs" => dev::docs(args),
        "logs" => dev::logs(args),
        "build" => build::all(args),
        "build-server" => build::server(args),
        "build-app" | "build-tauri" => build::app(args),
        "dmg" => build::dmg(args),
        "install" => install::install(args),
        "release" => version::release(args),
        "set-version" => version::set(args),
        "newest-version" => version::newest(args),
        "signing-identity" => sign::print(args),
        "check" => python::check(args),
        "check-python" => python::check_python(args),
        "check-rust" => python::check_rust(args),
        "lint" | "lint-python" => python::lint(args),
        "format" | "format-python" => python::format(args),
        "fix" | "fix-python" => python::fix(args),
        "test" => python::test(args),
        "test-rust" => python::test_rust(args),
        "db-init" => python::db_init(args),
        "db-reset" => python::db_reset(args),
        "update-icons" => build::icons(args),
        "dmg-background" => build::dmg_background(args),
        "py" => python::py(args),
        "clean" => dev::clean(args),
        "clean-python" => dev::clean_python(args),
        "clean-all" => dev::clean_all(args),
        _ => Err(format!(
            "Unknown command: {command} (see cargo xtask --help)"
        )),
    }
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let command = args.remove(0);
    if let Err(error) = std::env::set_current_dir(util::root()) {
        eprintln!("error: couldn't go to the repo root: {error}");
        return ExitCode::FAILURE;
    }
    match dispatch(&command, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let s = util::style();
            eprintln!("{}error:{} {error}", s.red, s.reset);
            ExitCode::FAILURE
        }
    }
}
