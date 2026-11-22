//! kass.db rows as a stand-in sends them and copies them, in the JSON the
//! Python worker wrote (`backend/services/sync/rows.py` at b340f4f7), so a
//! Mac on either side of the port understands the other.
//!
//! Each column's value is read by its declared type, the one rule for every
//! table: `DATETIME` as Python's `isoformat()`, `BOOLEAN` as true or false,
//! `JSON` as the value it holds, anything else as stored.

use chrono::{NaiveDateTime, Timelike};
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{params_from_iter, Connection};
use serde_json::{Map, Value};

use crate::dictionary::store::{parse_stored, stored};

/// One row, by column.
pub type Row = Map<String, Value>;

/// What a stand-in records and sends, by kind, with its table, in the order
/// the shared Kass applies them: a capture's style before the capture, a
/// report's capture before the report.
pub const KINDS: [(&str, &str); 7] = [
    ("style", "writing_styles"),
    ("app_style", "app_styles"),
    ("settings", "capture_settings"),
    ("dictionary", "dictionary_entries"),
    ("capture", "captures"),
    ("feedback", "capture_feedback"),
    ("take", "take_reports"),
];

/// Settings that belong to one Mac, never copied between them.
pub const OWN_SETTINGS: [&str; 4] = [
    "id",
    "input_device_id",
    "usage_device_id",
    "usage_sent_through",
];

/// Copied from a shared Kass whole, replacing the stand-in's own: the name
/// in a snapshot and the table, in the order they're written back.
pub const MIRRORED: [(&str, &str); 4] = [
    ("styles", "writing_styles"),
    ("app_styles", "app_styles"),
    ("dictionary", "dictionary_entries"),
    ("known_names", "known_names"),
];

pub fn table(kind: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, table)| *table)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Type {
    DateTime,
    Bool,
    Json,
    Plain,
}

