//! Fetch a model's files from Hugging Face into the cache, laid out as
//! huggingface_hub's `snapshot_download` leaves them: each file a blob named
//! by its hash (`blobs/<sha256 or git oid>`), linked from
//! `snapshots/<commit>/<path>`, with `refs/main` naming the commit. A blob
//! downloads to `<blob>.incomplete` first, and resumes from there.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::cache::repo_folder;
use super::registry::Model;

/// Where models come from: `HF_ENDPOINT`, as huggingface_hub reads it, or
/// huggingface.co.
#[derive(Clone)]
pub struct Hub {
    client: reqwest::Client,
    endpoint: String,
}

impl Hub {
    pub fn new(endpoint: Option<String>) -> Self {
        let endpoint = endpoint
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| "https://huggingface.co".into());
        let client = reqwest::Client::builder()
            .user_agent("kass")
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(std::env::var("HF_ENDPOINT").ok())
    }
}

#[derive(Deserialize)]
struct Revision {
    sha: String,
    siblings: Vec<Sibling>,
}

#[derive(Deserialize)]
struct Sibling {
    rfilename: String,
    #[serde(rename = "blobId")]
    blob_id: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
    sha256: String,
    size: u64,
}

/// A file to have in the snapshot.
struct File {
    path: String,
    blob: String,
    size: u64,
}

/// Bytes fetched so far of all the bytes to fetch, and the file it's on.
pub trait Report: Send + Sync {
    fn report(&self, current: u64, total: u64, filename: &str);
}

impl<F: Fn(u64, u64, &str) + Send + Sync> Report for F {
    fn report(&self, current: u64, total: u64, filename: &str) {
        self(current, total, filename)
    }
}

/// Fetch every file `model`'s loader reads that isn't in `cache` yet.
pub async fn fetch(
    hub: &Hub,
    cache: &Path,
    model: &Model,
    report: &dyn Report,
) -> Result<(), String> {
    let url = format!(
        "{}/api/models/{}/revision/main?blobs=true",
        hub.endpoint, model.repo
    );
    let revision: Revision = hub
        .client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("Could not reach Hugging Face for {}: {error}", model.repo))?
        .json()
        .await
        .map_err(|error| {
            format!(
                "Hugging Face's file list for {} didn't read: {error}",
                model.repo
            )
        })?;
    let files: Vec<File> = revision
        .siblings
        .into_iter()
        .filter(|sibling| model.wants(&sibling.rfilename))
        .map(|sibling| match sibling.lfs {
            Some(lfs) => File {
                path: sibling.rfilename,
                blob: lfs.sha256,
                size: lfs.size,
            },
            None => File {
                path: sibling.rfilename,
                blob: sibling.blob_id,
                size: sibling.size.unwrap_or(0),
            },
        })
        .collect();
    if files
        .iter()
        .any(|file| !safe_path(&file.path) || !safe_name(&file.blob))
        || !safe_name(&revision.sha)
    {
        return Err(format!(
            "Hugging Face listed an unsafe path for {}",
            model.repo
        ));
    }

    let folder = cache.join(repo_folder(model.repo));
    let blobs = folder.join("blobs");
    let snapshot = folder.join("snapshots").join(&revision.sha);
    tokio::fs::create_dir_all(&blobs)
        .await
        .map_err(|error| format!("Could not create {}: {error}", blobs.display()))?;

    let missing: Vec<&File> = files
        .iter()
        .filter(|file| !blobs.join(&file.blob).is_file())
        .collect();
    let total: u64 = missing.iter().map(|file| file.size).sum();
    let mut done: u64 = missing
        .iter()
        .map(|file| partial_len(&blobs, file).min(file.size))
        .sum();
    for file in missing {
        let url = format!(
            "{}/{}/resolve/{}/{}",
            hub.endpoint, model.repo, revision.sha, file.path
        );
        let name = file_name(&file.path);
        report.report(done, total, name);
        done = fetch_blob(hub, &url, &blobs, file, done, total, name, report).await?;
    }

    for file in &files {
        link(&snapshot, file).await?;
    }
    let refs = folder.join("refs");
    tokio::fs::create_dir_all(&refs)
        .await
        .and(tokio::fs::write(refs.join("main"), &revision.sha).await)
        .map_err(|error| format!("Could not record {}'s revision: {error}", model.repo))?;
    Ok(())
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn partial(blobs: &Path, file: &File) -> PathBuf {
    blobs.join(format!("{}.incomplete", file.blob))
}

fn partial_len(blobs: &Path, file: &File) -> u64 {
    std::fs::metadata(partial(blobs, file))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Download one blob, resuming a partial one; returns `done` plus what this
/// blob added.
#[allow(clippy::too_many_arguments)]
async fn fetch_blob(
    hub: &Hub,
    url: &str,
    blobs: &Path,
    file: &File,
    mut done: u64,
    total: u64,
    name: &str,
    report: &dyn Report,
) -> Result<u64, String> {
    let partial = partial(blobs, file);
    let mut have = partial_len(blobs, file);
    if have > file.size {
        have = 0;
    }
    let failed =
        |error: &dyn std::fmt::Display| format!("Downloading {} failed: {error}", file.path);
    let mut request = hub.client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let response = request.send().await.map_err(|e| failed(&e))?;
    let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let response = response.error_for_status().map_err(|e| failed(&e))?;
    if !resumed {
        // The server sent it whole: start the count for this file again.
        done -= have.min(done);
        have = 0;
    }
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&partial)
        .await
        .map_err(|e| failed(&e))?;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| failed(&e))?;
        out.write_all(&chunk).await.map_err(|e| failed(&e))?;
        have += chunk.len() as u64;
        done += chunk.len() as u64;
        report.report(done, total, name);
    }
    out.flush().await.map_err(|e| failed(&e))?;
    drop(out);
    if file.size > 0 && have != file.size {
        return Err(failed(&format!("got {have} of {} bytes", file.size)));
    }
    tokio::fs::rename(&partial, blobs.join(&file.blob))
        .await
        .map_err(|e| failed(&e))?;
    Ok(done)
}

