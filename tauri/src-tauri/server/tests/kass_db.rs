//! kass.db made by the server (`db::prepare`) against what the Python worker
//! made, recorded in `kass_db/history.json` (see `kass_db/history.py`): a new
//! install's schema, and every older version's schema and rows after the
//! last Python migrations opened it.

use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};

fn history() -> Value {
    serde_json::from_str(include_str!("kass_db/history.json")).unwrap()
}

/// A folder of its own, gone when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("kass-db-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every table and index, as `history.py` lists them.
fn schema(c: &Connection) -> Vec<Value> {
    let mut statement = c
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap();
    statement
        .query_map([], |r| {
            Ok(json!({
                "type": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "table": r.get::<_, String>(2)?,
                "sql": r.get::<_, Option<String>>(3)?,
            }))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every table's columns and rows, as `history.py` lists them.
fn rows(c: &Connection) -> Value {
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = serde_json::Map::new();
    for table in tables {
        let mut statement = c
            .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
            .unwrap();
        let columns: Vec<Value> = statement
            .column_names()
            .into_iter()
            .map(|name| json!(name))
            .collect();
        let width = columns.len();
        let rows: Vec<Value> = statement
            .query_map([], |r| {
                Ok(Value::Array(
                    (0..width)
                        .map(|i| match r.get_ref(i).unwrap() {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(n) => json!(n),
                            ValueRef::Real(x) => json!(x),
                            ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                            ValueRef::Blob(b) => panic!("{table} has a blob: {b:?}"),
                        })
                        .collect(),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.insert(table, json!({ "columns": columns, "rows": rows }));
    }
    Value::Object(out)
}

/// Prepare twice: the second time must change nothing.
fn prepare(path: &Path) -> Connection {
    kass_server::db::prepare(path).unwrap();
    kass_server::db::prepare(path).unwrap();
    Connection::open(path).unwrap()
}

#[test]
fn a_new_install_gets_the_schema_sqlalchemy_made() {
    let dir = TempDir::new("new");
    let c = prepare(&dir.0.join("kass.db"));
    assert_eq!(Value::Array(schema(&c)), history()["current"]);
}

#[test]
fn every_older_version_ends_up_as_python_left_it() {
    let history = history();
    let current = history["current"].as_array().unwrap();
    let versions = history["versions"].as_array().unwrap();
    assert!(versions.len() > 30, "history.json lists {}", versions.len());
    for version in versions {
        let label = format!("{} {}", version["revision"], version["label"]);
        let dir = TempDir::new(version["revision"].as_str().unwrap());
        let path = dir.0.join("kass.db");
        {
            let c = Connection::open(&path).unwrap();
            // As Python wrote them: the bundled SQLite checks foreign keys.
            c.pragma_update(None, "foreign_keys", false).unwrap();
            let created = version["schema"].as_array().unwrap();
            // Tables before their indexes; SQLite makes its own autoindexes.
            for kind in ["table", "index"] {
                for entry in created.iter().filter(|e| e["type"] == kind) {
                    if let Some(sql) = entry["sql"].as_str() {
                        c.execute_batch(sql).unwrap();
                    }
                }
            }
            assert_eq!(schema(&c), *created, "{label}: rebuilt as it was");
            for insert in version["inserts"].as_array().unwrap() {
                c.execute_batch(insert.as_str().unwrap()).unwrap();
            }
        }

        let c = prepare(&path);

        let expected: Vec<Value> = version["migrated"]["schema"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| match entry.as_str() {
                Some(name) => current
                    .iter()
                    .find(|e| e["name"] == name)
                    .unwrap_or_else(|| panic!("{label}: {name} is not in current"))
                    .clone(),
                None => entry.clone(),
            })
            .collect();
        assert_eq!(schema(&c), expected, "{label}: schema");
        assert_eq!(rows(&c), version["migrated"]["rows"], "{label}: rows");
    }
}
