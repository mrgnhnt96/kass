//! The dictionary in `kass.db` (`dictionary_entries`), as the Python worker
//! created it. Every write runs in one transaction and, while this Mac
//! stands in for a shared Kass, records each row it changed in
//! `sync_changes` the way the worker's `services/sync/changes.py` does.
//!
//! The worker keeps the entries in memory for dictation; whoever calls a
//! write here tells it they changed (`crate::worker`).

use std::collections::{HashMap, HashSet};

use chrono::NaiveDateTime;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use super::{
    clean_spoken, clean_written, key, normal, resolve, spelled_word, EditError, Entry, Group,
    Place, MAX_ENTRIES, SOURCES,
};

/// SQLAlchemy's `DateTime` on SQLite.
const STORED: &str = "%Y-%m-%d %H:%M:%S%.6f";

pub fn now() -> NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

fn stored(at: NaiveDateTime) -> String {
    at.format(STORED).to_string()
}

fn parse_stored(text: Option<String>) -> Option<NaiveDateTime> {
    let text = text?;
    ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(&text, format).ok())
}

/// Whether a write records what it changed for the shared Kass.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sync {
    pub standing_in: bool,
}

/// Rows changed in one transaction, recorded as one batch: the last change
/// to a row wins, as in the worker's flush.
#[derive(Default)]
struct Changes(Vec<(String, bool)>);

impl Changes {
    fn changed(&mut self, id: &str) {
        self.0.push((id.to_string(), false));
    }

    fn deleted(&mut self, id: &str) {
        self.0.push((id.to_string(), true));
    }

    fn record(self, tx: &Transaction, sync: Sync, at: NaiveDateTime) -> rusqlite::Result<()> {
        if !sync.standing_in {
            return Ok(());
        }
        let mut statement = tx.prepare(
            "INSERT INTO sync_changes (kind, ref_id, deleted, changed_at) VALUES ('dictionary', ?1, ?2, ?3) \
             ON CONFLICT (kind, ref_id) DO UPDATE SET deleted = excluded.deleted, changed_at = excluded.changed_at",
        )?;
        for (id, deleted) in self.0 {
            statement.execute(params![id, deleted, stored(at)])?;
        }
        Ok(())
    }
}

// -- reading ------------------------------------------------------------------

const COLUMNS: &str = "id, scope, scope_id, app_name, written, spoken, \"key\", group_id, \
                       match_sound, source, added_by, phrase, created_at";

/// One `dictionary_entries` row.
#[derive(Clone, Debug)]
struct Stored {
    id: String,
    scope: String,
    scope_id: String,
    app_name: Option<String>,
    written: String,
    spoken: Option<String>,
    key: String,
    group_id: Option<String>,
    match_sound: Option<bool>,
    source: Option<String>,
    added_by: Option<String>,
    phrase: Option<bool>,
    created_at: Option<NaiveDateTime>,
}

impl Stored {
    fn read(row: &Row) -> rusqlite::Result<Self> {
        Ok(Stored {
            id: row.get(0)?,
            scope: row.get(1)?,
            scope_id: row.get(2)?,
            app_name: row.get(3)?,
            written: row.get(4)?,
            spoken: row.get(5)?,
            key: row.get(6)?,
            group_id: row.get(7)?,
            match_sound: row.get(8)?,
            source: row.get(9)?,
            added_by: row.get(10)?,
            phrase: row.get(11)?,
            created_at: parse_stored(row.get(12)?),
        })
    }

    fn group_id(&self) -> &str {
        self.group_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&self.id)
    }

    fn place(&self) -> Place {
        Place {
            scope: self.scope.clone(),
            scope_id: Some(self.scope_id.clone()).filter(|id| !id.is_empty()),
            app_name: self.app_name.clone(),
        }
    }

    fn entry(&self) -> Entry {
        Entry {
            id: self.id.clone(),
            scope: self.scope.clone(),
            scope_id: Some(self.scope_id.clone()).filter(|id| !id.is_empty()),
            app_name: self.app_name.clone(),
            written: self.written.clone(),
            spoken: self.spoken.clone(),
            created_at: self.created_at,
            group_id: self.group_id().to_string(),
            match_sound: self.match_sound != Some(false),
            phrase: self.phrase == Some(true),
        }
    }
}

fn query(
    conn: &Connection,
    filter: &str,
    args: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Stored>> {
    let sql = format!("SELECT {COLUMNS} FROM dictionary_entries {filter}");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(args, Stored::read)?;
    rows.collect()
}

fn rows(conn: &Connection, group_id: &str) -> rusqlite::Result<Vec<Stored>> {
    query(
        conn,
        "WHERE coalesce(group_id, id) = ?1 ORDER BY created_at",
        [group_id],
    )
}

fn group(rows: &[Stored]) -> Group {
    let first = &rows[0];
    Group {
        id: first.group_id().to_string(),
        written: first.written.clone(),
        spoken: first.spoken.clone(),
        places: rows.iter().map(Stored::place).collect(),
        created_at: rows.iter().filter_map(|row| row.created_at).min(),
        match_sound: first.match_sound != Some(false),
        source: first
            .source
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "user".into()),
        phrase: first.phrase == Some(true),
    }
}

