//! What dictation reads, copied from a shared Kass to a stand-in, so
//! dictating on the stand-in sounds the same: settings, styles, the
//! dictionary, what was learned from corrections and the trained models in
//! use (`backend/services/sync/mirror.py` at b340f4f7).
//!
//! The shared Kass serves a [`snapshot`]; the stand-in replaces its own
//! copy with it ([`replace`]), never recording that as a change of its own.
//! A stand-in only copies once everything it changed has been sent, so
//! nothing it did is lost underneath the copy.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::rows::{self, MIRRORED, OWN_SETTINGS};

/// What was learned from corrections, kept in the data dir.
pub const LEARNED_FILES: [&str; 3] = [
    "writing-style.json",
    "correction-notes.json",
    "correction-learning.json",
];

/// Where a stand-in keeps the shared Kass's trained models: the worker's
/// `manager.mirrored_root()`.
pub fn mirrored_root(data_dir: &Path) -> PathBuf {
    data_dir.join("model-improvement").join("mirrored")
}

/// A name that's one plain file or folder name, nothing that leads elsewhere.
pub fn plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(part)), None) if part == name
    )
}

/// The files of a trained model's folder, by name.
fn files_in(folder: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// The trained models in use (the worker's `manager.in_use()`), each with
/// its files instead of where it is on this Mac. One whose folder can't be
/// read is left out.
pub fn in_use_adapters(in_use: &Map<String, Value>) -> Map<String, Value> {
    let mut adapters = Map::new();
    for (kind, item) in in_use {
        let Some(path) = item.get("path").and_then(Value::as_str) else {
            adapters.insert(kind.clone(), item.clone());
            continue;
        };
        let Ok(files) = files_in(Path::new(path)) else {
            continue;
        };
        let mut item = item.as_object().cloned().unwrap_or_default();
        item.remove("path");
        item.insert("files".into(), files.into());
        adapters.insert(kind.clone(), Value::Object(item));
    }
    adapters
}

/// One file of the trained model in use for `kind`, for a stand-in to copy.
pub fn adapter_file(in_use: &Map<String, Value>, kind: &str, name: &str) -> Option<PathBuf> {
    let folder = Path::new(in_use.get(kind)?.get("path")?.as_str()?);
    files_in(folder)
        .ok()?
        .iter()
        .any(|file| file == name)
        .then(|| folder.join(name))
}

fn read_learned(data_dir: &Path, name: &str) -> Value {
    std::fs::read(data_dir.join(name))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

/// The rows a snapshot copies: this Mac's settings but its own, and every
/// mirrored table.
pub fn snapshot_rows(conn: &Connection) -> rusqlite::Result<Map<String, Value>> {
    let mut data = Map::new();
    let settings = rows::get(conn, "capture_settings", "1")?.map(|mut row| {
        row.retain(|key, _| !OWN_SETTINGS.contains(&key.as_str()));
        Value::Object(row)
    });
    data.insert("settings".into(), settings.unwrap_or(Value::Null));
    for (name, table) in MIRRORED {
        let rows = rows::all(conn, table)?.into_iter().map(Value::Object);
        data.insert(name.into(), Value::Array(rows.collect()));
    }
    Ok(data)
}

/// Everything a stand-in copies, with a revision that changes when any of
/// it does.
pub fn snapshot(
    mut data: Map<String, Value>,
    data_dir: &Path,
    adapters: Map<String, Value>,
) -> Map<String, Value> {
    let files: Map<String, Value> = LEARNED_FILES
        .iter()
        .map(|name| (name.to_string(), read_learned(data_dir, name)))
        .collect();
    data.insert("files".into(), Value::Object(files));
    data.insert("adapters".into(), Value::Object(adapters));
    // serde_json's maps keep their keys sorted, so the same data hashes the same.
    let revision = Sha256::digest(Value::Object(data.clone()).to_string().as_bytes());
    let revision: String = revision.iter().map(|byte| format!("{byte:02x}")).collect();
    data.insert("revision".into(), revision.into());
    data
}

/// Replace this Mac's settings (but its own) and mirrored tables with the
/// snapshot's, recording nothing to send back.
pub fn replace(conn: &mut Connection, data: &Map<String, Value>) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(settings) = data.get("settings").and_then(Value::as_object) {
        rows::update(&tx, "capture_settings", "1", settings, &OWN_SETTINGS)?;
    }
    for (_, table) in MIRRORED.iter().rev() {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    for (name, table) in MIRRORED {
        for row in data
            .get(name)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(row) = row.as_object() {
                rows::insert(&tx, table, row, &[])?;
            }
        }
    }
    tx.commit()
}

/// Write the learned files as the snapshot has them; one it hasn't goes.
pub fn write_learned(data_dir: &Path, files: &Map<String, Value>) -> std::io::Result<()> {
    for name in LEARNED_FILES {
        let path = data_dir.join(name);
        match files.get(name) {
            None | Some(Value::Null) => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            },
            Some(content) => {
                let temporary = path.with_extension("mirror.tmp");
                let text = serde_json::to_string_pretty(content).map_err(std::io::Error::other)?;
                std::fs::write(&temporary, text)?;
                std::fs::rename(&temporary, &path)?;
            }
        }
    }
    Ok(())
}

