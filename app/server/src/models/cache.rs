//! The Hugging Face cache models download to, laid out as huggingface_hub
//! lays it out so the worker's loaders find them:
//! `models--<org>--<name>/{blobs,snapshots/<commit>,refs/main}`.

use std::collections::HashSet;
use std::ffi::CString;
use std::path::{Path, PathBuf};

/// Weights in any of these count as a downloaded model (`/models/status`).
const WEIGHTS: [&str; 5] = [".safetensors", ".bin", ".pt", ".pth", ".npz"];

/// huggingface_hub's `HF_HUB_CACHE`: the folder the app chose
/// (`KASS_MODELS_DIR` for the worker), else the environment's, else
/// `~/.cache/huggingface/hub`.
pub fn resolve(chosen: Option<&Path>) -> PathBuf {
    if let Some(dir) = chosen {
        return dir.to_path_buf();
    }
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if let Some(dir) = var("HF_HUB_CACHE").or_else(|| var("HUGGINGFACE_HUB_CACHE")) {
        return expand_home(&dir);
    }
    let home = var("HF_HOME")
        .map(|dir| expand_home(&dir))
        .unwrap_or_else(|| {
            var("XDG_CACHE_HOME")
                .map(|dir| expand_home(&dir))
                .unwrap_or_else(|| expand_home("~/.cache"))
                .join("huggingface")
        });
    home.join("hub")
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(format!("{home}{rest}"))
        }
        _ => PathBuf::from(path),
    }
}

/// `models--openai--whisper-base` for `openai/whisper-base`.
pub fn repo_folder(repo: &str) -> String {
    format!("models--{}", repo.replace('/', "--"))
}

/// What's on disk for one repository.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnDisk {
    pub downloaded: bool,
    pub size_mb: Option<f64>,
}

/// Downloaded once a snapshot holds weights and no blob is still
/// `.incomplete`; the size is what its snapshots point to, each revision
/// counted as huggingface_hub's `scan_cache_dir` counts it.
pub fn on_disk(cache: &Path, repo: &str) -> OnDisk {
    let folder = cache.join(repo_folder(repo));
    let missing = OnDisk {
        downloaded: false,
        size_mb: None,
    };
    if has_incomplete(&folder.join("blobs")) {
        return missing;
    }
    let Ok(revisions) = std::fs::read_dir(folder.join("snapshots")) else {
        return missing;
    };
    let mut has_weights = false;
    let mut bytes = 0u64;
    for revision in revisions.flatten() {
        let mut blobs = HashSet::new();
        for file in files_in(&revision.path()) {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            has_weights |= WEIGHTS.iter().any(|ext| name.ends_with(ext));
            let target = std::fs::canonicalize(&file).unwrap_or(file);
            if blobs.insert(target.clone()) {
                bytes += std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    if !has_weights {
        return missing;
    }
    OnDisk {
        downloaded: true,
        size_mb: Some(bytes as f64 / (1024.0 * 1024.0)),
    }
}

/// Weights the readiness checklist counts (`is_model_cached`'s default).
pub const READY_WEIGHTS: &[&str] = &[".safetensors", ".bin"];

/// Weights `/health` counts for the chosen Whisper model.
pub const WHISPER_WEIGHTS: &[&str] = &[".safetensors", ".bin", ".npz"];

/// Whether `repo` is downloaded as `backend/backends/base.py`'s
/// `is_model_cached` decides it: no blob still `.incomplete`, and some
/// name under `snapshots` ending in one of `weights`. Only names count, so
/// a dangling link counts as Python's `rglob` counts it.
pub fn is_cached(cache: &Path, repo: &str, weights: &[&str]) -> bool {
    let folder = cache.join(repo_folder(repo));
    if has_incomplete(&folder.join("blobs")) {
        return false;
    }
    let mut pending = vec![folder.join("snapshots")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if weights.iter().any(|ext| name.ends_with(ext)) {
                return true;
            }
            // `rglob` doesn't follow links to folders.
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
    false
}

fn has_incomplete(blobs: &Path) -> bool {
    std::fs::read_dir(blobs).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".incomplete"))
    })
}