/// Every entry, newest first.
pub fn list_groups(conn: &Connection) -> rusqlite::Result<Vec<Group>> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<Stored>> = HashMap::new();
    for row in query(conn, "ORDER BY created_at", [])? {
        let id = row.group_id().to_string();
        if !grouped.contains_key(&id) {
            order.push(id.clone());
        }
        grouped.entry(id).or_default().push(row);
    }
    let mut groups: Vec<Group> = order.iter().map(|id| group(&grouped[id])).collect();
    groups.sort_by_key(|group| std::cmp::Reverse(group.created_at));
    Ok(groups)
}

/// Every row, one per place, newest first.
pub fn list_entries(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    Ok(query(conn, "ORDER BY created_at DESC", [])?
        .iter()
        .map(Stored::entry)
        .collect())
}

/// One entry: everywhere it applies.
pub fn get_group(conn: &Connection, group_id: &str) -> rusqlite::Result<Option<Group>> {
    let rows = rows(conn, group_id)?;
    Ok((!rows.is_empty()).then(|| group(&rows)))
}

// -- writing styles, as the worker's styles snapshot reads them ----------------

/// The style used before any exist (the worker's `MIGRATED_STYLE`).
const FALLBACK_STYLE: (&str, &str) = ("personal", "Personal");

struct Styles {
    /// (id, name, is_default), in the user's order.
    styles: Vec<(String, String, bool)>,
    /// Bundle id → style id, for the apps the user assigned.
    apps: HashMap<String, String>,
}

impl Styles {
    fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let mut statement = conn.prepare(
            "SELECT id, name, is_default FROM writing_styles ORDER BY position, created_at",
        )?;
        let styles: Vec<(String, String, bool)> = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, Option<bool>>(2)? == Some(true),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if styles.is_empty() {
            let (id, name) = FALLBACK_STYLE;
            return Ok(Styles {
                styles: vec![(id.into(), name.into(), true)],
                apps: HashMap::new(),
            });
        }
        let mut statement = conn.prepare("SELECT bundle_id, style_id FROM app_styles")?;
        let apps = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Styles { styles, apps })
    }

    fn name(&self, style_id: &str) -> Option<&str> {
        self.styles
            .iter()
            .find(|(id, _, _)| id == style_id)
            .map(|(_, name, _)| name.as_str())
    }

    fn default(&self) -> &str {
        let (id, _, _) = self
            .styles
            .iter()
            .find(|(_, _, is_default)| *is_default)
            .unwrap_or(&self.styles[0]);
        id
    }

    /// The style `bundle_id` is assigned to, or the default.
    ///
    /// The worker also puts Kass's own window in the style being taught
    /// while a teach session is open, which only it knows.
    fn for_app(&self, bundle_id: Option<&str>) -> &str {
        let assigned = self.apps.get(bundle_id.unwrap_or(""));
        assigned
            .and_then(|id| self.styles.iter().find(|(style, _, _)| style == id))
            .map(|(id, _, _)| id.as_str())
            .unwrap_or_else(|| self.default())
    }
}

/// The style a dictation in `bundle_id` is written in.
pub fn style_for_app(conn: &Connection, bundle_id: Option<&str>) -> rusqlite::Result<String> {
    Ok(Styles::load(conn)?.for_app(bundle_id).to_string())
}

// -- places -------------------------------------------------------------------

/// A place as the API sends it, before it's checked.
#[derive(Clone, Debug, Default)]
pub struct PlaceInput {
    pub scope: String,
    pub scope_id: Option<String>,
    pub app_name: Option<String>,
}

impl From<Place> for PlaceInput {
    fn from(place: Place) -> Self {
        PlaceInput {
            scope: place.scope,
            scope_id: place.scope_id,
            app_name: place.app_name,
        }
    }
}

fn place(input: &PlaceInput, styles: &Styles) -> Result<Place, EditError> {
    if !["app", "style", "global"].contains(&input.scope.as_str()) {
        return Err(EditError::Invalid("Unknown dictionary scope".into()));
    }
    if input.scope == "global" {
        return Ok(Place::global());
    }
    let scope_id = input.scope_id.as_deref().map(super::strip).unwrap_or("");
    if scope_id.is_empty() {
        return Err(EditError::Invalid(
            "Choose the style or app this entry is for".into(),
        ));
    }
    if input.scope == "style" {
        if styles.name(scope_id).is_none() {
            return Err(EditError::Invalid(
                "That writing style doesn't exist".into(),
            ));
        }
        return Ok(Place {
            scope: "style".into(),
            scope_id: Some(scope_id.into()),
            app_name: None,
        });
    }
    let app_name = input
        .app_name
        .clone()
        .filter(|name| !super::strip(name).is_empty());
    Ok(Place {
        scope: "app".into(),
        scope_id: Some(scope_id.into()),
        app_name,
    })
}

