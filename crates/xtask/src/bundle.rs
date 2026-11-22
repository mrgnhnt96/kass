//! Packing the release build into Kass.app: the bundle layout, its
//! Info.plist, code signing, notarization and the background updater's
//! signed archive.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::util::{
    APP_DIR, BUILT_APP, BUNDLE_ID, Result, SERVER_DIR, TempDir, cmd, output, root, run,
};
use crate::version;

/// What every copy of Kass declares about itself, on top of the app's own
/// Info.plist (its icon, URL schemes and privacy prompts).
const INFO: [(&str, Value); 13] = [
    ("CFBundleDevelopmentRegion", Value::Text("English")),
    ("CFBundleDisplayName", Value::Text("Kass")),
    ("CFBundleExecutable", Value::Text("kass")),
    ("CFBundleIdentifier", Value::Text(BUNDLE_ID)),
    ("CFBundleInfoDictionaryVersion", Value::Text("6.0")),
    ("CFBundleName", Value::Text("Kass")),
    ("CFBundlePackageType", Value::Text("APPL")),
    ("CSResourcesFileMapped", Value::Yes),
    // The oldest macOS the Rust toolchain builds for by default.
    ("LSMinimumSystemVersion", Value::Text("11.0")),
    ("LSRequiresCarbon", Value::Yes),
    ("NSHighResolutionCapable", Value::Yes),
    ("CFBundleShortVersionString", Value::Version),
    ("CFBundleVersion", Value::Version),
];

enum Value {
    Text(&'static str),
    Yes,
    Version,
}

/// Files copied into Contents/Resources, from the app's folder.
const RESOURCES: [(&str, &str); 4] = [
    ("icons/icon.icns", "icon.icns"),
    // The Liquid Glass icon build.rs compiles with actool.
    ("gen/Assets.car", "Assets.car"),
    ("gen/kass.icns", "kass.icns"),
    ("gen/partial.plist", "partial.plist"),
];

/// The app's entitlements: the hardened runtime exceptions the bundled
/// Python server needs, and the microphone.
pub const ENTITLEMENTS: &str = "tauri/src-tauri/Entitlements.plist";

/// The updater's minisign key (the secret key file, or its base64) and its
/// password. Set, the build also packs and signs Kass.app.tar.gz.
const UPDATER_KEY: &str = "KASS_UPDATER_KEY";
const UPDATER_KEY_PASSWORD: &str = "KASS_UPDATER_KEY_PASSWORD";

pub fn updater_key_set() -> bool {
    env::var_os(UPDATER_KEY).is_some_and(|key| !key.is_empty())
}

/// Lay Kass.app out from the release build and the signed server, sign it,
/// and with APPLE_API_KEY set, notarize it and staple the ticket.
pub fn make(identity: &str, timestamp: &str) -> Result {
    let app = Path::new(BUILT_APP);
    if app.exists() {
        fs::remove_dir_all(app)
            .map_err(|error| format!("Couldn't remove the old {BUILT_APP}: {error}"))?;
    }
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    for dir in [&macos, &resources] {
        fs::create_dir_all(dir)
            .map_err(|error| format!("Couldn't create {}: {error}", dir.display()))?;
    }

    copy(
        &Path::new(APP_DIR).join("target/release/kass"),
        &macos.join("kass"),
    )?;
    info_plist(&contents.join("Info.plist"))?;
    for (from, to) in RESOURCES {
        copy(&Path::new(APP_DIR).join(from), &resources.join(to))?;
    }
    copy(
        Path::new("scripts/kass-report.sh"),
        &resources.join("kass-report.sh"),
    )?;
    // ditto keeps the symlinks PyInstaller makes between bundled libraries
    // and the signatures already on them.
    run(cmd(
        "ditto",
        [Path::new(SERVER_DIR), &resources.join("kass-server")],
    ))?;

    // Inside out: the executable, then the bundle, which seals the rest.
    for path in [macos.join("kass"), app.to_path_buf()] {
        run(cmd(
            "codesign",
            [
                "--force",
                timestamp,
                "--options",
                "runtime",
                "--entitlements",
                ENTITLEMENTS,
                "--sign",
                identity,
            ],
        )
        .arg(path))?;
    }
    if env::var_os("APPLE_API_KEY").is_some_and(|key| !key.is_empty()) {
        notarize(app)?;
    }
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|error| format!("Couldn't copy {} into the app: {error}", from.display()))
}

