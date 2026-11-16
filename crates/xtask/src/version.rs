//! The app version: writing it everywhere, comparing versions and cutting a release.

use std::cmp::Ordering;
use std::env;
use std::fs;

use crate::util::{Result, cmd, no_more, output, run, take_flag, venv_bin};

/// major.minor.patch, optionally -beta.N. A beta comes before its release
/// (0.7.0-beta.2 < 0.7.0), as the app's updater orders them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    beta: Option<u64>,
    text: String,
}

impl Version {
    /// Parses a version; a leading v is optional.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, beta) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(number(pre.strip_prefix("beta.")?)?)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let (major, minor, patch) = (
            number(parts.next()?)?,
            number(parts.next()?)?,
            number(parts.next()?)?,
        );
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            beta,
            text: text.to_string(),
        })
    }

    fn key(&self) -> (u64, u64, u64, bool, u64) {
        (
            self.major,
            self.minor,
            self.patch,
            self.beta.is_none(),
            self.beta.unwrap_or(0),
        )
    }

    pub fn is_beta(&self) -> bool {
        self.beta.is_some()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_or_usage(text: &str, usage: &str) -> Result<Version> {
    Version::parse(text).ok_or_else(|| format!("Not a version: {text}\nUsage: {usage}"))
}

/// cargo xtask newest-version <version>...: prints the newest, ordered like semver.
pub fn newest(args: Vec<String>) -> Result {
    let usage = "cargo xtask newest-version <version>...";
    if args.is_empty() {
        return Err(format!("Usage: {usage}"));
    }
    let versions = args
        .iter()
        .map(|arg| parse_or_usage(arg, usage))
        .collect::<Result<Vec<_>>>()?;
    println!(
        "{}",
        versions.into_iter().max().expect("at least one version")
    );
    Ok(())
}

const CONF: &str = "tauri/src-tauri/tauri.conf.json";
const CARGO_TOML: &str = "tauri/src-tauri/Cargo.toml";
const CARGO_LOCK: &str = "tauri/src-tauri/Cargo.lock";
const PYPROJECT: &str = "backend/pyproject.toml";
const INIT_PY: &str = "backend/__init__.py";
/// Every file the version is written into, as release commits them.
pub const FILES: [&str; 5] = [CONF, CARGO_TOML, CARGO_LOCK, PYPROJECT, INIT_PY];

/// How each file writes its version: the first line starting `prefix`,
/// or with `after`, the first such line right after that line.
struct Spot {
    prefix: &'static str,
    after: Option<&'static str>,
}

fn spot(file: &str) -> Spot {
    match file {
        CONF => Spot {
            prefix: "  \"version\": \"",
            after: None,
        },
        CARGO_LOCK => Spot {
            prefix: "version = \"",
            after: Some("name = \"kass\""),
        },
        INIT_PY => Spot {
            prefix: "__version__ = \"",
            after: None,
        },
        _ => Spot {
            prefix: "version = \"",
            after: None,
        },
    }
}

fn find_line(text: &str, spot: &Spot) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    match spot.after {
        Some(after) => {
            let index = lines.iter().position(|line| *line == after)? + 1;
            lines
                .get(index)
                .filter(|line| line.starts_with(spot.prefix))
                .map(|_| index)
        }
        None => lines.iter().position(|line| line.starts_with(spot.prefix)),
    }
}

fn read_version(file: &str) -> Result<String> {
    let text =
        fs::read_to_string(file).map_err(|error| format!("Couldn't read {file}: {error}"))?;
    let spot = spot(file);
    let line = find_line(&text, &spot)
        .and_then(|index| text.lines().nth(index))
        .unwrap_or("");
    Ok(line
        .strip_prefix(spot.prefix)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("")
        .to_string())
}

fn write_version(file: &str, version: &Version) -> Result {
    let text =
        fs::read_to_string(file).map_err(|error| format!("Couldn't read {file}: {error}"))?;
    let spot = spot(file);
    let index = find_line(&text, &spot).ok_or_else(|| format!("No version in {file}"))?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let rest = lines[index][spot.prefix.len()..]
        .split_once('"')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default();
    lines[index] = format!("{}{version}\"{rest}", spot.prefix);
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    fs::write(file, out).map_err(|error| format!("Couldn't write {file}: {error}"))
}

/// Prints each file's version and returns them.
fn versions() -> Result<Vec<String>> {
    FILES
        .iter()
        .map(|file| {
            let version = read_version(file)?;
            println!("{file} {version}");
            Ok(version)
        })
        .collect()
}

/// cargo xtask set-version <version> | --check [version]
///
/// GitHub releases are the source of truth: release calls this before it
/// tags, and the release workflow runs --check against the tag.
pub fn set(mut args: Vec<String>) -> Result {
    let usage = "cargo xtask set-version <major.minor.patch[-beta.N]> | --check [version]";
    if take_flag(&mut args, "--check") {
        let expected = args
            .first()
            .map(|arg| arg.strip_prefix('v').unwrap_or(arg).to_string());
        no_more(args.get(1..).unwrap_or_default())?;
        let found = versions()?;
        let mut unique = found.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != 1 {
            return Err("Versions don't match. Run: cargo xtask set-version <version>".into());
        }
        if let Some(expected) = expected
            && unique[0] != expected
        {
            return Err(format!("Files say {}, expected {expected}.", unique[0]));
        }
        return Ok(());
    }
    let [version] = args.as_slice() else {
        return Err(format!("Usage: {usage}"));
    };
    let version = parse_or_usage(version, usage)?;
    write_all(&version)?;
    versions().map(|_| ())
}