/// `snapshots/<commit>/<path>` → `../../blobs/<blob>`, relative as
/// huggingface_hub links it so a moved cache still resolves.
async fn link(snapshot: &Path, file: &File) -> Result<(), String> {
    let at = snapshot.join(&file.path);
    if let Some(parent) = at.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    let depth = file.path.matches('/').count() + 2;
    let target = format!("{}blobs/{}", "../".repeat(depth), file.blob);
    if tokio::fs::symlink_metadata(&at).await.is_ok() {
        let _ = tokio::fs::remove_file(&at).await;
    }
    tokio::fs::symlink(&target, &at)
        .await
        .map_err(|error| format!("Could not link {}: {error}", at.display()))
}

/// A repository path that stays inside the snapshot.
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use axum::extract::Path as UrlPath;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;

    const WEIGHTS: &[u8] = b"0123456789abcdefghij";
    const WEIGHTS_SHA: &str = "aaaabbbbccccdddd";

    async fn listing() -> Response {
        axum::Json(serde_json::json!({
            "sha": "c0ffee",
            "siblings": [
                { "rfilename": "config.json", "blobId": "cf01", "size": 2 },
                { "rfilename": "README.md", "blobId": "rd01", "size": 3 },
                { "rfilename": "model.safetensors", "blobId": "b33b", "size": 134,
                  "lfs": { "sha256": WEIGHTS_SHA, "size": WEIGHTS.len(), "pointerSize": 134 } },
                { "rfilename": "pytorch_model.bin", "blobId": "pt01", "size": 134,
                  "lfs": { "sha256": "ffff", "size": 99, "pointerSize": 134 } },
            ]
        }))
        .into_response()
    }

    async fn resolve(
        UrlPath((_, _, file)): UrlPath<(String, String, String)>,
        headers: HeaderMap,
    ) -> Response {
        let body: &[u8] = match file.as_str() {
            "config.json" => b"{}",
            "model.safetensors" => WEIGHTS,
            _ => return StatusCode::NOT_FOUND.into_response(),
        };
        let from = headers
            .get("range")
            .and_then(|range| range.to_str().ok())
            .and_then(|range| range.strip_prefix("bytes="))
            .and_then(|range| range.trim_end_matches('-').parse::<usize>().ok());
        match from {
            Some(from) => (StatusCode::PARTIAL_CONTENT, body[from..].to_vec()).into_response(),
            None => body.to_vec().into_response(),
        }
    }

    async fn fake_hub() -> String {
        let app = Router::new()
            .route("/api/models/{org}/{name}/revision/main", get(listing))
            .route("/{org}/{name}/resolve/c0ffee/{*file}", get(resolve));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-download-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn fetches_the_files_the_loader_reads_into_the_cache_layout() {
        let hub = Hub::new(Some(fake_hub().await));
        let cache = temp();
        let model = super::super::registry::find("whisper-base").unwrap();
        let seen = Mutex::new(Vec::new());
        let report = |current: u64, total: u64, name: &str| {
            seen.lock()
                .unwrap()
                .push((current, total, name.to_string()))
        };
        fetch(&hub, &cache, model, &report).await.unwrap();

        let folder = cache.join("models--openai--whisper-base");
        let snapshot = folder.join("snapshots/c0ffee");
        assert_eq!(
            std::fs::read(snapshot.join("model.safetensors")).unwrap(),
            WEIGHTS
        );
        assert_eq!(std::fs::read(snapshot.join("config.json")).unwrap(), b"{}");
        assert_eq!(
            std::fs::read_link(snapshot.join("model.safetensors")).unwrap(),
            PathBuf::from(format!("../../blobs/{WEIGHTS_SHA}"))
        );
        assert!(!snapshot.join("pytorch_model.bin").exists());
        assert!(!snapshot.join("README.md").exists());
        assert_eq!(
            std::fs::read_to_string(folder.join("refs/main")).unwrap(),
            "c0ffee"
        );
        assert_eq!(seen.lock().unwrap().last().unwrap().0, 22);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|(_, total, _)| *total == 22));
        assert!(super::super::cache::on_disk(&cache, model.repo).downloaded);
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[tokio::test]
    async fn resumes_a_partial_blob() {
        let hub = Hub::new(Some(fake_hub().await));
        let cache = temp();
        let blobs = cache.join("models--openai--whisper-base/blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(
            blobs.join(format!("{WEIGHTS_SHA}.incomplete")),
            &WEIGHTS[..7],
        )
        .unwrap();
        let model = super::super::registry::find("whisper-base").unwrap();
        let first = Mutex::new(None);
        let report = |current: u64, _: u64, _: &str| {
            first.lock().unwrap().get_or_insert(current);
        };
        fetch(&hub, &cache, model, &report).await.unwrap();
        assert_eq!(*first.lock().unwrap(), Some(7));
        assert_eq!(std::fs::read(blobs.join(WEIGHTS_SHA)).unwrap(), WEIGHTS);
        assert!(!blobs.join(format!("{WEIGHTS_SHA}.incomplete")).exists());
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn refuses_paths_that_leave_the_snapshot() {
        assert!(safe_path("config.json"));
        assert!(safe_path("sub/model.safetensors"));
        assert!(!safe_path("../config.json"));
        assert!(!safe_path("/etc/passwd"));
        assert!(!safe_name("../x"));
    }
}
