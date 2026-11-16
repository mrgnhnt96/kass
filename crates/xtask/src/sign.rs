//! The code-signing identity to sign Kass with on this Mac.
//!
//! macOS keeps an app's privacy grants (Microphone, Accessibility, Input
//! Monitoring) only while its signature's designated requirement stays the
//! same. Ad-hoc signing changes it on every build, so every update would lose
//! them. Signing every build with the same identity keeps them.
//!
//! The identity, in order: KASS_SIGNING_IDENTITY; a Developer ID Application
//! certificate (what releases are signed with, so a local build and a
//! downloaded one keep the same grants); the one the installed app is signed
//! with; the one used last time; an Apple Development certificate; otherwise a
//! self-signed "Kass Local Signing" certificate, made once and kept in its own
//! keychain.

use std::env;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::util::{Result, TempDir, all_output, cmd, home, no_more, output, run};

/// cargo xtask signing-identity: prints the identity's SHA-1.
pub fn print(args: Vec<String>) -> Result {
    no_more(&args)?;
    println!("{}", identity()?);
    Ok(())
}

struct Signing {
    state: PathBuf,
    keychain: PathBuf,
    local_name: String,
}

impl Signing {
    fn password_file(&self) -> PathBuf {
        self.state.join("keychain-password")
    }

    fn password(&self) -> Result<String> {
        fs::read_to_string(self.password_file())
            .map(|text| text.trim().to_string())
            .map_err(|error| format!("Couldn't read the signing keychain's password: {error}"))
    }

    /// codesign only finds keys in unlocked keychains on the search list.
    fn open_keychain(&self) -> Result {
        let keychain = self.keychain.to_string_lossy().into_owned();
        run(&mut cmd(
            "security",
            ["unlock-keychain", "-p", &self.password()?, &keychain],
        ))?;
        let listed: Vec<String> = output(&mut cmd("security", ["list-keychains", "-d", "user"]))?
            .lines()
            .map(|line| line.trim().trim_matches('"').to_string())
            .filter(|line| !line.is_empty())
            .collect();
        if listed.contains(&keychain) {
            return Ok(());
        }
        run(cmd("security", ["list-keychains", "-d", "user", "-s"])
            .args(&listed)
            .arg(&keychain))
    }

    /// The hash of the named identity, remembering the name for next time.
    fn pick(&self, name: &str) -> Option<String> {
        let (hash, _) = identities().into_iter().find(|(_, found)| found == name)?;
        let _ = fs::write(self.state.join("identity"), format!("{name}\n"));
        Some(hash)
    }

    fn create_local_identity(&self) -> Result {
        let pem = self.state.join("local-signing.pem");
        let name = &self.local_name;
        if !pem.is_file() {
            eprintln!("Creating a code-signing certificate for this Mac, \"{name}\".");
            let tmp = TempDir::new("signing")?;
            let at = |file: &str| tmp.0.join(file).to_string_lossy().into_owned();
            fs::write(
                at("cert.cnf"),
                format!(
                    "[req]\ndistinguished_name = dn\nx509_extensions = ext\nprompt = no\n[dn]\nCN = {name}\n[ext]\n\
                     basicConstraints = critical, CA:false\nkeyUsage = critical, digitalSignature\n\
                     extendedKeyUsage = critical, codeSigning\n"
                ),
            )
            .map_err(|error| format!("Couldn't write the certificate request: {error}"))?;
            let openssl = "/usr/bin/openssl";
            output(&mut cmd(
                openssl,
                [
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-days",
                    "7300",
                    "-config",
                    &at("cert.cnf"),
                    "-keyout",
                    &at("key.pem"),
                    "-out",
                    &at("cert.pem"),
                ],
            ))?;
            run(&mut cmd(
                openssl,
                [
                    "pkcs12",
                    "-export",
                    "-inkey",
                    &at("key.pem"),
                    "-in",
                    &at("cert.pem"),
                    "-name",
                    name,
                    "-out",
                    &at("identity.p12"),
                    "-passout",
                    "pass:kass",
                ],
            ))?;
            let keychain = self.keychain.to_string_lossy().into_owned();
            if !self.keychain.is_file() {
                let password = output(&mut cmd(openssl, ["rand", "-hex", "24"]))?;
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(self.password_file())
                    .and_then(|mut file| {
                        std::io::Write::write_all(&mut file, format!("{password}\n").as_bytes())
                    })
                    .map_err(|error| {
                        format!("Couldn't save the signing keychain's password: {error}")
                    })?;
                run(&mut cmd(
                    "security",
                    ["create-keychain", "-p", &password, &keychain],
                ))?;
                // Never lock on its own, so builds don't stop to ask for it.
                run(&mut cmd("security", ["set-keychain-settings", &keychain]))?;
            }
            let password = self.password()?;
            self.open_keychain()?;
            output(&mut cmd(
                "security",
                [
                    "import",
                    &at("identity.p12"),
                    "-k",
                    &keychain,
                    "-P",
                    "kass",
                    "-T",
                    "/usr/bin/codesign",
                ],
            ))?;
            output(&mut cmd(
                "security",
                [
                    "set-key-partition-list",
                    "-S",
                    "apple-tool:,apple:,codesign:",
                    "-s",
                    "-k",
                    &password,
                    &keychain,
                ],
            ))?;
            fs::copy(at("cert.pem"), &pem)
                .map_err(|error| format!("Couldn't save the certificate: {error}"))?;
        }
        eprintln!("macOS will ask for your password to trust \"{name}\" for code signing.");
        eprintln!("This happens once; later installs reuse it.");
        run(cmd(
            "security",
            [
                "add-trusted-cert",
                "-r",
                "trustRoot",
                "-p",
                "codeSign",
                "-k",
            ],
        )
        .arg(&self.keychain)
        .arg(&pem))
    }
}

