//! `kass.db`. The app creates it, or brings an older one up to date
//! (`prepare`, `schema`), before it starts the Python worker, which reads
//! the reports and captures it trains on. The journal mode stays SQLite's
//! default, as the worker opens it.

mod schema;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// How long a statement waits for the worker's write lock: pysqlite's
/// default, which the worker uses too.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One connection, used by one statement at a time, off the async threads.
/// It opens on first use, so the server starts whether or not anything
/// reads the database yet.
#[derive(Clone)]
pub struct Db {
    path: Arc<PathBuf>,
    connection: Arc<Mutex<Option<Connection>>>,
}

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    /// The blocking task panicked or was cancelled.
    Task(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Sqlite(error) => write!(f, "{error}"),
            DbError::Task(error) => write!(f, "database task failed: {error}"),
        }
    }
}

impl std::error::Error for DbError {}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::Sqlite(error)
    }
}

impl Db {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            connection: Arc::new(Mutex::new(None)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `work` with the connection on a blocking thread, opening it first
    /// if needed.
    pub async fn call<T, F>(&self, work: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let path = Arc::clone(&self.path);
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut slot = connection.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                *slot = Some(open(&path)?);
            }
            let connection = slot.as_mut().expect("opened above");
            work(connection).map_err(DbError::from)
        })
        .await
        .map_err(|e| DbError::Task(e.to_string()))?
    }
}

/// Create kass.db at `path`, or bring an older one up to date, with no
/// data moved (`schema`). Run before the worker starts, which only uses the
/// tables made here. Takes a few milliseconds on an up-to-date database.
pub fn prepare(path: &Path) -> Result<(), String> {
    let failed =
        |error: &dyn std::fmt::Display| format!("Could not prepare {}: {error}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| failed(&e))?;
    }
    rename_old_database(path).map_err(|e| failed(&e))?;
    let mut connection = Connection::open(path).map_err(|e| failed(&e))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| failed(&e))?;
    // The bundled SQLite checks foreign keys by default; Python's never did,
    // and its migrations ran without.
    connection
        .pragma_update(None, "foreign_keys", false)
        .map_err(|e| failed(&e))?;
    schema::migrate(&mut connection).map_err(|e| failed(&e))
}

/// What a new install starts with, in a kass.db `prepare` made: the
/// settings, the Personal style and the default dictionary words. The app runs it once
/// at launch, after `prepare`. Idempotent.
pub fn seed(path: &Path) -> Result<(), String> {
    let failed =
        |error: &dyn std::fmt::Display| format!("Could not seed {}: {error}", path.display());
    let mut connection = Connection::open(path).map_err(|e| failed(&e))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| failed(&e))?;
    crate::settings::get(&mut connection).map_err(|e| failed(&e))?;
    crate::styles::ensure(&mut connection).map_err(|e| failed(&e))?;
    crate::dictionary::store::ensure_defaults(
        &mut connection,
        &path.with_file_name("dictionary-defaulted"),
    )
    .map_err(|e| failed(&e))
}

/// Kass was Herga (herga.db), and before that Voicebox (voicebox.db); carry
/// the newest name found over to `path` once, journal files and all.
fn rename_old_database(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let Some(old) = ["herga.db", "voicebox.db"]
        .iter()
        .map(|name| path.with_file_name(name))
        .find(|old| old.exists())
    else {
        return Ok(());
    };
    let name = |path: &Path, suffix: &str| {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        path.with_file_name(name)
    };
    for suffix in ["-wal", "-shm"] {
        let journal = name(&old, suffix);
        if journal.exists() {
            std::fs::rename(&journal, name(path, suffix))?;
        }
    }
    std::fs::rename(&old, path)?;
    println!("Renamed {} to {}", old.display(), path.display());
    Ok(())
}