struct Column {
    name: String,
    kind: Type,
    key: bool,
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Column>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({})", quoted(table)))?;
    let columns = statement.query_map([], |row| {
        let declared: String = row.get(2)?;
        let kind = match declared.to_ascii_uppercase().as_str() {
            "DATETIME" | "TIMESTAMP" => Type::DateTime,
            "BOOLEAN" => Type::Bool,
            "JSON" => Type::Json,
            _ => Type::Plain,
        };
        Ok(Column {
            name: row.get(1)?,
            kind,
            key: row.get::<_, i64>(5)? > 0,
        })
    })?;
    columns.collect()
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The table's one primary-key column.
fn key_column(columns: &[Column]) -> rusqlite::Result<&Column> {
    columns
        .iter()
        .find(|column| column.key)
        .ok_or_else(|| rusqlite::Error::InvalidColumnName("primary key".into()))
}

/// Python's `datetime.isoformat()`: microseconds only when there are some.
pub fn isoformat(at: NaiveDateTime) -> String {
    if at.nanosecond() / 1000 == 0 {
        at.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}

/// A time as either side writes it: stored, or `isoformat()`, with or
/// without an offset.
pub fn parse_time(text: &str) -> Option<NaiveDateTime> {
    parse_stored(Some(text.to_string())).or_else(|| {
        chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|at| at.naive_utc())
    })
}

fn to_json(value: ValueRef, kind: Type) -> Value {
    match (value, kind) {
        (ValueRef::Null, _) => Value::Null,
        (ValueRef::Integer(number), Type::Bool) => Value::Bool(number != 0),
        (ValueRef::Integer(number), _) => number.into(),
        (ValueRef::Real(number), _) => serde_json::Number::from_f64(number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        (ValueRef::Text(text), kind) => {
            let text = String::from_utf8_lossy(text);
            match kind {
                Type::DateTime => parse_time(&text)
                    .map(|at| Value::String(isoformat(at)))
                    .unwrap_or_else(|| Value::String(text.into_owned())),
                Type::Json => serde_json::from_str(&text).unwrap_or(Value::String(text.into())),
                Type::Bool => Value::Bool(!matches!(text.as_ref(), "0" | "" | "false")),
                Type::Plain => Value::String(text.into_owned()),
            }
        }
        (ValueRef::Blob(_), _) => Value::Null,
    }
}

fn to_sql(value: &Value, kind: Type) -> Sql {
    match (value, kind) {
        // SQLAlchemy's JSON keeps None as JSON's null.
        (Value::Null, Type::Json) => Sql::Text("null".into()),
        (Value::Null, _) => Sql::Null,
        (value, Type::Json) => Sql::Text(value.to_string()),
        (Value::String(text), Type::DateTime) => {
            Sql::Text(parse_time(text).map(stored).unwrap_or_else(|| text.clone()))
        }
        (Value::Bool(flag), _) => Sql::Integer(i64::from(*flag)),
        (Value::Number(number), _) => match number.as_i64() {
            Some(whole) => Sql::Integer(whole),
            None => Sql::Real(number.as_f64().unwrap_or(0.0)),
        },
        (Value::String(text), _) => Sql::Text(text.clone()),
        (value, _) => Sql::Text(value.to_string()),
    }
}

/// Every row of `table`, oldest first, optionally only the one keyed `key`.
fn select(conn: &Connection, table: &str, key: Option<&str>) -> rusqlite::Result<Vec<Row>> {
    let columns = columns(conn, table)?;
    let names: Vec<String> = columns.iter().map(|column| quoted(&column.name)).collect();
    let mut sql = format!("SELECT {} FROM {}", names.join(", "), quoted(table));
    if key.is_some() {
        sql.push_str(&format!(
            " WHERE {} = ?1",
            quoted(&key_column(&columns)?.name)
        ));
    }
    sql.push_str(" ORDER BY rowid");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(key), |row| {
        let mut out = Row::new();
        for (index, column) in columns.iter().enumerate() {
            out.insert(
                column.name.clone(),
                to_json(row.get_ref(index)?, column.kind),
            );
        }
        Ok(out)
    })?;
    rows.collect()
}

pub fn all(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Row>> {
    select(conn, table, None)
}

pub fn get(conn: &Connection, table: &str, key: &str) -> rusqlite::Result<Option<Row>> {
    Ok(select(conn, table, Some(key))?.into_iter().next())
}

/// The row's values for the table's columns, leaving out `skip` and any
/// key the table doesn't have (a column only the other Mac's Kass has).
fn values<'a>(
    columns: &'a [Column],
    row: &'a Row,
    skip: &'a [&str],
) -> impl Iterator<Item = (&'a Column, Sql)> + 'a {
    columns.iter().filter_map(move |column| {
        let value = row.get(&column.name)?;
        (!skip.contains(&column.name.as_str())).then(|| (column, to_sql(value, column.kind)))
    })
}

pub fn insert(conn: &Connection, table: &str, row: &Row, skip: &[&str]) -> rusqlite::Result<()> {
    let columns = columns(conn, table)?;
    let (names, values): (Vec<String>, Vec<Sql>) = values(&columns, row, skip)
        .map(|(column, value)| (quoted(&column.name), value))
        .unzip();
    let marks: Vec<String> = (1..=names.len()).map(|n| format!("?{n}")).collect();
    conn.execute(
        &format!(
            "INSERT INTO {} ({}) VALUES ({})",
            quoted(table),
            names.join(", "),
            marks.join(", ")
        ),
        params_from_iter(values),
    )?;
    Ok(())
}

/// Set the row keyed `key` to `row`'s values; whether there was one.
pub fn update(
    conn: &Connection,
    table: &str,
    key: &str,
    row: &Row,
    skip: &[&str],
) -> rusqlite::Result<bool> {
    let columns = columns(conn, table)?;
    let key_name = key_column(&columns)?.name.clone();
    let (sets, mut values): (Vec<String>, Vec<Sql>) = values(&columns, row, skip)
        .filter(|(column, _)| column.name != key_name)
        .enumerate()
        .map(|(index, (column, value))| {
            (format!("{} = ?{}", quoted(&column.name), index + 1), value)
        })
        .unzip();
    if sets.is_empty() {
        return Ok(true);
    }
    values.push(Sql::Text(key.to_string()));
    let changed = conn.execute(
        &format!(
            "UPDATE {} SET {} WHERE {} = ?{}",
            quoted(table),
            sets.join(", "),
            quoted(&key_name),
            values.len()
        ),
        params_from_iter(values),
    )?;
    Ok(changed > 0)
}

pub fn delete(conn: &Connection, table: &str, key: &str) -> rusqlite::Result<bool> {
    let columns = columns(conn, table)?;
    let key_name = &key_column(&columns)?.name;
    let deleted = conn.execute(
        &format!(
            "DELETE FROM {} WHERE {} = ?1",
            quoted(table),
            quoted(key_name)
        ),
        [key],
    )?;
    Ok(deleted > 0)
}

/// When the row last changed, for the later edit to win; None when it
/// doesn't say.
pub fn edited_at(row: &Row) -> Option<NaiveDateTime> {
    ["updated_at", "created_at"]
        .iter()
        .find_map(|name| row.get(*name)?.as_str().and_then(parse_time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id VARCHAR NOT NULL, \"key\" VARCHAR, flag BOOLEAN NOT NULL, \
             keys JSON NOT NULL, at DATETIME, n FLOAT, PRIMARY KEY (id))",
        )
        .unwrap();
        conn
    }

    #[test]
    fn rows_read_back_as_python_wrote_them() {
        let conn = db();
        conn.execute(
            "INSERT INTO t VALUES ('a', 'k', 1, '[\"fn\"]', '2026-01-02 03:04:05.000000', 0.5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO t VALUES ('b', NULL, 0, 'null', '2026-01-02 03:04:05.120000', NULL)",
            [],
        )
        .unwrap();
        assert_eq!(
            Value::Array(
                all(&conn, "t")
                    .unwrap()
                    .into_iter()
                    .map(Value::Object)
                    .collect()
            ),
            json!([
                {"id": "a", "key": "k", "flag": true, "keys": ["fn"], "at": "2026-01-02T03:04:05", "n": 0.5},
                {"id": "b", "key": null, "flag": false, "keys": null, "at": "2026-01-02T03:04:05.120000", "n": null},
            ])
        );
    }

    #[test]
    fn rows_written_are_stored_as_sqlalchemy_stores_them() {
        let conn = db();
        let row = json!({"id": "a", "key": "k", "flag": true, "keys": {"x": 1},
                         "at": "2026-01-02T03:04:05", "n": 2, "gone": "column only the other Mac has"});
        insert(&conn, "t", row.as_object().unwrap(), &[]).unwrap();
        let stored: (i64, String, String, f64) = conn
            .query_row("SELECT flag, keys, at, n FROM t", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(
            stored,
            (
                1,
                "{\"x\":1}".into(),
                "2026-01-02 03:04:05.000000".into(),
                2.0
            )
        );

        let edit = json!({"id": "a", "key": "new", "flag": false});
        assert!(update(&conn, "t", "a", edit.as_object().unwrap(), &["flag"]).unwrap());
        let row = get(&conn, "t", "a").unwrap().unwrap();
        assert_eq!(
            (row["key"].clone(), row["flag"].clone()),
            (json!("new"), json!(true))
        );
        assert!(!update(&conn, "t", "z", edit.as_object().unwrap(), &[]).unwrap());
        assert!(delete(&conn, "t", "a").unwrap());
        assert!(get(&conn, "t", "a").unwrap().is_none());
    }

    #[test]
    fn the_later_edit_is_judged_by_updated_then_created() {
        let row = json!({"created_at": "2026-01-01T00:00:00", "updated_at": null});
        assert_eq!(
            edited_at(row.as_object().unwrap())
                .map(isoformat)
                .as_deref(),
            Some("2026-01-01T00:00:00")
        );
        let row =
            json!({"created_at": "2026-01-01T00:00:00", "updated_at": "2026-02-01T00:00:00.5"});
        assert_eq!(
            edited_at(row.as_object().unwrap())
                .map(isoformat)
                .as_deref(),
            Some("2026-02-01T00:00:00.500000")
        );
        assert_eq!(edited_at(json!({}).as_object().unwrap()), None);
    }
}