/// The app's Info.plist with the bundle's own keys and the version added.
fn info_plist(to: &Path) -> Result {
    copy(&Path::new(APP_DIR).join("Info.plist"), to)?;
    let version = version::app()?;
    let set = |key: &str, kind: &str, value: &str| {
        run(cmd("plutil", ["-replace", key, kind, value]).arg(to))
    };
    for (key, value) in INFO {
        match value {
            Value::Text(text) => set(key, "-string", text)?,
            Value::Yes => set(key, "-bool", "YES")?,
            Value::Version => set(key, "-string", &version)?,
        }
    }
    run(cmd("plutil", ["-lint"]).arg(to))
}

/// Send the app to Apple's notary service, wait for it, and staple the
/// ticket so it opens without Gatekeeper warnings, even offline. Uses the
/// App Store Connect API key: APPLE_API_KEY (its ID), APPLE_API_ISSUER and
/// APPLE_API_KEY_PATH (the .p8 file).
fn notarize(app: &Path) -> Result {
    let var = |name: &str| {
        env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{name} isn't set; notarizing needs it with APPLE_API_KEY"))
    };
    let (key_id, issuer, key) = (
        var("APPLE_API_KEY")?,
        var("APPLE_API_ISSUER")?,
        var("APPLE_API_KEY_PATH")?,
    );
    let tmp = TempDir::new("notarize")?;
    let zip = tmp.0.join("Kass.zip");
    run(
        cmd("ditto", ["-c", "-k", "--keepParent", "--sequesterRsrc"])
            .arg(app)
            .arg(&zip),
    )?;
    println!("Notarizing Kass.app (this waits for Apple)...");
    let auth = ["--key", &key, "--key-id", &key_id, "--issuer", &issuer];
    let submitted = output(
        cmd("xcrun", ["notarytool", "submit"])
            .arg(&zip)
            .args(auth)
            .args(["--wait", "--output-format", "json"]),
    )?;
    let reply: serde_json::Value = serde_json::from_str(&submitted)
        .map_err(|error| format!("notarytool's reply isn't JSON ({error}): {submitted}"))?;
    let status = reply["status"].as_str().unwrap_or("");
    if status != "Accepted" {
        let id = reply["id"].as_str().unwrap_or("");
        let log = output(cmd("xcrun", ["notarytool", "log", id]).args(auth)).unwrap_or_default();
        return Err(format!("Apple didn't notarize the app ({status}):\n{log}"));
    }
    run(cmd("xcrun", ["stapler", "staple", "-v"]).arg(app))
}

/// Kass.app.tar.gz and its .sig next to the app: what installed copies
/// download in the background. The signature is the minisign signature file,
/// base64-encoded, as latest.json carries it.
pub fn updater_archive() -> Result {
    let archive = PathBuf::from(format!("{BUILT_APP}.tar.gz"));
    pack(Path::new(BUILT_APP), &archive)?;
    let secret = updater_key()?;
    let public = minisign::PublicKeyBox::from_string(&base64_text(
        &app_public_key()?,
        "the updater's public key",
    )?)
    .and_then(|boxed| boxed.into_public_key())
    .map_err(|error| format!("The app's updater public key doesn't read: {error}"))?;
    if secret.keynum() != public.keynum() {
        return Err(format!(
            "{UPDATER_KEY} isn't the key the app's updater trusts (PUBLIC_KEY in src/updater.rs)"
        ));
    }
    let path = PathBuf::from(format!("{BUILT_APP}.tar.gz.sig"));
    fs::write(&path, signature(&secret, &archive)?)
        .map_err(|error| format!("Couldn't write {}: {error}", path.display()))?;
    println!("Packed {} and its signature", archive.display());
    Ok(())
}

/// A gzipped tar of the app with the Kass.app folder at its top, symlinks
/// kept as links.
fn pack(app: &Path, archive: &Path) -> Result {
    let file = fs::File::create(archive)
        .map_err(|error| format!("Couldn't create {}: {error}", archive.display()))?;
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));
    tar.follow_symlinks(false);
    tar.append_dir_all("Kass.app", app)
        .and_then(|()| tar.into_inner())
        .and_then(|gz| gz.finish())
        .and_then(|mut file| file.flush())
        .map_err(|error| format!("Couldn't pack {}: {error}", archive.display()))
}