/// "HASH NAME" for each valid code-signing identity.
fn identities() -> Vec<(String, String)> {
    parse_identities(&all_output(&mut cmd(
        "security",
        ["find-identity", "-v", "-p", "codesigning"],
    )))
}

fn parse_identities(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.trim_start().split_once(") ")?;
            let (hash, name) = rest.split_once(' ')?;
            let name = name.strip_prefix('"')?.strip_suffix('"')?;
            (hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| (hash.to_string(), name.to_string()))
        })
        .collect()
}

fn first_named(prefix: &str) -> Option<String> {
    identities()
        .into_iter()
        .map(|(_, name)| name)
        .find(|name| name.starts_with(prefix))
}

/// The SHA-1 of the identity to sign with, creating a local one the first
/// time there is none. Messages for the person go to stderr.
pub fn identity() -> Result<String> {
    if let Ok(identity) = env::var("KASS_SIGNING_IDENTITY")
        && !identity.is_empty()
    {
        return Ok(identity);
    }

    let support = home().join("Library/Application Support");
    let keychains = home().join("Library/Keychains");
    let mut signing = Signing {
        state: support.join("Kass Installer"),
        keychain: keychains.join("kass-signing.keychain-db"),
        local_name: "Kass Local Signing".into(),
    };
    let mut app = PathBuf::from("/Applications/Kass.app");
    // Kass was Herga, and before that Voicebox: keep using what an earlier
    // install set up, so the signature stays the same.
    if !app.is_dir() {
        app = PathBuf::from("/Applications/Herga.app");
    }
    for old in ["Herga", "Voicebox"] {
        let old_state = support.join(format!("{old} Installer"));
        if old_state.is_dir() && !signing.state.exists() {
            let _ = fs::rename(&old_state, &signing.state);
        }
        let old_keychain = keychains.join(format!("{}-signing.keychain-db", old.to_lowercase()));
        if old_keychain.is_file() && !signing.keychain.is_file() {
            signing.keychain = old_keychain;
            signing.local_name = format!("{old} Local Signing");
            break;
        }
    }
    fs::create_dir_all(&signing.state)
        .map_err(|error| format!("Couldn't create {}: {error}", signing.state.display()))?;

    if signing.keychain.is_file() && signing.password_file().is_file() {
        signing.open_keychain()?;
    }

    if let Some(hash) =
        first_named("Developer ID Application: ").and_then(|name| signing.pick(&name))
    {
        return Ok(hash);
    }
    if app.is_dir()
        && let Some(hash) = authority(&app).and_then(|name| signing.pick(&name))
    {
        return Ok(hash);
    }
    if let Ok(last) = fs::read_to_string(signing.state.join("identity"))
        && let Some(hash) = signing.pick(last.trim())
    {
        return Ok(hash);
    }
    if let Some(hash) = first_named("Apple Development: ").and_then(|name| signing.pick(&name)) {
        return Ok(hash);
    }

    signing.create_local_identity()?;
    signing.pick(&signing.local_name).ok_or_else(|| {
        format!(
            "Could not set up \"{0}\" for code signing.\nOpen Keychain Access, find it in the {1} keychain, and set\n\
             Trust > Code Signing to Always Trust, then run the install again.",
            signing.local_name,
            signing.keychain.file_stem().unwrap_or_default().to_string_lossy().trim_end_matches(".keychain")
        )
    })
}

/// The first Authority an app is signed by.
pub fn authority(app: &Path) -> Option<String> {
    all_output(cmd("codesign", ["-dvv"]).arg(app))
        .lines()
        .find_map(|line| line.strip_prefix("Authority="))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::parse_identities;

    #[test]
    fn reads_find_identity_lines() {
        let listing = "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Apple Development: Someone (TEAM)\"\n\
                       \x20    1 valid identities found\n";
        assert_eq!(
            parse_identities(listing),
            [(
                "0123456789ABCDEF0123456789ABCDEF01234567".to_string(),
                "Apple Development: Someone (TEAM)".to_string()
            )]
        );
    }
}