/// A trained model to copy: its kind, its id and its files.
pub type ToCopy = (String, String, Vec<String>);

/// The trained models a snapshot names that have files to copy: (kind, the
/// model's id, its files). One with a name that isn't a plain file name is
/// refused.
pub fn adapters_to_copy(adapters: &Map<String, Value>) -> Vec<Result<ToCopy, String>> {
    adapters
        .iter()
        .filter_map(|(kind, item)| {
            let files = item.get("files")?.as_array()?;
            let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let files: Vec<String> = files
                .iter()
                .map(|file| file.as_str().unwrap_or_default().to_string())
                .collect();
            Some(
                match std::iter::once(id)
                    .chain(std::iter::once(kind.as_str()))
                    .chain(files.iter().map(String::as_str))
                    .find(|name| !plain_name(name))
                {
                    Some(name) => Err(format!("Not a file name: {name:?}")),
                    None => Ok((kind.clone(), id.to_string(), files)),
                },
            )
        })
        .collect()
}

/// The snapshot's trained models as this Mac uses them: those copied here
/// point at their folder, those with nothing to copy stay as they are.
pub fn active(adapters: &Map<String, Value>, root: &Path, copied: &[String]) -> Map<String, Value> {
    let mut active = Map::new();
    for (kind, item) in adapters {
        if item.get("files").is_none() {
            active.insert(kind.clone(), item.clone());
            continue;
        }
        if !copied.contains(kind) {
            continue;
        }
        let mut item = item.as_object().cloned().unwrap_or_default();
        item.remove("files");
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        let folder = root.join(id).join(kind);
        item.insert("path".into(), folder.to_string_lossy().into_owned().into());
        active.insert(kind.clone(), Value::Object(item));
    }
    active
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_names_lead_nowhere_else() {
        for name in ["adapters.safetensors", "run-1", "a.b"] {
            assert!(plain_name(name), "{name}");
        }
        for name in ["", ".", "..", "../x", "a/b", "/etc", "./a"] {
            assert!(!plain_name(name), "{name}");
        }
    }

    #[test]
    fn adapters_are_sent_as_their_files_and_copied_to_their_own_folder() {
        let dir = std::env::temp_dir().join(format!("kass-mirror-{}", uuid::Uuid::new_v4()));
        let folder = dir.join("run");
        std::fs::create_dir_all(folder.join("nested")).unwrap();
        std::fs::write(folder.join("b.json"), "{}").unwrap();
        std::fs::write(folder.join("a.safetensors"), "w").unwrap();
        let in_use = json!({
            "llm": {"id": "r1", "sha256": "x", "path": folder.to_string_lossy()},
            "speech": {"model": "turbo", "configured": "turbo"},
            "voice": {"id": "gone", "path": dir.join("missing").to_string_lossy()},
        });
        let in_use = in_use.as_object().unwrap();
        let adapters = in_use_adapters(in_use);
        assert_eq!(
            Value::Object(adapters.clone()),
            json!({
                "llm": {"id": "r1", "sha256": "x", "files": ["a.safetensors", "b.json"]},
                "speech": {"model": "turbo", "configured": "turbo"},
            })
        );
        assert_eq!(
            adapter_file(in_use, "llm", "b.json"),
            Some(folder.join("b.json"))
        );
        assert_eq!(adapter_file(in_use, "llm", "nested"), None);
        assert_eq!(adapter_file(in_use, "llm", "../run/b.json"), None);

        let to_copy = adapters_to_copy(&adapters);
        assert_eq!(
            to_copy,
            vec![Ok((
                "llm".into(),
                "r1".into(),
                vec!["a.safetensors".into(), "b.json".into()]
            ))]
        );
        let root = Path::new("/data/model-improvement/mirrored");
        assert_eq!(
            Value::Object(active(&adapters, root, &["llm".into()])),
            json!({
                "llm": {"id": "r1", "sha256": "x", "path": "/data/model-improvement/mirrored/r1/llm"},
                "speech": {"model": "turbo", "configured": "turbo"},
            })
        );
        // One that couldn't be copied is left out: dictation uses the base model.
        assert_eq!(
            Value::Object(active(&adapters, root, &[])),
            json!({"speech": {"model": "turbo", "configured": "turbo"}})
        );
        let evil = json!({"llm": {"id": "..", "files": ["a"]}});
        assert!(adapters_to_copy(evil.as_object().unwrap())[0].is_err());
        let evil = json!({"llm": {"id": "r", "files": ["../../x"]}});
        assert!(adapters_to_copy(evil.as_object().unwrap())[0].is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn learned_files_are_written_and_removed_as_the_snapshot_has_them() {
        let dir = std::env::temp_dir().join(format!("kass-learned-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("correction-notes.json"), "{}").unwrap();
        let files = json!({"writing-style.json": {"a": [1]}, "correction-notes.json": null});
        write_learned(&dir, files.as_object().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("writing-style.json")).unwrap(),
            "{\n  \"a\": [\n    1\n  ]\n}"
        );
        assert!(!dir.join("correction-notes.json").exists());
        let snapshot = snapshot(Map::new(), &dir, Map::new());
        assert_eq!(snapshot["files"]["writing-style.json"], json!({"a": [1]}));
        assert_eq!(snapshot["files"]["correction-learning.json"], Value::Null);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
