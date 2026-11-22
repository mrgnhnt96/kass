//! Moving the downloaded models to another folder (`POST /models/migrate`),
//! with progress under the name `migration`: a rename per model on the same
//! volume, counted in models, or a copy counted in bytes across volumes.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use super::progress::Progress;

pub const NAME: &str = "migration";

#[derive(Debug, PartialEq)]
pub enum Refused {
    /// The current folder isn't there.
    Missing,
    /// The destination can't take the models: why.
    Invalid(&'static str),
}

/// Models ready to move.
pub struct Move {
    pub source: PathBuf,
    pub destination: PathBuf,
    models: Vec<PathBuf>,
    same_volume: bool,
}

/// Check the move and list what to move; `Ok(None)` when there's nothing.
pub fn prepare(source: &Path, destination: &Path) -> Result<Option<Move>, Refused> {
    if !source.exists() {
        return Err(Refused::Missing);
    }
    let (from, to) = (resolve(source), resolve(destination));
    if from == to {
        return Err(Refused::Invalid(
            "Source and destination are the same directory",
        ));
    }
    if to.starts_with(&from) {
        return Err(Refused::Invalid(
            "Destination cannot be inside the current cache directory",
        ));
    }
    let mut models: Vec<PathBuf> = std::fs::read_dir(source)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("models--"))
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    if models.is_empty() {
        return Ok(None);
    }
    Ok(Some(Move {
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        models,
        same_volume: false,
    }))
}

impl Move {
    /// Make the destination folder, before the move starts in the background.
    pub fn make_destination(&mut self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.destination)?;
        self.same_volume = match (
            std::fs::metadata(&self.source),
            std::fs::metadata(&self.destination),
        ) {
            (Ok(from), Ok(to)) => from.dev() == to.dev(),
            _ => false,
        };
        Ok(())
    }

    /// Move every model, reporting progress; blocking. A model that fails is
    /// listed in the errors and the rest still move.
    pub fn run(self, progress: &Progress) -> (usize, Vec<String>) {
        let mut moved = 0;
        let mut errors = Vec::new();
        let total_models = self.models.len() as u64;
        let total_bytes: u64 = if self.same_volume {
            0
        } else {
            let total = self
                .models
                .iter()
                .map(|dir| super::cache::dir_size(dir))
                .sum();
            progress.update(NAME, 0, total, Some("Calculating..."), "downloading");
            total
        };
        let mut copied = 0u64;
        for (i, model) in self.models.iter().enumerate() {
            let name = model
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let target = self.destination.join(&name);
            let result = (|| {
                if std::fs::symlink_metadata(&target).is_ok() {
                    std::fs::remove_dir_all(&target)?;
                }
                if self.same_volume {
                    std::fs::rename(model, &target)
                } else {
                    copy_tree(model, &target, &mut |bytes, file| {
                        copied += bytes;
                        progress.update(NAME, copied, total_bytes, Some(file), "downloading");
                    })?;
                    std::fs::remove_dir_all(model)
                }
            })();
            match result {
                Ok(()) => {
                    moved += 1;
                    if self.same_volume {
                        progress.update(
                            NAME,
                            i as u64 + 1,
                            total_models,
                            Some(&name),
                            "downloading",
                        );
                    }
                }
                Err(error) => errors.push(format!("{name}: {error}")),
            }
        }
        (moved, errors)
    }
}

/// Copy a folder, links as links (snapshots link into `blobs`), reporting
/// bytes as they're written.
fn copy_tree(from: &Path, to: &Path, copied: &mut dyn FnMut(u64, &str)) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, copied)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
        } else {
            let name = entry.file_name().to_string_lossy().into_owned();
            let mut input = std::fs::File::open(entry.path())?;
            let mut output = std::fs::File::create(&target)?;
            let mut buffer = vec![0u8; 8 * 1024 * 1024];
            loop {
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                output.write_all(&buffer[..n])?;
                copied(n as u64, &name);
            }
            output.set_permissions(entry.metadata()?.permissions())?;
        }
    }
    Ok(())
}

/// `Path.resolve()`: absolute, links resolved as far as the path exists.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut existing = absolute.clone();
    let mut rest = Vec::new();
    while std::fs::canonicalize(&existing).is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut resolved = std::fs::canonicalize(&existing).unwrap_or(existing);
    for part in rest.into_iter().rev() {
        resolved.push(part);
    }
    // Drop `.` and fold `..` left in the part that doesn't exist yet.
    let mut clean = PathBuf::new();
    for component in resolved.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn model(cache: &Path, name: &str) {
        let folder = cache.join(name);
        std::fs::create_dir_all(folder.join("blobs")).unwrap();
        std::fs::create_dir_all(folder.join("snapshots/abc")).unwrap();
        std::fs::write(folder.join("blobs/b1"), b"weights").unwrap();
        std::os::unix::fs::symlink(
            "../../blobs/b1",
            folder.join("snapshots/abc/model.safetensors"),
        )
        .unwrap();
    }

    #[test]
    fn refuses_the_same_folder_or_one_inside_it() {
        let source = temp();
        assert_eq!(
            prepare(&source, &source.join(".")).err(),
            Some(Refused::Invalid(
                "Source and destination are the same directory"
            ))
        );
        assert_eq!(
            prepare(&source, &source.join("inner/models")).err(),
            Some(Refused::Invalid(
                "Destination cannot be inside the current cache directory"
            ))
        );
        assert_eq!(
            prepare(&source.join("gone"), &source).err(),
            Some(Refused::Missing)
        );
        assert!(prepare(&source, &temp()).unwrap().is_none());
    }

    #[test]
    fn copies_across_volumes_keeping_links() {
        let (source, destination) = (temp(), temp().join("new"));
        model(&source, "models--a--b");
        std::fs::create_dir_all(source.join("not-a-model")).unwrap();
        let mut plan = prepare(&source, &destination).unwrap().unwrap();
        plan.make_destination().unwrap();
        plan.same_volume = false;
        let progress = Progress::default();
        assert_eq!(plan.run(&progress), (1, vec![]));
        let link = destination.join("models--a--b/snapshots/abc/model.safetensors");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../../blobs/b1")
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"weights");
        assert!(!source.join("models--a--b").exists());
        assert!(source.join("not-a-model").exists());
        assert_eq!(progress.get(NAME).unwrap().current, 7);
    }

    #[test]
    fn renames_on_the_same_volume() {
        let source = temp();
        let destination = source
            .parent()
            .unwrap()
            .join(format!("kass-migrate-to-{}", uuid::Uuid::new_v4()));
        model(&source, "models--a--b");
        model(&source, "models--c--d");
        let mut plan = prepare(&source, &destination).unwrap().unwrap();
        plan.make_destination().unwrap();
        assert!(plan.same_volume);
        let progress = Progress::default();
        assert_eq!(plan.run(&progress), (2, vec![]));
        assert!(destination.join("models--c--d/blobs/b1").is_file());
        let update = progress.get(NAME).unwrap();
        assert_eq!((update.current, update.total), (2, 2));
        std::fs::remove_dir_all(destination).unwrap();
    }
}
