//! Building the server and the app, packing the DMG and regenerating images.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sign;
use crate::util::{
    APP_DIR, BACKEND, BUILT_APP, Result, SERVER_DIR, TempDir, all_output, cmd, ensure_venv, home,
    is_executable, no_more, on_path, output, path_with, root, run, succeeds, venv_bin,
};

/// The Tauri CLI only packs and signs the bundle; the app no longer uses Tauri.
const TAURI_CLI: &str = "2.9.6";

/// Cargo's own bin folder, where rustup and cargo install put tools; a fresh
/// shell may not have it on PATH yet.
fn cargo_path() -> std::ffi::OsString {
    path_with(&[home().join(".cargo/bin")])
}

/// cargo xtask build: the server, then the app.
pub fn all(args: Vec<String>) -> Result {
    no_more(&args)?;
    server(vec![])?;
    app(vec![])
}

/// cargo xtask build-server: the Python server as a PyInstaller folder,
/// copied where the bundle picks it up (bundle.macOS.files puts it in
/// Kass.app/Contents/Resources).
pub fn server(args: Vec<String>) -> Result {
    no_more(&args)?;
    ensure_venv()?;
    let platform =
        output(&mut cmd("rustc", ["--print", "host-tuple"])).unwrap_or_else(|_| "unknown".into());
    println!("Building Kass server for platform: {platform}");
    let python = root().join(venv_bin("python"));
    let in_backend = |args: &[&str]| {
        let mut command = cmd(&python, args);
        command
            .current_dir(BACKEND)
            .env("PATH", path_with(&[root().join(venv_bin(""))]));
        command
    };
    if !succeeds(&mut in_backend(&["-c", "import PyInstaller"])) {
        println!("Installing PyInstaller...");
        run(&mut in_backend(&["-m", "pip", "install", "pyinstaller"]))?;
    }
    run(&mut in_backend(&["build_binary.py"]))?;

    let binaries = Path::new(SERVER_DIR)
        .parent()
        .expect("the server sits in binaries/");
    fs::create_dir_all(binaries)
        .map_err(|error| format!("Couldn't create {}: {error}", binaries.display()))?;
    if let Ok(entries) = fs::read_dir(binaries) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("kass-server")
            {
                let _ = fs::remove_dir_all(entry.path()).or_else(|_| fs::remove_file(entry.path()));
            }
        }
    }
    let built = Path::new(BACKEND).join("dist/kass-server");
    if !is_executable(&built.join("kass-server")) {
        return Err(format!("{}/kass-server not found", built.display()));
    }
    // ditto keeps the symlinks PyInstaller makes between bundled libraries.
    run(cmd("ditto", [&built, Path::new(SERVER_DIR)]))?;
    println!("Built kass-server for {platform}");
    Ok(())
}