/// Checked, without repeats; everywhere replaces every other place.
fn places(inputs: &[PlaceInput], styles: &Styles) -> Result<Vec<Place>, EditError> {
    let mut cleaned: Vec<Place> = Vec::new();
    for input in inputs {
        let found = place(input, styles)?;
        if found.scope == "global" {
            return Ok(vec![found]);
        }
        if !cleaned
            .iter()
            .any(|p| p.scope == found.scope && p.scope_id == found.scope_id)
        {
            cleaned.push(found);
        }
    }
    if cleaned.is_empty() {
        return Err(EditError::Invalid("Choose where this word applies".into()));
    }
    Ok(cleaned)
}

fn place_name(place: &Place, styles: &Styles) -> String {
    match place.scope.as_str() {
        "global" => "everywhere".into(),
        "style" => match place.scope_id.as_deref().and_then(|id| styles.name(id)) {
            Some(name) => format!("the {name} style"),
            None => "that style".into(),
        },
        _ => place
            .app_name
            .clone()
            .or_else(|| place.scope_id.clone())
            .unwrap_or_default(),
    }
}

fn check_unique(
    conn: &Connection,
    styles: &Styles,
    place: &Place,
    key: &str,
    group_id: Option<&str>,
) -> rusqlite::Result<Result<(), EditError>> {
    let scope_id = place.scope_id.as_deref().unwrap_or("");
    let taken: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM dictionary_entries WHERE scope = ?1 AND scope_id = ?2 AND \"key\" = ?3 \
             AND (?4 IS NULL OR coalesce(group_id, id) != ?4) LIMIT 1",
            params![place.scope, scope_id, key, group_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match taken {
        Some(_) => Err(EditError::Duplicate(format!(
            "That word is already in the dictionary for {}",
            place_name(place, styles)
        ))),
        None => Ok(()),
    })
}

// -- writing ------------------------------------------------------------------

/// Runs `edit` in a transaction that holds the write lock from its first
/// read, committed only when the edit is accepted.
fn write<T>(
    conn: &mut Connection,
    edit: impl FnOnce(&Transaction) -> rusqlite::Result<Result<T, EditError>>,
) -> rusqlite::Result<Result<T, EditError>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let done = edit(&tx)?;
    if done.is_ok() {
        tx.commit()?;
    }
    Ok(done)
}

/// What to keep in a new entry.
#[derive(Clone, Debug)]
pub struct NewGroup {
    pub written: String,
    pub spoken: Option<String>,
    pub places: Vec<PlaceInput>,
    pub match_sound: bool,
    pub source: String,
    /// The capture or report that added it, so deleting that removes it.
    pub added_by: Option<String>,
    /// `written` is text to insert where `spoken` is said.
    pub phrase: bool,
}

impl NewGroup {
    pub fn new(
        written: impl Into<String>,
        spoken: Option<String>,
        places: Vec<PlaceInput>,
    ) -> Self {
        NewGroup {
            written: written.into(),
            spoken,
            places,
            match_sound: true,
            source: "user".into(),
            added_by: None,
            phrase: false,
        }
    }
}

struct NewRow<'a> {
    group_id: &'a str,
    place: &'a Place,
    written: &'a str,
    spoken: Option<&'a str>,
    key: &'a str,
    created_at: Option<NaiveDateTime>,
    match_sound: bool,
    source: Option<&'a str>,
    added_by: Option<&'a str>,
    phrase: bool,
}

fn insert(
    tx: &Transaction,
    row: NewRow,
    at: NaiveDateTime,
    changes: &mut Changes,
) -> rusqlite::Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO dictionary_entries (id, scope, scope_id, app_name, written, spoken, \"key\", group_id, \
         match_sound, source, added_by, phrase, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            id,
            row.place.scope,
            row.place.scope_id.as_deref().unwrap_or(""),
            row.place.app_name,
            row.written,
            row.spoken,
            row.key,
            row.group_id,
            row.match_sound,
            row.source,
            row.added_by,
            row.phrase,
            stored(row.created_at.unwrap_or(at)),
            stored(at),
        ],
    )?;
    changes.changed(&id);
    Ok(())
}

fn count_groups(conn: &Connection) -> rusqlite::Result<usize> {
    conn.query_row(
        "SELECT count(DISTINCT coalesce(group_id, id)) FROM dictionary_entries",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as usize)
}

