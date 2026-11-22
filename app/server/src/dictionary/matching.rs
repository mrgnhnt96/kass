//! The dictionary a dictation applies to its text, built from `kass.db`
//! with kass-text's matching (`kass_text::dictionary`). Python: `for_app`
//! and `build` in `backend/services/dictionary.py`.
//!
//! Each app's dictionary is built once and kept until an entry changes.
//! The worker still edits entries too (sync), so a change is noticed from
//! the table itself rather than from this server's own writes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kass_text::dictionary::{self as text, Dictionary, Term};
use rusqlite::Connection;

use super::store::{list_entries, style_for_app};
use super::{resolve, Entry};

/// The entries of `resolved` that apply, most specific first, as matching
/// takes them.
pub fn active(resolved: &[(Entry, bool)]) -> Vec<Term> {
    resolved
        .iter()
        .filter(|(_, overridden)| !overridden)
        .map(|(entry, _)| Term {
            written: entry.written.clone(),
            spoken: entry.spoken.clone(),
            match_sound: entry.match_sound,
            phrase: entry.phrase,
        })
        .collect()
}

/// The dictionary for `resolved`, every term but the ones with sound
/// matching off matched by sound.
pub fn build(resolved: &[(Entry, bool)]) -> Dictionary {
    text::build(&active(resolved), true)
}

/// What changes whenever an entry is added, edited or deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    count: i64,
    last_row: i64,
    last_change: Option<String>,
}

fn version(conn: &Connection) -> rusqlite::Result<Version> {
    conn.query_row(
        "SELECT count(*), coalesce(max(rowid), 0), max(coalesce(updated_at, created_at)) \
         FROM dictionary_entries",
        [],
        |row| {
            Ok(Version {
                count: row.get(0)?,
                last_row: row.get(1)?,
                last_change: row.get(2)?,
            })
        },
    )
}

#[derive(Default)]
struct Built {
    version: Option<Version>,
    by_app: HashMap<(String, String), Arc<Dictionary>>,
}

/// Every app's dictionary, as dictations ask for them.
#[derive(Default)]
pub struct Dictionaries {
    built: Mutex<Built>,
}

impl Dictionaries {
    /// The dictionary a dictation in `bundle_id` uses (None: an unknown
    /// app), written in `style_id` when one was asked for, else the app's
    /// style.
    pub fn for_app(
        &self,
        conn: &Connection,
        bundle_id: Option<&str>,
        style_id: Option<&str>,
    ) -> rusqlite::Result<Arc<Dictionary>> {
        let style_id = match style_id.filter(|id| !id.is_empty()) {
            Some(id) => id.to_string(),
            None => style_for_app(conn, bundle_id)?,
        };
        let key = (bundle_id.unwrap_or("").to_string(), style_id);
        let now = version(conn)?;
        {
            let mut built = self.built.lock().unwrap_or_else(|e| e.into_inner());
            if built.version.as_ref() != Some(&now) {
                built.by_app.clear();
                built.version = Some(now.clone());
            }
            if let Some(found) = built.by_app.get(&key) {
                return Ok(Arc::clone(found));
            }
        }
        let resolved = resolve(&list_entries(conn)?, bundle_id, Some(&key.1));
        let found = Arc::new(build(&resolved));
        let mut built = self.built.lock().unwrap_or_else(|e| e.into_inner());
        if built.version.as_ref() == Some(&now) {
            built.by_app.insert(key, Arc::clone(&found));
        }
        Ok(found)
    }

    /// Forget every built dictionary.
    pub fn invalidate(&self) {
        let mut built = self.built.lock().unwrap_or_else(|e| e.into_inner());
        *built = Built::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::store::tests::database;
    use crate::dictionary::store::{add_group, delete_group, NewGroup, PlaceInput, Sync};
    use crate::dictionary::Place;

    const SLACK: &str = "com.tinyspeck.slackmacgap";

    fn add(
        conn: &mut Connection,
        written: &str,
        spoken: Option<&str>,
        place: PlaceInput,
    ) -> String {
        add_group(
            conn,
            NewGroup::new(written, spoken.map(Into::into), vec![place]),
            Sync::default(),
        )
        .unwrap()
        .unwrap()
        .id
    }

    fn app(bundle: &str) -> PlaceInput {
        PlaceInput {
            scope: "app".into(),
            scope_id: Some(bundle.into()),
            app_name: None,
        }
    }

    #[test]
    fn each_app_gets_its_own_words_and_the_global_ones() {
        let mut conn = database();
        add(
            &mut conn,
            "Voicebox",
            Some("voice box"),
            Place::global().into(),
        );
        add(&mut conn, "Kubernetes", None, app(SLACK));
        let dictionaries = Dictionaries::default();

        let slack = dictionaries.for_app(&conn, Some(SLACK), None).unwrap();
        assert_eq!(
            slack.apply("ship voice box to kubernetes"),
            "ship Voicebox to Kubernetes"
        );
        let other = dictionaries.for_app(&conn, None, None).unwrap();
        assert_eq!(
            other.apply("ship voice box to kubernetes"),
            "ship Voicebox to kubernetes"
        );
        assert!(Arc::ptr_eq(
            &slack,
            &dictionaries.for_app(&conn, Some(SLACK), None).unwrap()
        ));
    }

    #[test]
    fn a_changed_entry_rebuilds_the_dictionary() {
        let mut conn = database();
        let dictionaries = Dictionaries::default();
        assert!(dictionaries
            .for_app(&conn, None, None)
            .unwrap()
            .terms
            .is_empty());

        let id = add(&mut conn, "Kubernetes", None, Place::global().into());
        let found = dictionaries.for_app(&conn, None, None).unwrap();
        assert_eq!(found.terms, ["Kubernetes"]);

        delete_group(&mut conn, &id, Sync::default()).unwrap();
        assert!(dictionaries
            .for_app(&conn, None, None)
            .unwrap()
            .terms
            .is_empty());
    }

    #[test]
    fn the_most_specific_entry_for_a_word_wins() {
        let mut conn = database();
        add(
            &mut conn,
            "Voicebox",
            Some("voice box"),
            Place::global().into(),
        );
        add(&mut conn, "VoiceBox", Some("voice box"), app(SLACK));
        let dictionaries = Dictionaries::default();
        let slack = dictionaries.for_app(&conn, Some(SLACK), None).unwrap();
        assert_eq!(slack.apply("open voice box"), "open VoiceBox");
        assert_eq!(slack.terms, ["VoiceBox"]);
    }
}