/// cargo xtask build-app: the app, signed with the same identity every time
/// so updates keep its privacy grants. The server must be built already.
///
/// With APPLE_API_KEY set (release.yml) every signature gets a secure
/// timestamp for notarization; with TAURI_SIGNING_PRIVATE_KEY set the bundler
/// also packs a signed .app.tar.gz for the background updater.
pub fn app(args: Vec<String>) -> Result {
    no_more(&args)?;
    let identity = sign::identity()?;

    // The server folder is copied into the app as-is, so sign its code first:
    // every library, then the executable with the app's hardened-runtime
    // entitlements. The bundler then seals the folder into the app signature.
    let server = Path::new(SERVER_DIR);
    if !is_executable(&server.join("kass-server")) {
        return Err(format!(
            "Server not built at {SERVER_DIR}. Run cargo xtask build-server first."
        ));
    }
    // Local builds skip the round trip to Apple for a timestamp.
    let timestamp = if env::var_os("APPLE_API_KEY").is_some_and(|key| !key.is_empty()) {
        "--timestamp"
    } else {
        "--timestamp=none"
    };
    let libraries = mach_o_files(&server.join("_internal"))?;
    // Eight codesign runs at a time, 32 files each.
    for batch in libraries.chunks(32 * 8) {
        let running = batch
            .chunks(32)
            .map(|chunk| {
                cmd(
                    "codesign",
                    [
                        "--force",
                        timestamp,
                        "--options",
                        "runtime",
                        "--sign",
                        &identity,
                    ],
                )
                .args(chunk)
                .spawn()
                .map_err(|error| format!("Couldn't run codesign: {error}"))
            })
            .collect::<Result<Vec<_>>>()?;
        for mut child in running {
            let status = child.wait().map_err(|error| format!("codesign: {error}"))?;
            if !status.success() {
                return Err(format!(
                    "codesign failed on the server's libraries ({status})"
                ));
            }
        }
    }
    run(cmd(
        "codesign",
        [
            "--force",
            timestamp,
            "--options",
            "runtime",
            "--entitlements",
            "tauri/src-tauri/Entitlements.plist",
        ],
    )
    .args(["--sign", &identity])
    .arg(server.join("kass-server")))?;

    let tmp = TempDir::new("bundle-config")?;
    let config = tmp.0.join("signing.json");
    let updater = env::var_os("TAURI_SIGNING_PRIVATE_KEY").is_some_and(|key| !key.is_empty());
    fs::write(
        &config,
        format!(
            r#"{{"bundle": {{"createUpdaterArtifacts": {updater}, "macOS": {{"signingIdentity": "{}"}}}}}}"#,
            identity.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .map_err(|error| format!("Couldn't write the bundle config: {error}"))?;

    let path = cargo_path();
    let installed = all_output(cmd("cargo", ["tauri", "--version"]).env("PATH", &path));
    if installed.trim() != format!("tauri-cli {TAURI_CLI}") {
        run(cmd(
            "cargo",
            ["install", "tauri-cli", "--version", TAURI_CLI, "--locked"],
        )
        .env("PATH", &path))?;
    }
    run(cmd("cargo", ["build", "--release"])
        .current_dir(APP_DIR)
        .env("PATH", &path))?;
    run(
        cmd("cargo", ["tauri", "bundle", "--bundles", "app", "--config"])
            .arg(&config)
            .current_dir(APP_DIR)
            .env("PATH", &path),
    )?;
    run(&mut cmd(
        "codesign",
        ["--verify", "--deep", "--strict", BUILT_APP],
    ))
}

/// Every Mach-O file under the folder.
fn mach_o_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|error| format!("Couldn't read {}: {error}", dir.display()))?;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() && is_mach_o(&entry.path()) {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_mach_o(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let read = fs::File::open(path).and_then(|mut file| file.read_exact(&mut magic));
    read.is_ok()
        && matches!(
            u32::from_be_bytes(magic),
            0xfeedface | 0xfeedfacf | 0xcefaedfe | 0xcffaedfe | 0xcafebabe | 0xbebafeca
        )
}

/// cargo xtask dmg [app] [out.dmg]: packs the app into a DMG with the branded
/// window (background from tauri/assets/dmg, the app on the left and
/// Applications on the right; the layout is scripts/dmg-settings.py).
pub fn dmg(args: Vec<String>) -> Result {
    let app = PathBuf::from(args.first().map_or(BUILT_APP, String::as_str));
    let out = args.get(1).map_or("Kass.dmg", String::as_str).to_string();
    no_more(args.get(2..).unwrap_or_default())?;
    if !app.is_dir() {
        return Err(format!(
            "No app at {}. Build it first (cargo xtask build).",
            app.display()
        ));
    }
    let app = app
        .canonicalize()
        .map_err(|error| format!("Couldn't find {}: {error}", app.display()))?;

    // dmgbuild writes Finder's layout file directly, so it works on CI
    // without driving Finder. 1.6.7 is the first whose background link still
    // works once the image is compressed on macOS 26, and it needs Python 3.10+.
    let python = ["python3.12", "python3.13", "python3"]
        .into_iter()
        .find(|candidate| {
            on_path(candidate)
                && succeeds(&mut cmd(
                    candidate,
                    ["-c", "import sys; sys.exit(sys.version_info < (3, 10))"],
                ))
        })
        .ok_or("Needs Python 3.10 or newer: brew install python@3.12")?;
    let venv = TempDir::new("dmgbuild")?;
    run(cmd(python, ["-m", "venv"]).arg(&venv.0))?;
    run(cmd(
        venv.0.join("bin/pip"),
        [
            "install",
            "--quiet",
            "--disable-pip-version-check",
            "dmgbuild==1.6.7",
        ],
    ))?;
    run(cmd(
        venv.0.join("bin/dmgbuild"),
        ["-s", "scripts/dmg-settings.py", "-D"],
    )
    .arg(format!("app={}", app.display()))
    .arg("-D")
    .arg(format!("root={}", root().display()))
    .args(["Kass", &out]))?;
    println!("Wrote {out}");
    Ok(())
}

/// cargo xtask dmg-background: renders tauri/assets/dmg/background.html to
/// background.tiff at 1x and 2x, so it's sharp on Retina screens. Commit the
/// TIFF; releases use the committed file. Needs Chrome or Chromium (CHROME
/// picks one) and site/node_modules for the font.
pub fn dmg_background(args: Vec<String>) -> Result {
    no_more(&args)?;
    let dir = "tauri/assets/dmg";
    let font = "site/node_modules/@fontsource-variable/bricolage-grotesque";
    if !Path::new(font).is_dir() {
        return Err(format!("Missing {font}. Run: cd site && npm ci"));
    }
    let chrome = match env::var("CHROME") {
        Ok(chrome) if !chrome.is_empty() => PathBuf::from(chrome),
        _ => find_chrome().ok_or("Chrome not found. Install it or set CHROME=/path/to/chrome.")?,
    };
    let tmp = TempDir::new("dmg-background")?;
    let page = format!("file://{}/{dir}/background.html", root().display());
    let shots: Vec<PathBuf> = [1, 2]
        .iter()
        .map(|scale| tmp.0.join(format!("background@{scale}x.png")))
        .collect();
    for (scale, shot) in [1, 2].iter().zip(&shots) {
        let _ = all_output(
            cmd(
                &chrome,
                [
                    "--headless",
                    "--disable-gpu",
                    "--hide-scrollbars",
                    "--allow-file-access-from-files",
                    "--window-size=660,420",
                    &format!("--force-device-scale-factor={scale}"),
                ],
            )
            .arg(format!("--screenshot={}", shot.display()))
            .arg(&page),
        );
    }
    let sizes = output(cmd("sips", ["-g", "pixelWidth", "-g", "pixelHeight"]).args(&shots))?;
    sizes
        .lines()
        .filter(|line| line.contains("pixel"))
        .for_each(|line| println!("{line}"));
    // One TIFF holding both sizes; Finder picks the one for the screen.
    run(cmd("tiffutil", ["-cathidpicheck"])
        .args(&shots)
        .args(["-out", &format!("{dir}/background.tiff")]))?;
    println!("Wrote {dir}/background.tiff");
    Ok(())
}

fn find_chrome() -> Option<PathBuf> {
    let fixed = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    if let Some(found) = fixed
        .iter()
        .map(PathBuf::from)
        .find(|path| is_executable(path))
    {
        return Some(found);
    }
    // Playwright's headless shell: ms-playwright/chromium_headless_shell-*/*/chrome-headless-shell
    let cache = home().join("Library/Caches/ms-playwright");
    let mut found = Vec::new();
    for shell in fs::read_dir(cache).ok()?.flatten() {
        if !shell
            .file_name()
            .to_string_lossy()
            .starts_with("chromium_headless_shell-")
        {
            continue;
        }
        for build in fs::read_dir(shell.path()).into_iter().flatten().flatten() {
            let binary = build.path().join("chrome-headless-shell");
            if is_executable(&binary) {
                found.push(binary);
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

/// cargo xtask update-icons: regenerates every Kass icon from the Icon
/// Composer source, tauri/assets/kass.icon.
///
/// 1. Renders the Liquid Glass appearances with ictool.
/// 2. Compiles kass.icon with actool into Assets.car and kass.icns.
/// 3. Writes the bundle's fallback PNGs and icon.icns.
/// 4. Writes the in-app logo, the site icons and the README icon.
///
/// site/public/favicon.svg is a hand-drawn SVG with thicker strokes so the
/// mark holds up at 16 px; edit it directly. Needs Xcode 26+ (ictool),
/// ImageMagick (the README webp) and site/node_modules (sharp, for the
/// apple-touch icon).
pub fn icons(args: Vec<String>) -> Result {
    no_more(&args)?;
    let xcode = output(&mut cmd("xcode-select", ["-p"]))?;
    let ictool =
        PathBuf::from(xcode).join("../Applications/Icon Composer.app/Contents/Executables/ictool");
    let bundle = "tauri/assets/kass.icon";
    let exports = "tauri/assets/kass_exports";
    let icons = "tauri/src-tauri/icons";
    let generated = "tauri/src-tauri/gen";
    let source = format!("{exports}/kass-macOS-Default-1024x1024@1x.png");
    if !is_executable(&ictool) {
        return Err(format!(
            "ictool not found at {} (install Xcode 26 or later)",
            ictool.display()
        ));
    }
    let mkdir = |dir: &str| {
        fs::create_dir_all(dir).map_err(|error| format!("Couldn't create {dir}: {error}"))
    };
    let copy = |from: &str, to: &str| {
        fs::copy(from, to)
            .map(|_| ())
            .map_err(|error| format!("Couldn't copy {from} to {to}: {error}"))
    };
    let resize = |size: u32, out: &str| {
        let size = size.to_string();
        output(&mut cmd(
            "sips",
            [
                "-s", "format", "png", "-z", &size, &size, &source, "--out", out,
            ],
        ))
        .map(|_| ())
    };

    println!("Updating all Kass icons from {bundle}");
    println!("Rendering appearances...");
    mkdir(exports)?;
    for entry in fs::read_dir(exports)
        .map_err(|error| format!("Couldn't read {exports}: {error}"))?
        .flatten()
    {
        if entry.path().extension().is_some_and(|ext| ext == "png") {
            let _ = fs::remove_file(entry.path());
        }
    }
    for rendition in [
        "Default",
        "Dark",
        "ClearLight",
        "ClearDark",
        "TintedLight",
        "TintedDark",
    ] {
        let out = format!("{exports}/kass-macOS-{rendition}-1024x1024@1x.png");
        output(
            cmd(
                &ictool,
                [
                    bundle,
                    "--export-image",
                    "--output-file",
                    &out,
                    "--platform",
                    "macOS",
                    "--rendition",
                    rendition,
                ],
            )
            .args(["--width", "1024", "--height", "1024", "--scale", "1"]),
        )?;
    }

    // The same actool call as tauri/src-tauri/build.rs.
    println!("Compiling kass.icon with actool...");
    mkdir(generated)?;
    for file in ["kass.icns", "Assets.car", "partial.plist"] {
        let _ = fs::remove_file(Path::new(generated).join(file));
    }
    output(&mut cmd(
        "xcrun",
        [
            "actool",
            "--compile",
            generated,
            "--output-format",
            "human-readable-text",
            "--output-partial-info-plist",
            &format!("{generated}/partial.plist"),
            "--app-icon",
            "kass",
            "--include-all-app-icons",
            "--target-device",
            "mac",
            "--minimum-deployment-target",
            "11.0",
            "--platform",
            "macosx",
            bundle,
        ],
    ))?;

    println!("Writing bundle icons...");
    mkdir(icons)?;
    for (size, name) in [
        (32, "32x32.png"),
        (64, "64x64.png"),
        (128, "128x128.png"),
        (256, "128x128@2x.png"),
        (512, "icon.png"),
    ] {
        resize(size, &format!("{icons}/{name}"))?;
    }
    copy(
        &format!("{generated}/kass.icns"),
        &format!("{icons}/icon.icns"),
    )?;
    // build.rs falls back to this PNG when actool can't produce kass.icns.
    copy(&source, &format!("{bundle}/Assets/Kass.png"))?;

    println!("Writing app, site and README icons...");
    copy(&source, "tauri/assets/kass-logo.png")?;
    resize(512, "site/public/icon.png")?;
    resize(256, "site/src/assets/icon.png")?;
    // iOS masks the home-screen icon itself, so it must be a full-bleed square.
    let touch_icon = r##"<svg xmlns="http://www.w3.org/2000/svg" width="180" height="180" viewBox="0 0 100 100"><rect width="100" height="100" fill="#111318"/><g fill="none" stroke-width="8" stroke-linecap="round"><path d="M18 58 Q 34 55, 54 32" stroke="#F4F2EC"/><path d="M22 74 Q 48 70, 80 26" stroke="#5CF2B0"/><path d="M36 82 Q 60 78, 82 50" stroke="#F4F2EC"/></g></svg>"##;
    run(cmd(
        "node",
        ["-e", "require('sharp')(Buffer.from(process.argv[1])).png().toFile('public/apple-touch-icon.png')", touch_icon],
    )
    .current_dir("site"))?;
    run(&mut cmd(
        "magick",
        [
            &format!("{exports}/kass-macOS-Dark-1024x1024@1x.png"),
            "-resize",
            "256x256",
            "docs/assets/icon-dark.webp",
        ],
    ))?;
    println!("Icons updated. Rebuild the app to pick them up.");
    Ok(())
}