fn add_in(
    tx: &Transaction,
    new: NewGroup,
    sync: Sync,
) -> rusqlite::Result<Result<Group, EditError>> {
    let styles = Styles::load(tx)?;
    let written = match clean_written(&new.written, new.phrase) {
        Ok(written) => written,
        Err(error) => return Ok(Err(error)),
    };
    let spoken = match clean_spoken(new.spoken.as_deref()) {
        Ok(spoken) => spoken,
        Err(error) => return Ok(Err(error)),
    };
    let places = match places(&new.places, &styles) {
        Ok(places) => places,
        Err(error) => return Ok(Err(error)),
    };
    if new.phrase && spoken.is_none() {
        return Ok(Err(EditError::Invalid(
            "Say what writes this phrase".into(),
        )));
    }
    if count_groups(tx)? >= MAX_ENTRIES {
        return Ok(Err(EditError::Invalid(format!(
            "Dictionaries can hold at most {MAX_ENTRIES} entries"
        ))));
    }
    let key = key(&written, spoken.as_deref());
    for place in &places {
        if let Err(error) = check_unique(tx, &styles, place, &key, None)? {
            return Ok(Err(error));
        }
    }
    if !SOURCES.contains(&new.source.as_str()) {
        return Ok(Err(EditError::Invalid("Unknown dictionary source".into())));
    }
    let group_id = uuid::Uuid::new_v4().to_string();
    let at = now();
    let mut changes = Changes::default();
    for place in &places {
        let row = NewRow {
            group_id: &group_id,
            place,
            written: &written,
            spoken: spoken.as_deref(),
            key: &key,
            created_at: Some(at),
            match_sound: new.match_sound,
            source: (new.source != "user").then_some(new.source.as_str()),
            added_by: new.added_by.as_deref(),
            phrase: new.phrase,
        };
        insert(tx, row, at, &mut changes)?;
    }
    changes.record(tx, sync, at)?;
    Ok(Ok(group(&rows(tx, &group_id)?)))
}

pub fn add_group(
    conn: &mut Connection,
    new: NewGroup,
    sync: Sync,
) -> rusqlite::Result<Result<Group, EditError>> {
    write(conn, |tx| add_in(tx, new, sync))
}

/// What an edit changes; `None` keeps it as it is.
#[derive(Clone, Debug, Default)]
pub struct GroupPatch {
    pub written: Option<String>,
    /// `Some(None)` removes what's said: the entry becomes a term.
    pub spoken: Option<Option<String>>,
    pub places: Option<Vec<PlaceInput>>,
    pub match_sound: Option<bool>,
}

/// Change an entry's words everywhere it applies, where it applies, and
/// whether it matches by sound. An entry the user edits is theirs from then
/// on. `Ok(Ok(None))`: there's no such entry.
pub fn update_group(
    conn: &mut Connection,
    group_id: &str,
    patch: GroupPatch,
    sync: Sync,
) -> rusqlite::Result<Result<Option<Group>, EditError>> {
    write(conn, |tx| {
        let rows = rows(tx, group_id)?;
        if rows.is_empty() {
            return Ok(Ok(None));
        }
        let styles = Styles::load(tx)?;
        let current = group(&rows);
        let written = match patch.written.as_deref() {
            Some(text) => match clean_written(text, current.phrase) {
                Ok(written) => written,
                Err(error) => return Ok(Err(error)),
            },
            None => current.written.clone(),
        };
        let spoken = match &patch.spoken {
            Some(spoken) => match clean_spoken(spoken.as_deref()) {
                Ok(spoken) => spoken,
                Err(error) => return Ok(Err(error)),
            },
            None => current.spoken.clone(),
        };
        if current.phrase && spoken.is_none() {
            return Ok(Err(EditError::Invalid(
                "Say what writes this phrase".into(),
            )));
        }
        let wanted_places = match &patch.places {
            Some(inputs) => match places(inputs, &styles) {
                Ok(places) => places,
                Err(error) => return Ok(Err(error)),
            },
            None => current.places.clone(),
        };
        let match_sound = patch.match_sound.unwrap_or(current.match_sound);
        let key = key(&written, spoken.as_deref());
        for place in &wanted_places {
            if let Err(error) = check_unique(tx, &styles, place, &key, Some(group_id))? {
                return Ok(Err(error));
            }
        }
        let at = now();
        let mut changes = Changes::default();
        let mut wanted: Vec<Option<&Place>> = wanted_places.iter().map(Some).collect();
        for row in &rows {
            let found = wanted.iter_mut().find(|place| {
                place.is_some_and(|p| {
                    p.scope == row.scope && p.scope_id.as_deref().unwrap_or("") == row.scope_id
                })
            });
            let Some(place) = found.and_then(Option::take) else {
                tx.execute("DELETE FROM dictionary_entries WHERE id = ?1", [&row.id])?;
                changes.deleted(&row.id);
                continue;
            };
            let app_name = place.app_name.clone().or_else(|| row.app_name.clone());
            let unchanged = row.written == written
                && row.spoken == spoken
                && row.key == key
                && row.group_id.as_deref() == Some(group_id)
                && row.match_sound == Some(match_sound)
                && row.source.is_none()
                && row.added_by.is_none()
                && row.app_name == app_name;
            if unchanged {
                continue;
            }
            tx.execute(
                "UPDATE dictionary_entries SET written = ?2, spoken = ?3, \"key\" = ?4, group_id = ?5, \
                 match_sound = ?6, source = NULL, added_by = NULL, app_name = ?7, updated_at = ?8 WHERE id = ?1",
                params![row.id, written, spoken, key, group_id, match_sound, app_name, stored(at)],
            )?;
            changes.changed(&row.id);
        }
        for place in wanted.into_iter().flatten() {
            let row = NewRow {
                group_id,
                place,
                written: &written,
                spoken: spoken.as_deref(),
                key: &key,
                created_at: current.created_at,
                match_sound,
                source: None,
                added_by: None,
                phrase: current.phrase,
            };
            insert(tx, row, at, &mut changes)?;
        }
        changes.record(tx, sync, at)?;
        Ok(Ok(Some(group(&self::rows(tx, group_id)?))))
    })
}