/// The updater's secret key: KASS_UPDATER_KEY holds the key file's text
/// base64-encoded, or the path of a file holding that.
fn updater_key() -> Result<minisign::SecretKey> {
    let key = env::var(UPDATER_KEY).unwrap_or_default();
    let key = if Path::new(&key).is_file() {
        fs::read_to_string(&key).map_err(|error| format!("Couldn't read {UPDATER_KEY}: {error}"))?
    } else {
        key
    };
    let password = env::var(UPDATER_KEY_PASSWORD).unwrap_or_default();
    minisign::SecretKeyBox::from_string(&base64_text(&key, UPDATER_KEY)?)
        .and_then(|boxed| boxed.into_secret_key(Some(password)))
        .map_err(|error| format!("{UPDATER_KEY} or its password is wrong: {error}"))
}

/// The file's minisign signature, base64-encoded.
fn signature(secret: &minisign::SecretKey, file: &Path) -> Result<String> {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let reader = fs::File::open(file)
        .map_err(|error| format!("Couldn't read {}: {error}", file.display()))?;
    let signature = minisign::sign(
        None,
        secret,
        reader,
        Some(&format!("timestamp:{now}\tfile:{name}")),
        Some("signature from kass secret key"),
    )
    .map_err(|error| format!("Couldn't sign {}: {error}", file.display()))?;
    Ok(BASE64.encode(signature.into_string()))
}

fn base64_text(text: &str, what: &str) -> Result<String> {
    BASE64
        .decode(text.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| format!("{what} isn't base64 text"))
}

/// The public key installed copies check downloads against.
fn app_public_key() -> Result<String> {
    let source = root().join(APP_DIR).join("src/updater.rs");
    let text = fs::read_to_string(&source)
        .map_err(|error| format!("Couldn't read {}: {error}", source.display()))?;
    text.lines()
        .find_map(|line| line.strip_prefix("const PUBLIC_KEY: &str = \""))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_string)
        .ok_or_else(|| format!("No PUBLIC_KEY in {}", source.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_holds_the_app_folder_and_its_links() {
        let tmp = TempDir::new("pack-test").unwrap();
        let app = tmp.0.join("Built.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/kass"), b"binary").unwrap();
        std::os::unix::fs::symlink("kass", app.join("Contents/MacOS/link")).unwrap();
        let archive = tmp.0.join("Kass.app.tar.gz");
        pack(&app, &archive).unwrap();

        let file = fs::File::open(&archive).unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut entries: Vec<(String, bool)> = tar
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path().unwrap().to_string_lossy().into_owned();
                (path, entry.header().entry_type().is_symlink())
            })
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            [
                ("Kass.app/".into(), false),
                ("Kass.app/Contents".into(), false),
                ("Kass.app/Contents/MacOS".into(), false),
                ("Kass.app/Contents/MacOS/kass".into(), false),
                ("Kass.app/Contents/MacOS/link".into(), true),
            ]
        );
    }

    /// The app decodes latest.json's signature from base64 and checks it
    /// with minisign (src/updater.rs).
    #[test]
    fn the_signature_verifies_the_way_the_updater_reads_it() {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let tmp = TempDir::new("sign-test").unwrap();
        let file = tmp.0.join("Kass.app.tar.gz");
        fs::write(&file, b"archive").unwrap();

        let encoded = signature(&pair.sk, &file).unwrap();
        let text = base64_text(&encoded, "signature").unwrap();
        let signature = minisign::SignatureBox::from_string(&text).unwrap();
        assert!(
            signature
                .trusted_comment()
                .unwrap()
                .ends_with("\tfile:Kass.app.tar.gz")
        );
        minisign::verify(
            &pair.pk,
            &signature,
            std::io::Cursor::new(b"archive"),
            true,
            false,
            false,
        )
        .unwrap();
        let tampered = minisign::verify(
            &pair.pk,
            &signature,
            std::io::Cursor::new(b"other"),
            true,
            false,
            false,
        );
        assert!(tampered.is_err());
    }

    #[test]
    fn the_app_public_key_is_found() {
        let key = base64_text(&app_public_key().unwrap(), "key").unwrap();
        minisign::PublicKeyBox::from_string(&key)
            .unwrap()
            .into_public_key()
            .unwrap();
    }
}