/// Open the existing database. A missing file is an error: `prepare`
/// creates it before the server starts.
///
/// Foreign keys go unchecked, as Python's connections always left them:
/// kass.db keeps reports whose capture history retention deleted, and a
/// synced report can arrive before its capture, or without one.
fn open(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", false)?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kass-db-{name}-{}.db", std::process::id()))
    }

    #[tokio::test]
    async fn reads_what_another_connection_wrote() {
        let path = temp_path("shared");
        let _ = std::fs::remove_file(&path);
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE words (word TEXT); INSERT INTO words VALUES ('Kass');")
            .unwrap();
        let db = Db::new(path.clone());
        let word: String = db
            .call(|c| c.query_row("SELECT word FROM words", [], |row| row.get(0)))
            .await
            .unwrap();
        drop(writer);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(word, "Kass");
    }

    #[tokio::test]
    async fn keeps_a_report_whose_capture_is_gone() {
        let path = temp_path("orphan");
        let _ = std::fs::remove_file(&path);
        Connection::open(&path)
            .unwrap()
            .execute_batch(include_str!("../tests/kass_schema.sql"))
            .unwrap();
        let db = Db::new(path.clone());
        let inserted = db
            .call(|c| {
                c.execute(
                    "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot) \
                     VALUES ('r', 'gone', 'refined', 'x', '', '{}')",
                    [],
                )
            })
            .await;
        std::fs::remove_file(&path).unwrap();
        assert_eq!(inserted.unwrap(), 1);
    }

    #[tokio::test]
    async fn never_creates_a_missing_database() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let db = Db::new(path.clone());
        assert!(db.call(|c| c.execute_batch("SELECT 1")).await.is_err());
        assert!(!path.exists());
    }

    /// A database file of its own, gone when dropped.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kass-db-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDb(dir.join("kass.db"))
        }

        /// An older kass.db, as `sql` made it.
        fn old(name: &str, sql: &str) -> Self {
            let db = Self::new(name);
            Connection::open(&db.0).unwrap().execute_batch(sql).unwrap();
            db
        }

        /// Prepare it twice: the second time must change nothing.
        fn prepare(&self) -> Connection {
            prepare(&self.0).unwrap();
            prepare(&self.0).unwrap();
            Connection::open(&self.0).unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn creates_a_missing_database_and_its_folder() {
        let db = TempDb::new("fresh");
        let nested = db.0.parent().unwrap().join("data/kass.db");
        prepare(&nested).unwrap();
        let tables: i64 = Connection::open(&nested)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 10);
    }

    #[test]
    fn a_new_install_starts_with_the_personal_style_and_the_default_words() {
        let db = TempDb::new("seed");
        let mut c = db.prepare();
        crate::settings::get(&mut c).unwrap();
        c.execute(
            "UPDATE capture_settings SET punctuation_style = 'casual', preserve_technical = 0",
            [],
        )
        .unwrap();
        seed(&db.0).unwrap();
        seed(&db.0).unwrap();
        let styles: Vec<(String, String, bool, String)> = c
            .prepare("SELECT id, name, is_default, punctuation_style FROM writing_styles")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0].3, "casual");
        assert_eq!(
            (styles[0].0.as_str(), styles[0].1.as_str(), styles[0].2),
            ("personal", "Personal", true)
        );
        let words: Vec<String> = c
            .prepare("SELECT written FROM dictionary_entries")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(words, ["Kass"]);

        // A deleted default word stays deleted; two defaults become one.
        c.execute_batch(
            "DELETE FROM dictionary_entries;
             INSERT INTO writing_styles (id, name, position, is_default, punctuation_style, \
             capitalize_first, smart_cleanup, preserve_technical) \
             VALUES ('work', 'Work', 1, 1, 'standard', 1, 1, 1);",
        )
        .unwrap();
        seed(&db.0).unwrap();
        let words: i64 = c
            .query_row("SELECT count(*) FROM dictionary_entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(words, 0);
        let defaults: Vec<String> = c
            .prepare("SELECT id FROM writing_styles WHERE is_default != 0")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(defaults, ["personal"]);
    }

    #[test]
    fn upgrades_add_the_settings_with_the_defaults_an_existing_install_gets() {
        let db = TempDb::old(
            "settings",
            "CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR);
             INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo');
             CREATE TABLE captures (id VARCHAR PRIMARY KEY, audio_path VARCHAR);
             INSERT INTO captures (id, audio_path) VALUES ('a', 'captures/a.wav');",
        );
        let c = db.prepare();
        let settings: (i64, i64, i64, i64, String) = c
            .query_row(
                "SELECT history_retention_days, history_retention_confirmed, onboarding_completed,
                        discard_audio, chord_push_to_talk_keys FROM capture_settings",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        // 30 days, asked before the first deletion; already set up, so no
        // onboarding; recordings kept.
        assert_eq!(settings, (30, 0, 1, 0, r#"["MetaRight", "AltGr"]"#.into()));
        let audio_deleted: i64 = c
            .query_row("SELECT audio_deleted FROM captures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audio_deleted, 0);
    }

    #[test]
    fn an_existing_dictionary_entry_keeps_matching_by_sound_as_a_word_of_the_users() {
        let db = TempDb::old(
            "dictionary",
            "CREATE TABLE dictionary_entries (id VARCHAR PRIMARY KEY, scope VARCHAR NOT NULL,
                scope_id VARCHAR NOT NULL, app_name VARCHAR, written VARCHAR NOT NULL,
                spoken VARCHAR, key VARCHAR NOT NULL, created_at DATETIME);
             INSERT INTO dictionary_entries VALUES ('old', 'global', '', NULL, 'Zed', NULL, 'zed', NULL);",
        );
        let row: (String, Option<String>, i64, Option<String>, i64) = db
            .prepare()
            .query_row(
                "SELECT id, group_id, match_sound, source, phrase FROM dictionary_entries",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(row, ("old".into(), None, 1, None, 0));
    }

    #[test]
    fn kass_old_bundle_ids_become_its_own_and_a_row_already_under_it_wins() {
        let db = TempDb::old(
            "bundles",
            "CREATE TABLE app_styles (bundle_id VARCHAR PRIMARY KEY, app_name VARCHAR,
                style_id VARCHAR NOT NULL, updated_at DATETIME);
             INSERT INTO app_styles VALUES ('sh.voicebox.app', 'Voicebox', 'work', NULL);
             INSERT INTO app_styles VALUES ('com.mrgnhnt.herga', 'Herga', 'code', NULL);
             INSERT INTO app_styles VALUES ('com.tinyspeck.slackmacgap', 'Slack', 'chat', NULL);
             CREATE TABLE captures (id VARCHAR PRIMARY KEY, audio_path VARCHAR, app_bundle_id VARCHAR);
             INSERT INTO captures VALUES ('a', '', 'com.mrgnhnt.voicebox');",
        );
        let c = db.prepare();
        let styles: Vec<(String, String)> = c
            .prepare("SELECT bundle_id, style_id FROM app_styles ORDER BY bundle_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            styles,
            [
                ("com.mrgnhnt.kass".to_string(), "work".to_string()),
                ("com.tinyspeck.slackmacgap".into(), "chat".into()),
            ]
        );
        let app: String = c
            .query_row("SELECT app_bundle_id FROM captures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app, "com.mrgnhnt.kass");
    }

    #[test]
    fn herga_db_carries_over_with_its_journal_once() {
        let db = TempDb::new("herga");
        let herga = db.0.with_file_name("herga.db");
        Connection::open(&herga)
            .unwrap()
            .execute_batch("CREATE TABLE known_names (name VARCHAR PRIMARY KEY, last_seen_at DATETIME NOT NULL); INSERT INTO known_names VALUES ('Morgan', '2026-01-01');")
            .unwrap();
        std::fs::write(db.0.with_file_name("herga.db-wal"), b"").unwrap();
        std::fs::write(db.0.with_file_name("voicebox.db"), b"older").unwrap();
        let c = db.prepare();
        let name: String = c
            .query_row("SELECT name FROM known_names", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Morgan");
        assert!(!herga.exists());
        assert!(db.0.with_file_name("kass.db-wal").exists());
        // Only one database carries over.
        assert!(db.0.with_file_name("voicebox.db").exists());
    }
}