fn delete_rows(tx: &Transaction, rows: &[Stored], sync: Sync) -> rusqlite::Result<()> {
    let mut changes = Changes::default();
    for row in rows {
        tx.execute("DELETE FROM dictionary_entries WHERE id = ?1", [&row.id])?;
        changes.deleted(&row.id);
    }
    changes.record(tx, sync, now())
}

/// Delete an entry everywhere it applies. False: there's no such entry.
pub fn delete_group(conn: &mut Connection, group_id: &str, sync: Sync) -> rusqlite::Result<bool> {
    write(conn, |tx| {
        let rows = rows(tx, group_id)?;
        delete_rows(tx, &rows, sync)?;
        Ok(Ok(!rows.is_empty()))
    })
    .map(|done| done.unwrap_or(false))
}

/// Remove the words a voice edit capture spelled, or a report corrected
/// (`capture_id` is the report's id then), into the dictionary, except
/// those the user has edited since (they are theirs then). Whether any were.
pub fn delete_added_by(
    conn: &mut Connection,
    capture_id: &str,
    sync: Sync,
) -> rusqlite::Result<bool> {
    write(conn, |tx| {
        let rows = query(
            tx,
            "WHERE added_by = ?1 AND source IN ('spoken_fix', 'correction')",
            [capture_id],
        )?;
        delete_rows(tx, &rows, sync)?;
        Ok(Ok(!rows.is_empty()))
    })
    .map(|done| done.unwrap_or(false))
}

/// A deleted style's entries join `to_style_id`'s; one it already has is
/// dropped.
pub fn move_style(
    conn: &mut Connection,
    style_id: &str,
    to_style_id: &str,
    sync: Sync,
) -> rusqlite::Result<()> {
    write(conn, |tx| {
        let moving = query(tx, "WHERE scope = 'style' AND scope_id = ?1", [style_id])?;
        let mut taken: HashSet<String> =
            query(tx, "WHERE scope = 'style' AND scope_id = ?1", [to_style_id])?
                .into_iter()
                .map(|row| row.key)
                .collect();
        let at = now();
        let mut changes = Changes::default();
        for row in moving {
            if taken.contains(&row.key) {
                tx.execute("DELETE FROM dictionary_entries WHERE id = ?1", [&row.id])?;
                changes.deleted(&row.id);
            } else {
                tx.execute(
                    "UPDATE dictionary_entries SET scope_id = ?2, updated_at = ?3 WHERE id = ?1",
                    params![row.id, to_style_id, stored(at)],
                )?;
                changes.changed(&row.id);
                taken.insert(row.key);
            }
        }
        changes.record(tx, sync, at)?;
        Ok(Ok(()))
    })
    .map(|_| ())
}