/// Every file under `dir`, links followed, as `Path.rglob` finds them.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => pending.push(path),
                Ok(meta) if meta.is_file() => found.push(path),
                _ => {}
            }
        }
    }
    found
}

/// Bytes in every file under `dir`, without following links (Python's
/// `_get_dir_size` stats each file `rglob` finds; snapshot links point into
/// `blobs`, already counted).
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Free MB on `path`'s volume, measured at its nearest existing parent.
pub fn free_mb_at(path: &Path) -> u64 {
    let mut existing = path;
    while !existing.exists() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => break,
        }
    }
    free_bytes(existing).unwrap_or(0) / (1024 * 1024)
}

fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid C string and `stats` a writable statvfs.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    Some(stats.f_bavail as u64 * stats.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-cache-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn snapshot(cache: &Path, repo: &str, files: &[(&str, usize)]) {
        let folder = cache.join(repo_folder(repo));
        let snap = folder.join("snapshots/abc");
        std::fs::create_dir_all(folder.join("blobs")).unwrap();
        std::fs::create_dir_all(&snap).unwrap();
        for (i, (name, size)) in files.iter().enumerate() {
            let blob = folder.join(format!("blobs/blob{i}"));
            std::fs::write(&blob, vec![0u8; *size]).unwrap();
            std::os::unix::fs::symlink(format!("../../blobs/blob{i}"), snap.join(name)).unwrap();
        }
    }

    #[test]
    fn cached_needs_weights_of_the_kinds_asked_for() {
        let cache = temp("is-cached");
        assert!(!is_cached(&cache, "openai/whisper-base", READY_WEIGHTS));
        snapshot(&cache, "openai/whisper-base", &[("weights.npz", 8)]);
        assert!(!is_cached(&cache, "openai/whisper-base", READY_WEIGHTS));
        assert!(is_cached(&cache, "openai/whisper-base", WHISPER_WEIGHTS));
        let nested = cache
            .join(repo_folder("openai/whisper-small"))
            .join("snapshots/abc/sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("model.safetensors"), b"").unwrap();
        assert!(is_cached(&cache, "openai/whisper-small", READY_WEIGHTS));
    }

    #[test]
    fn a_download_still_running_is_not_cached() {
        let cache = temp("incomplete");
        snapshot(&cache, "openai/whisper-base", &[("model.safetensors", 8)]);
        let blobs = cache.join(repo_folder("openai/whisper-base")).join("blobs");
        std::fs::write(blobs.join("x.incomplete"), b"").unwrap();
        assert!(!is_cached(&cache, "openai/whisper-base", READY_WEIGHTS));
    }

    #[test]
    fn a_repo_with_weights_is_downloaded_with_its_size() {
        let cache = temp("weights");
        snapshot(
            &cache,
            "openai/whisper-base",
            &[
                ("model.safetensors", 1024 * 1024),
                ("config.json", 1024 * 1024),
            ],
        );
        assert_eq!(
            on_disk(&cache, "openai/whisper-base"),
            OnDisk {
                downloaded: true,
                size_mb: Some(2.0)
            }
        );
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn config_alone_or_an_unfinished_blob_is_not_downloaded() {
        let cache = temp("partial");
        snapshot(&cache, "a/b", &[("config.json", 10)]);
        assert!(!on_disk(&cache, "a/b").downloaded);
        snapshot(&cache, "c/d", &[("model.safetensors", 10)]);
        std::fs::write(cache.join("models--c--d/blobs/x.incomplete"), b"").unwrap();
        assert!(!on_disk(&cache, "c/d").downloaded);
        assert!(!on_disk(&cache, "never/fetched").downloaded);
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn the_chosen_folder_wins() {
        assert_eq!(
            resolve(Some(Path::new("/Volumes/x"))),
            PathBuf::from("/Volumes/x")
        );
        assert_eq!(
            repo_folder("mlx-community/Qwen3-4B-4bit"),
            "models--mlx-community--Qwen3-4B-4bit"
        );
    }

    #[test]
    fn free_space_is_measured_at_the_nearest_folder_that_exists() {
        let cache = temp("free");
        assert!(free_mb_at(&cache.join("not/yet/made")) > 0);
        std::fs::remove_dir_all(cache).unwrap();
    }
}