fn write_all(version: &Version) -> Result {
    FILES
        .iter()
        .try_for_each(|file| write_version(file, version))
}

/// cargo xtask release <version> [--force]: set the version, commit, tag and push.
///
/// tb release cut runs this through scripts/release.sh with TB_RELEASE set and
/// records the release on the board when it succeeds. On its own it refuses
/// unless given --force. Pushing the tag runs .github/workflows/release.yml,
/// which builds the app and publishes the GitHub release with the DMG. A beta
/// is published as a prerelease that only beta-channel copies install.
pub fn release(mut args: Vec<String>) -> Result {
    let usage = "cargo xtask release [v]<major.minor.patch[-beta.N]> [--force]";
    let force = take_flag(&mut args, "--force");
    let given = args.first().cloned().unwrap_or_default();
    no_more(args.get(1..).unwrap_or_default())?;
    let shown = given.strip_prefix('v').unwrap_or(&given);
    if env::var_os("TB_RELEASE").is_none() && !force {
        let name = if shown.is_empty() { "<version>" } else { shown };
        return Err(format!(
            "Run tb release cut v{name} instead; it runs this and records the release. (--force to run it without the board.)"
        ));
    }
    let version = Version::parse(&given).ok_or_else(|| format!("Usage: {usage}"))?;
    let tag = format!("v{version}");

    let branch = output(&mut cmd("git", ["rev-parse", "--abbrev-ref", "HEAD"]))?;
    if branch != "main" {
        return Err(format!(
            "Release from main (on {branch}). Merge your branch into main first."
        ));
    }
    if !output(&mut cmd("git", ["status", "--porcelain"]))?.is_empty() {
        return Err("Commit or stash your changes first.".into());
    }
    // A shared cleanup adapter tested with other cleanup code is ignored by
    // the app (backend/services/shared_adapters.py): retrain it before releasing.
    run(&mut cmd(
        venv_bin("python"),
        ["scripts/shared-adapters/publish.py", "--check"],
    ))?;
    run(&mut cmd("git", ["fetch", "--tags", "origin"]))?;
    if !output(&mut cmd("git", ["tag", "--list", &tag]))?.is_empty() {
        return Err(format!("{tag} already exists."));
    }
    // A public release only has to be newer than the last public one: a
    // hotfix can go out while a newer beta is out, and beta copies stay on
    // the beta. A beta has to be newer than everything, or beta copies would
    // never get it.
    let tags = output(&mut cmd("git", ["tag", "--list", "v[0-9]*"]))?;
    let latest = tags
        .lines()
        .filter_map(Version::parse)
        .filter(|tagged| version.is_beta() || !tagged.is_beta())
        .max();
    if let Some(latest) = latest
        && latest >= version
    {
        return Err(format!("{tag} isn't newer than v{latest}."));
    }

    write_all(&version)?;
    versions()?;
    run(cmd("git", ["add"]).args(FILES))?;
    run(&mut cmd(
        "git",
        ["commit", "-m", &format!("chore: release {tag}")],
    ))?;
    run(&mut cmd(
        "git",
        ["tag", "-a", &tag, "-m", &format!("Kass {version}")],
    ))?;
    run(&mut cmd("git", ["push", "origin", &branch, &tag]))?;

    println!();
    println!("Pushed {tag}. The release workflow builds the app and publishes the release:");
    println!("  https://github.com/mrgnhnt96/kass/actions/workflows/release.yml");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn newest_of(versions: &[&str]) -> String {
        versions
            .iter()
            .filter_map(|text| Version::parse(text))
            .max()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_beta_comes_before_its_release() {
        assert_eq!(newest_of(&["0.7.0-beta.2", "0.6.2", "0.7.0"]), "0.7.0");
        assert_eq!(newest_of(&["0.7.0-beta.2", "0.6.2"]), "0.7.0-beta.2");
        assert_eq!(
            newest_of(&["0.7.0-beta.10", "0.7.0-beta.9"]),
            "0.7.0-beta.10"
        );
        assert_eq!(newest_of(&["0.10.0", "0.9.9"]), "0.10.0");
    }

    #[test]
    fn parses_only_release_and_beta_versions() {
        assert_eq!(Version::parse("v0.6.0").unwrap().to_string(), "0.6.0");
        for bad in [
            "0.6",
            "0.6.0.1",
            "0.6.0-rc.1",
            "0.6.0-beta",
            "a.b.c",
            "0.6.0-beta.x",
            "",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn writes_only_the_version_line() {
        let lock = "[[package]]\nname = \"other\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"kass\"\nversion = \"0.1.0\"\n";
        assert_eq!(find_line(lock, &spot(CARGO_LOCK)), Some(6));
        let conf = "{\n  \"app\": {\n    \"version\": \"x\"\n  },\n  \"version\": \"0.1.0\",\n}\n";
        assert_eq!(find_line(conf, &spot(CONF)), Some(4));
    }
}