/// Keep a word the user spelled aloud to fix it, with no confirmation.
///
/// `letters` are the spelled letters as the worker joined them ("MEGHAN");
/// `heard` is the word the fix was for, when known. The word is added
/// everywhere, since a name is the same name in every app, spelling only:
/// it never replaces a word that sounds like it (a real "Megan").
/// `bundle_id` is the app it was spelled in: nothing is added when that
/// app's dictionary already writes the word, and an entry the user made is
/// never changed. `added_by` is the voice edit capture that spelled it.
/// Returns the entry that now has the word, or None when there's no word or
/// it can't be added (a full dictionary, or the same word said writing
/// something else).
pub fn add_spelled_word(
    conn: &mut Connection,
    letters: &str,
    bundle_id: Option<&str>,
    heard: Option<&str>,
    added_by: Option<&str>,
    sync: Sync,
) -> rusqlite::Result<Option<Group>> {
    let written = spelled_word(letters, heard);
    let Ok(written) = clean_written(&written, false) else {
        return Ok(None);
    };
    let added = write(conn, |tx| {
        let wanted = normal(&written);
        let style_id = Styles::load(tx)?.for_app(bundle_id).to_string();
        let entries = list_entries(tx)?;
        let has = resolve(&entries, bundle_id, Some(&style_id))
            .into_iter()
            .find(|(entry, overridden)| {
                !overridden && !entry.phrase && normal(&entry.written) == wanted
            });
        if let Some((entry, _)) = has {
            return Ok(Ok(group(&rows(tx, &entry.group_id)?)));
        }
        let mut new = NewGroup::new(written.clone(), None, vec![Place::global().into()]);
        new.match_sound = false;
        new.source = "spoken_fix".into();
        new.added_by = added_by.map(Into::into);
        add_in(tx, new, sync)
    })?;
    Ok(added.ok())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `kass.db` with the worker's schema and its first style.
    pub fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../tests/kass_schema.sql"))
            .unwrap();
        conn.execute_batch(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style, capitalize_first, \
             smart_cleanup, preserve_technical, description, created_at, updated_at) VALUES \
             ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1, '', '2026-01-01 00:00:00.000000', NULL), \
             ('formal', 'Formal', 1, 0, 'standard', 1, 1, 1, '', '2026-01-02 00:00:00.000000', NULL);",
        )
        .unwrap();
        conn
    }

    const STAND_IN: Sync = Sync { standing_in: true };
    const LOCAL: Sync = Sync { standing_in: false };

    fn global() -> Vec<PlaceInput> {
        vec![Place::global().into()]
    }

    fn app(bundle: &str, name: Option<&str>) -> PlaceInput {
        PlaceInput {
            scope: "app".into(),
            scope_id: Some(bundle.into()),
            app_name: name.map(Into::into),
        }
    }

    fn style(id: &str) -> PlaceInput {
        PlaceInput {
            scope: "style".into(),
            scope_id: Some(id.into()),
            app_name: None,
        }
    }

    fn add(
        conn: &mut Connection,
        written: &str,
        spoken: Option<&str>,
        places: Vec<PlaceInput>,
    ) -> Result<Group, EditError> {
        add_group(
            conn,
            NewGroup::new(written, spoken.map(Into::into), places),
            LOCAL,
        )
        .unwrap()
    }

    fn changes(conn: &Connection) -> Vec<(String, bool)> {
        let mut statement = conn
            .prepare("SELECT ref_id, deleted FROM sync_changes WHERE kind = 'dictionary' ORDER BY ref_id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn row_ids(conn: &Connection) -> Vec<String> {
        let mut ids: Vec<String> = query(conn, "", [])
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn an_entry_applies_in_every_place_it_was_added_to() {
        let mut conn = database();
        let added = add(
            &mut conn,
            "  k8s ",
            Some("kates"),
            vec![
                app("dev.zed.Zed", Some("Zed")),
                style("formal"),
                app("dev.zed.Zed", None),
            ],
        )
        .unwrap();
        assert_eq!(added.written, "k8s");
        assert_eq!(added.spoken.as_deref(), Some("kates"));
        assert_eq!(added.places.len(), 2);
        assert_eq!(added.source, "user");
        let listed = list_groups(&conn).unwrap();
        assert_eq!(listed, vec![added.clone()]);
        assert_eq!(list_entries(&conn).unwrap().len(), 2);
    }

    #[test]
    fn everywhere_replaces_every_other_place() {
        let mut conn = database();
        let added = add(
            &mut conn,
            "Kass",
            None,
            vec![style("formal"), Place::global().into()],
        )
        .unwrap();
        assert_eq!(added.places, vec![Place::global()]);
    }

    #[test]
    fn places_are_checked() {
        let mut conn = database();
        let refused = |conn: &mut Connection, places: Vec<PlaceInput>| {
            add(conn, "Kass", None, places)
                .unwrap_err()
                .message()
                .to_string()
        };
        assert_eq!(refused(&mut conn, vec![]), "Choose where this word applies");
        assert_eq!(
            refused(&mut conn, vec![style("gone")]),
            "That writing style doesn't exist"
        );
        assert_eq!(
            refused(&mut conn, vec![app("  ", None)]),
            "Choose the style or app this entry is for"
        );
        let unknown = PlaceInput {
            scope: "team".into(),
            ..Default::default()
        };
        assert_eq!(
            refused(&mut conn, vec![unknown]),
            "Unknown dictionary scope"
        );
    }

    #[test]
    fn the_same_word_said_is_refused_in_the_same_place() {
        let mut conn = database();
        add(&mut conn, "Kubernetes", None, global()).unwrap();
        let refused = add(&mut conn, "kubernetes", None, global()).unwrap_err();
        assert!(matches!(refused, EditError::Duplicate(_)));
        assert_eq!(
            refused.message(),
            "That word is already in the dictionary for everywhere"
        );
        add(&mut conn, "Formalword", None, vec![style("formal")]).unwrap();
        let refused = add(&mut conn, "FORMALWORD", None, vec![style("formal")]).unwrap_err();
        assert_eq!(
            refused.message(),
            "That word is already in the dictionary for the Formal style"
        );
        add(
            &mut conn,
            "Zedword",
            None,
            vec![app("dev.zed.Zed", Some("Zed"))],
        )
        .unwrap();
        let refused = add(&mut conn, "zedword", None, vec![app("dev.zed.Zed", None)]).unwrap_err();
        assert_eq!(
            refused.message(),
            "That word is already in the dictionary for dev.zed.Zed"
        );
        // Another place may have it.
        add(&mut conn, "KUBERNETES", None, vec![style("formal")]).unwrap();
    }

    #[test]
    fn a_phrase_needs_what_is_said() {
        let mut conn = database();
        let mut new = NewGroup::new("Thanks,\n  Morgan", None, global());
        new.phrase = true;
        let refused = add_group(&mut conn, new.clone(), LOCAL)
            .unwrap()
            .unwrap_err();
        assert_eq!(refused.message(), "Say what writes this phrase");
        new.spoken = Some("sign off".into());
        let added = add_group(&mut conn, new, LOCAL).unwrap().unwrap();
        assert_eq!(added.written, "Thanks,\nMorgan");
        assert!(added.phrase);
        let refused = update_group(
            &mut conn,
            &added.id,
            GroupPatch {
                spoken: Some(None),
                ..Default::default()
            },
            LOCAL,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(refused.message(), "Say what writes this phrase");
    }

    #[test]
    fn a_full_dictionary_takes_no_more() {
        let mut conn = database();
        let tx = conn.transaction().unwrap();
        for n in 0..MAX_ENTRIES {
            tx.execute(
                "INSERT INTO dictionary_entries (id, scope, scope_id, written, \"key\") VALUES (?1, 'global', '', ?1, ?1)",
                [format!("word{n}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let refused = add(&mut conn, "Kass", None, global()).unwrap_err();
        assert_eq!(
            refused.message(),
            "Dictionaries can hold at most 1000 entries"
        );
    }

    #[test]
    fn an_edit_moves_the_entry_between_places() {
        let mut conn = database();
        let added = add(
            &mut conn,
            "k8s",
            Some("kates"),
            vec![Place::global().into()],
        )
        .unwrap();
        let first_row = row_ids(&conn);
        let patch = GroupPatch {
            written: Some("K8s".into()),
            places: Some(vec![app("dev.zed.Zed", Some("Zed")), style("formal")]),
            match_sound: Some(false),
            ..Default::default()
        };
        let edited = update_group(&mut conn, &added.id, patch, STAND_IN)
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(edited.id, added.id);
        assert_eq!(edited.written, "K8s");
        assert_eq!(edited.spoken.as_deref(), Some("kates"));
        assert!(!edited.match_sound);
        assert_eq!(edited.created_at, added.created_at);
        let scopes: Vec<&str> = edited.places.iter().map(|p| p.scope.as_str()).collect();
        assert_eq!(scopes, ["app", "style"]);
        let recorded = changes(&conn);
        assert_eq!(recorded.len(), 3);
        assert!(recorded.contains(&(first_row[0].clone(), true)));
        let missing = update_group(&mut conn, "nope", GroupPatch::default(), LOCAL)
            .unwrap()
            .unwrap();
        assert_eq!(missing, None);
    }

    #[test]
    fn an_edit_that_changes_nothing_records_nothing() {
        let mut conn = database();
        let added = add(&mut conn, "Kass", None, global()).unwrap();
        let patch = GroupPatch {
            written: Some(" Kass ".into()),
            ..Default::default()
        };
        update_group(&mut conn, &added.id, patch, STAND_IN)
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(changes(&conn).is_empty());
    }

    #[test]
    fn an_edit_takes_an_entry_the_worker_added() {
        let mut conn = database();
        let added = add_spelled_word(&mut conn, "MEGHAN", None, None, Some("capture-1"), LOCAL)
            .unwrap()
            .unwrap();
        assert_eq!(added.source, "spoken_fix");
        let patch = GroupPatch {
            match_sound: Some(true),
            ..Default::default()
        };
        let edited = update_group(&mut conn, &added.id, patch, LOCAL)
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(edited.source, "user");
        assert!(!delete_added_by(&mut conn, "capture-1", LOCAL).unwrap());
        assert_eq!(list_groups(&conn).unwrap().len(), 1);
    }

    #[test]
    fn deleting_records_each_row_for_the_shared_kass() {
        let mut conn = database();
        let added = add(
            &mut conn,
            "k8s",
            None,
            vec![style("formal"), style("personal")],
        )
        .unwrap();
        let ids = row_ids(&conn);
        assert!(delete_group(&mut conn, &added.id, STAND_IN).unwrap());
        assert!(!delete_group(&mut conn, &added.id, STAND_IN).unwrap());
        assert_eq!(
            changes(&conn),
            ids.into_iter().map(|id| (id, true)).collect::<Vec<_>>()
        );
        assert!(list_groups(&conn).unwrap().is_empty());
    }

    #[test]
    fn adding_records_only_while_standing_in() {
        let mut conn = database();
        add(&mut conn, "Kass", None, global()).unwrap();
        assert!(changes(&conn).is_empty());
        add_group(&mut conn, NewGroup::new("Zed", None, global()), STAND_IN)
            .unwrap()
            .unwrap();
        assert_eq!(changes(&conn).len(), 1);
    }

    #[test]
    fn a_spelled_word_is_added_once_and_never_over_the_users_own() {
        let mut conn = database();
        let added = add_spelled_word(
            &mut conn,
            "MEGHAN",
            Some("dev.zed.Zed"),
            Some("Megan"),
            None,
            LOCAL,
        )
        .unwrap()
        .unwrap();
        assert_eq!(added.written, "Meghan");
        assert!(!added.match_sound);
        assert_eq!(added.places, vec![Place::global()]);
        let again = add_spelled_word(&mut conn, "meghan", None, None, None, LOCAL)
            .unwrap()
            .unwrap();
        assert_eq!(again.id, added.id);
        // Said "kates", but the user's entry for "kates" writes k8s.
        add(&mut conn, "k8s", Some("Kates"), global()).unwrap();
        assert_eq!(
            add_spelled_word(&mut conn, "KATES", None, None, None, LOCAL).unwrap(),
            None
        );
        assert_eq!(
            add_spelled_word(&mut conn, " . ", None, None, None, LOCAL).unwrap(),
            None
        );
    }

    #[test]
    fn deleting_a_capture_removes_the_words_it_spelled() {
        let mut conn = database();
        add_spelled_word(&mut conn, "MEGHAN", None, None, Some("capture-1"), LOCAL).unwrap();
        add(&mut conn, "Kass", None, global()).unwrap();
        assert!(delete_added_by(&mut conn, "capture-1", LOCAL).unwrap());
        let left: Vec<String> = list_groups(&conn)
            .unwrap()
            .into_iter()
            .map(|g| g.written)
            .collect();
        assert_eq!(left, ["Kass"]);
    }

    #[test]
    fn a_deleted_styles_entries_join_another_style() {
        let mut conn = database();
        add(&mut conn, "Kass", None, vec![style("formal")]).unwrap();
        add(&mut conn, "k8s", None, vec![style("formal")]).unwrap();
        add(&mut conn, "K8S", None, vec![style("personal")]).unwrap();
        move_style(&mut conn, "formal", "personal", STAND_IN).unwrap();
        let mut moved: Vec<(String, Option<String>)> = list_entries(&conn)
            .unwrap()
            .into_iter()
            .map(|e| (e.written, e.scope_id))
            .collect();
        moved.sort();
        assert_eq!(
            moved,
            [
                ("K8S".into(), Some("personal".into())),
                ("Kass".into(), Some("personal".into()))
            ]
        );
        assert_eq!(changes(&conn).len(), 2);
    }

    #[test]
    fn a_dictation_uses_its_apps_style() {
        let mut conn = database();
        assert_eq!(
            style_for_app(&conn, Some("dev.zed.Zed")).unwrap(),
            "personal"
        );
        conn.execute(
            "INSERT INTO app_styles (bundle_id, app_name, style_id) VALUES ('dev.zed.Zed', 'Zed', 'formal')",
            [],
        )
        .unwrap();
        assert_eq!(style_for_app(&conn, Some("dev.zed.Zed")).unwrap(), "formal");
        assert_eq!(style_for_app(&conn, None).unwrap(), "personal");
        conn.execute_batch("DELETE FROM app_styles; DELETE FROM writing_styles;")
            .unwrap();
        assert_eq!(style_for_app(&conn, None).unwrap(), "personal");
        add(&mut conn, "Kass", None, global()).unwrap();
    }

    #[test]
    fn rows_the_worker_wrote_read_back() {
        let conn = database();
        conn.execute(
            "INSERT INTO dictionary_entries (id, scope, scope_id, written, \"key\", group_id, match_sound, phrase, created_at) \
             VALUES ('a', 'global', '', 'Kass', 'kass', NULL, 1, 0, '2026-03-04 05:06:07.089000')",
            [],
        )
        .unwrap();
        let groups = list_groups(&conn).unwrap();
        assert_eq!(groups[0].id, "a");
        assert_eq!(
            groups[0].created_at,
            chrono::NaiveDate::from_ymd_opt(2026, 3, 4)
                .unwrap()
                .and_hms_micro_opt(5, 6, 7, 89_000)
        );
        assert_eq!(
            parse_stored(Some("2026-03-04 05:06:07".into()))
                .unwrap()
                .to_string(),
            "2026-03-04 05:06:07"
        );
    }
}
