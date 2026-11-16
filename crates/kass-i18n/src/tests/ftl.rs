//! The embedded `.ftl` files are well-formed and complete.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use fluent_bundle::FluentResource;
use fluent_syntax::ast::Entry;

use super::message_ids;
use crate::bundle::LOCALES;

fn locales_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("locales")
}

fn ftl_stems(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ftl"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn every_ftl_file_parses_without_errors() {
    for locale in LOCALES {
        for (name, source) in locale.files {
            if let Err((resource, errors)) = FluentResource::try_new((*source).to_owned()) {
                let junk: Vec<_> = resource
                    .entries()
                    .filter_map(|e| match e {
                        Entry::Junk { content } => Some(*content),
                        _ => None,
                    })
                    .collect();
                panic!("{}/{name}.ftl: {errors:?}\njunk: {junk:#?}", locale.tag);
            }
        }
    }
}

#[test]
fn no_duplicate_ids_across_files() {
    for locale in LOCALES {
        let mut seen: HashMap<String, &str> = HashMap::new();
        for (name, source) in locale.files {
            for id in message_ids(source) {
                if let Some(other) = seen.insert(id.clone(), name) {
                    panic!(
                        "{}: `{id}` is defined in both {other}.ftl and {name}.ftl",
                        locale.tag
                    );
                }
            }
        }
    }
}

#[test]
fn every_locale_folder_and_file_is_embedded() {
    let folders: BTreeSet<String> = std::fs::read_dir(locales_dir())
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let embedded: BTreeSet<String> = LOCALES.iter().map(|l| l.tag.to_owned()).collect();
    assert_eq!(
        folders, embedded,
        "every folder under locales/ needs a locale!() line in bundle.rs"
    );

    for locale in LOCALES {
        let on_disk = ftl_stems(&locales_dir().join(locale.tag));
        let listed: BTreeSet<String> = locale.files.iter().map(|(n, _)| (*n).to_owned()).collect();
        assert_eq!(
            on_disk, listed,
            "{}: every .ftl file needs a section!() line in bundle.rs",
            locale.tag
        );
    }
}

#[test]
fn no_unicode_isolation_marks() {
    let text = crate::tr!("models-delete-dialog-body", name = "Whisper");
    assert!(!text.contains(['\u{2068}', '\u{2069}']), "{text:?}");
    assert_eq!(
        text,
        "Are you sure you want to delete <strong>Whisper</strong>?"
    );
}
