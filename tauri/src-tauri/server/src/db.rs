//! `kass.db`, as the Python worker created and migrated it
//! (backend/database). Nothing here changes the schema or the journal mode,
//! so the worker and this server share the file as it is.

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

/// Open the existing database. A missing file is an error: the worker
/// creates it, with its schema, before anything here reads it.
fn open(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
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
    async fn never_creates_a_missing_database() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let db = Db::new(path.clone());
        assert!(db.call(|c| c.execute_batch("SELECT 1")).await.is_err());
        assert!(!path.exists());
    }
}
