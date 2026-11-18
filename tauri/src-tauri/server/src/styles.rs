//! Named writing styles and the apps assigned to them in `kass.db`
//! (docs/plans/PER_APP_STYLE.md). Python: `backend/services/styles.py`.
//!
//! Dictation reads the same rows to pick a style's cleanup prompt
//! (`cleanup_inputs`); what each style learned is kept in `learned`.
//! Every write runs in one transaction and, while this Mac stands in for a
//! shared Kass, records each row it changed in `sync_changes`.

use std::collections::{HashMap, HashSet};

use chrono::NaiveDateTime;
use kass_text::py;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::dictionary::store::{now, parse_stored, Sync};

mod spoken;

pub use spoken::spoken_style;

pub const MAX_NAME_CHARS: usize = 40;
/// Each style keeps its own prompt cache in the cleanup model.
pub const MAX_STYLES: i64 = 6;
pub const MAX_DESCRIPTION_CHARS: usize = 600;
pub const PUNCTUATION_STYLES: [&str; 3] = ["standard", "casual", "learned"];

/// Kass's own window: dictation there follows the style being taught while
/// a teach session is open, which only the worker knows.
pub const KASS_BUNDLE: &str = "com.mrgnhnt.kass";

/// The one style a new install starts with, used until `ensure` makes it.
pub(crate) const MIGRATED_STYLE: &str = "personal";
const MIGRATED_NAME: &str = "Personal";

/// SQLAlchemy's `DateTime` on SQLite.
const STORED: &str = "%Y-%m-%d %H:%M:%S%.6f";

/// Correction sources that teach a style (`CaptureFeedback.EXPLICIT_SOURCES`).
const TEACHING_SOURCES: &str = "('manual', 'voice_fix')";

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Style {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub is_default: bool,
    pub punctuation_style: String,
    pub preserve_technical: bool,
    /// How the user says they write in the style's apps.
    pub description: String,
}

impl Style {
    fn read(row: &Row) -> rusqlite::Result<Self> {
        let punctuation: String = row.get(4)?;
        Ok(Style {
            id: row.get(0)?,
            name: row.get(1)?,
            position: row.get(2)?,
            is_default: row.get::<_, Option<bool>>(3)? == Some(true),
            punctuation_style: if PUNCTUATION_STYLES.contains(&punctuation.as_str()) {
                punctuation
            } else {
                "standard".into()
            },
            preserve_technical: row.get::<_, Option<bool>>(5)? != Some(false),
            description: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        })
    }

    fn fallback() -> Self {
        Style {
            id: MIGRATED_STYLE.into(),
            name: MIGRATED_NAME.into(),
            position: 0,
            is_default: true,
            punctuation_style: "standard".into(),
            preserve_technical: true,
            description: String::new(),
        }
    }
}

const STYLE_COLUMNS: &str =
    "id, name, position, is_default, punctuation_style, preserve_technical, description";

/// The styles and assignments, as the worker's `Snapshot` holds them.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub styles: Vec<Style>,
    /// (bundle id, style id) for the apps the user assigned, in row order.
    pub apps: Vec<(String, String)>,
    pub app_names: HashMap<String, String>,
    /// The style being taught, which Kass's own window uses meanwhile.
    pub teaching: Option<String>,
}

impl Snapshot {
    pub fn default_style(&self) -> &Style {
        self.styles
            .iter()
            .find(|style| style.is_default)
            .unwrap_or(&self.styles[0])
    }

    pub fn get(&self, style_id: Option<&str>) -> Option<&Style> {
        let style_id = style_id?;
        self.styles.iter().find(|style| style.id == style_id)
    }

    pub fn assigned(&self, bundle_id: &str) -> Option<&str> {
        self.apps
            .iter()
            .find(|(bundle, _)| bundle == bundle_id)
            .map(|(_, style)| style.as_str())
    }

    /// `style_id`'s style, or the default for `None` or a deleted style.
    pub fn resolve(&self, style_id: Option<&str>) -> &Style {
        self.get(style_id).unwrap_or_else(|| self.default_style())
    }

    /// The style `bundle_id` is assigned to, or the default.
    pub fn for_app(&self, bundle_id: Option<&str>) -> &Style {
        if bundle_id == Some(KASS_BUNDLE) {
            if let Some(taught) = self.get(self.teaching.as_deref()) {
                return taught;
            }
        }
        self.get(self.assigned(bundle_id.unwrap_or("")))
            .unwrap_or_else(|| self.default_style())
    }

    /// The style a capture's corrections teach: the one they were left
    /// with, while it exists, else the style of the app.
    pub fn correction_style(&self, bundle_id: Option<&str>, teaches: Option<&str>) -> &str {
        match self.get(teaches) {
            Some(left) => &left.id,
            None => &self.for_app(bundle_id).id,
        }
    }
}

pub fn load(conn: &Connection, teaching: Option<String>) -> rusqlite::Result<Snapshot> {
    let mut statement = conn.prepare(&format!(
        "SELECT {STYLE_COLUMNS} FROM writing_styles ORDER BY position, created_at"
    ))?;
    let styles = statement
        .query_map([], Style::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if styles.is_empty() {
        return Ok(Snapshot {
            styles: vec![Style::fallback()],
            apps: Vec::new(),
            app_names: HashMap::new(),
            teaching,
        });
    }
    let mut statement =
        conn.prepare("SELECT bundle_id, style_id, app_name FROM app_styles ORDER BY rowid")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let app_names = rows
        .iter()
        .filter_map(|(bundle, _, name)| {
            let name = name.as_ref().filter(|name| !name.is_empty())?;
            Some((bundle.clone(), name.clone()))
        })
        .collect();
    let apps = rows
        .into_iter()
        .map(|(bundle, style, _)| (bundle, style))
        .collect();
    Ok(Snapshot {
        styles,
        apps,
        app_names,
        teaching,
    })
}

// -- suggestions --------------------------------------------------------------

/// Each app's App Store category, from its newest capture that has one, in
/// bundle id order.
fn app_categories(conn: &Connection) -> rusqlite::Result<Vec<(String, Option<String>)>> {
    let mut statement = conn.prepare(
        "SELECT app_bundle_id, app_category, MAX(created_at) FROM captures \
         WHERE app_bundle_id IS NOT NULL GROUP BY app_bundle_id, app_category \
         ORDER BY app_bundle_id, app_category",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut categories: Vec<(String, Option<String>)> = Vec::new();
    let mut latest: HashMap<String, Option<String>> = HashMap::new();
    for (bundle, category, when) in rows {
        if !categories.iter().any(|(seen, _)| *seen == bundle) {
            categories.push((bundle.clone(), None));
        }
        let Some(category) = category.filter(|category| !category.is_empty()) else {
            continue;
        };
        let newer = match latest.get(&bundle) {
            None => true,
            Some(before) => when.is_some() && when > *before,
        };
        if newer {
            if let Some(entry) = categories.iter_mut().find(|(seen, _)| *seen == bundle) {
                entry.1 = Some(category);
            }
            latest.insert(bundle, when);
        }
    }
    Ok(categories)
}

/// A style for every app not assigned yet: the one most assigned apps of its
/// App Store category use. An app with no category, or whose category no
/// assigned app has, gets the default; ties go to the style listed first.
pub fn suggest_styles(
    categories: &[(String, Option<String>)],
    styles: &Snapshot,
) -> HashMap<String, String> {
    let category_of: HashMap<&str, Option<&str>> = categories
        .iter()
        .map(|(bundle, category)| (bundle.as_str(), category.as_deref()))
        .collect();
    let mut votes: HashMap<&str, HashMap<&str, usize>> = HashMap::new();
    for (bundle, style_id) in &styles.apps {
        let category = category_of.get(bundle.as_str()).copied().flatten();
        if let Some(category) = category.filter(|c| !c.is_empty()) {
            if styles.get(Some(style_id)).is_some() {
                *votes
                    .entry(category)
                    .or_default()
                    .entry(style_id.as_str())
                    .or_default() += 1;
            }
        }
    }
    let order: HashMap<&str, usize> = styles
        .styles
        .iter()
        .enumerate()
        .map(|(index, style)| (style.id.as_str(), index))
        .collect();
    let mut suggested = HashMap::new();
    for (bundle, category) in categories {
        if styles.assigned(bundle).is_some() {
            continue;
        }
        let counted = votes.get(category.as_deref().unwrap_or(""));
        let style = counted
            .and_then(|counted| {
                counted
                    .iter()
                    .min_by_key(|(style, count)| (std::cmp::Reverse(**count), order[**style]))
                    .map(|(style, _)| style.to_string())
            })
            .unwrap_or_else(|| styles.default_style().id.clone());
        suggested.insert(bundle.clone(), style);
    }
    suggested
}

// -- the listing --------------------------------------------------------------

/// An app on the Writing style page: the style it uses and whether the user
/// chose it. `corrections` counts its captures whose corrections teach that
/// style; `suggested_style_id` is set until the user chooses.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StyledApp {
    pub bundle_id: String,
    pub name: Option<String>,
    pub style_id: String,
    pub confirmed: bool,
    pub count: i64,
    pub corrections: i64,
    pub suggested_style_id: Option<String>,
}

pub struct Listing {
    pub styles: Vec<Style>,
    pub apps: Vec<StyledApp>,
}

/// One app's captures: its newest name, how many, and when the last was.
struct Captured {
    bundle_id: String,
    name: Option<String>,
    count: i64,
    latest: Option<String>,
}

/// How many captures each app has, most first; an app's name is the one on
/// its newest capture (`services/captures.py`, `list_capture_apps`).
fn captured_apps(conn: &Connection) -> rusqlite::Result<Vec<Captured>> {
    let mut statement = conn.prepare(
        "SELECT app_bundle_id, app_name, COUNT(id), MAX(created_at) FROM captures \
         WHERE app_bundle_id IS NOT NULL GROUP BY app_bundle_id, app_name \
         ORDER BY app_bundle_id, app_name",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut apps: Vec<Captured> = Vec::new();
    for (bundle_id, name, count, latest) in rows {
        let at = match apps.iter().position(|app| app.bundle_id == bundle_id) {
            Some(at) => at,
            None => {
                apps.push(Captured {
                    bundle_id,
                    name: None,
                    count: 0,
                    latest: None,
                });
                apps.len() - 1
            }
        };
        let app = &mut apps[at];
        let newer = latest.is_some() && latest > app.latest;
        if name.as_ref().is_some_and(|name| !name.is_empty()) && (app.latest.is_none() || newer) {
            app.name = name;
        }
        app.count += count;
        if latest.is_some() && (app.latest.is_none() || newer) {
            app.latest = latest;
        }
    }
    apps.sort_by_key(|app| (std::cmp::Reverse(app.count), sort_name(app)));
    Ok(apps)
}

fn sort_name(app: &Captured) -> String {
    py::lower(app.name.as_deref().unwrap_or(&app.bundle_id))
}

/// Per app, the captures whose corrections teach the style the app is in
/// now. A correction whose capture history retention deleted keeps its own
/// copy of the app and style.
fn app_corrections(conn: &Connection, styles: &Snapshot) -> rusqlite::Result<HashMap<String, i64>> {
    let mut statement = conn.prepare(&format!(
        "SELECT DISTINCT f.capture_id, COALESCE(c.app_bundle_id, f.app_bundle_id), \
         COALESCE(c.teaches_style_id, f.teaches_style_id) \
         FROM capture_feedback f LEFT OUTER JOIN captures c ON c.id = f.capture_id \
         WHERE f.target = 'refined' AND f.source IN {TEACHING_SOURCES}"
    ))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts = HashMap::new();
    for (bundle, teaches) in rows {
        let Some(bundle) = bundle else { continue };
        let taught = styles.correction_style(Some(&bundle), teaches.as_deref());
        if taught == styles.for_app(Some(&bundle)).id {
            *counts.entry(bundle).or_default() += 1;
        }
    }
    Ok(counts)
}

/// Every style, and every app with captures or an assignment, with the
/// style it uses.
pub fn listing(conn: &Connection, teaching: Option<String>) -> rusqlite::Result<Listing> {
    let styles = load(conn, teaching)?;
    let suggested = suggest_styles(&app_categories(conn)?, &styles);
    let corrections = app_corrections(conn, &styles)?;
    let captured = captured_apps(conn)?;
    let counted: HashSet<String> = captured.iter().map(|app| app.bundle_id.clone()).collect();
    let mut apps: Vec<StyledApp> = captured
        .into_iter()
        .map(|app| StyledApp {
            name: app
                .name
                .or_else(|| styles.app_names.get(&app.bundle_id).cloned()),
            style_id: styles.for_app(Some(&app.bundle_id)).id.clone(),
            confirmed: styles.assigned(&app.bundle_id).is_some(),
            count: app.count,
            corrections: corrections.get(&app.bundle_id).copied().unwrap_or(0),
            suggested_style_id: suggested.get(&app.bundle_id).cloned(),
            bundle_id: app.bundle_id,
        })
        .collect();
    apps.extend(
        styles
            .apps
            .iter()
            .filter(|(bundle, _)| !counted.contains(bundle))
            .map(|(bundle, _)| StyledApp {
                bundle_id: bundle.clone(),
                name: styles.app_names.get(bundle).cloned(),
                style_id: styles.for_app(Some(bundle)).id.clone(),
                confirmed: true,
                count: 0,
                corrections: corrections.get(bundle).copied().unwrap_or(0),
                suggested_style_id: None,
            }),
    );
    Ok(Listing {
        styles: styles.styles,
        apps,
    })
}

/// One app in the Captures app list (`CaptureAppCount`): how many captures
/// went to it, the style its dictation uses, whether the user chose it, and
/// the style suggested for it until they do.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaptureApp {
    pub app_bundle_id: String,
    pub app_name: Option<String>,
    pub count: i64,
    #[serde(serialize_with = "crate::captures::iso")]
    pub last_captured_at: Option<NaiveDateTime>,
    pub style_id: String,
    pub confirmed: bool,
    pub suggested_style_id: Option<String>,
}

/// `GET /captures/apps` (`CaptureAppsResponse`). `unknown_count` is the
/// captures with no app recorded: uploads, and dictation from before the
/// target app was saved.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaptureApps {
    pub total: i64,
    pub unknown_count: i64,
    pub apps: Vec<CaptureApp>,
}

/// How many captures each app has, most first, counted over every row
/// rather than the page the list loads. Unlike [`listing`], only apps with
/// captures, named only from them (`services/captures.py`,
/// `list_capture_apps`).
pub fn capture_apps(conn: &Connection, teaching: Option<String>) -> rusqlite::Result<CaptureApps> {
    let styles = load(conn, teaching)?;
    let suggested = suggest_styles(&app_categories(conn)?, &styles);
    let unknown_count: i64 = conn.query_row(
        "SELECT COUNT(id) FROM captures WHERE app_bundle_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    let apps: Vec<CaptureApp> = captured_apps(conn)?
        .into_iter()
        .map(|app| CaptureApp {
            app_name: app.name,
            count: app.count,
            last_captured_at: parse_stored(app.latest),
            style_id: styles.for_app(Some(&app.bundle_id)).id.clone(),
            confirmed: styles.assigned(&app.bundle_id).is_some(),
            suggested_style_id: suggested.get(&app.bundle_id).cloned(),
            app_bundle_id: app.bundle_id,
        })
        .collect();
    Ok(CaptureApps {
        total: apps.iter().map(|app| app.count).sum::<i64>() + unknown_count,
        unknown_count,
        apps,
    })
}

// -- changes ------------------------------------------------------------------

/// Why a change wasn't made.
#[derive(Clone, Debug, PartialEq)]
pub enum Refused {
    /// 400, with the message the user sees.
    Invalid(String),
    /// 404: no such style.
    NotFound,
}

fn invalid<T>(message: impl Into<String>) -> rusqlite::Result<Result<T, Refused>> {
    Ok(Err(Refused::Invalid(message.into())))
}

/// Rows changed in one transaction, by sync kind, recorded as one batch.
#[derive(Default)]
struct Changes(Vec<(&'static str, String, bool)>);

impl Changes {
    fn changed(&mut self, kind: &'static str, id: &str) {
        self.0.push((kind, id.to_string(), false));
    }

    fn deleted(&mut self, kind: &'static str, id: &str) {
        self.0.push((kind, id.to_string(), true));
    }

    fn record(self, tx: &Transaction, sync: Sync, at: &str) -> rusqlite::Result<()> {
        if !sync.standing_in {
            return Ok(());
        }
        let mut statement = tx.prepare(
            "INSERT INTO sync_changes (kind, ref_id, deleted, changed_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (kind, ref_id) DO UPDATE SET deleted = excluded.deleted, changed_at = excluded.changed_at",
        )?;
        for (kind, id, deleted) in self.0 {
            statement.execute(params![kind, id, deleted, at])?;
        }
        Ok(())
    }
}

/// Runs `edit` in a transaction that holds the write lock from its first
/// read, committed only when the edit is accepted.
fn write<T>(
    conn: &mut Connection,
    sync: Sync,
    edit: impl FnOnce(&Transaction, &mut Changes, &str) -> rusqlite::Result<Result<T, Refused>>,
) -> rusqlite::Result<Result<T, Refused>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let at = now().format(STORED).to_string();
    let mut changes = Changes::default();
    let done = edit(&tx, &mut changes, &at)?;
    if done.is_ok() {
        changes.record(&tx, sync, &at)?;
        tx.commit()?;
    }
    Ok(done)
}

fn get_style(tx: &Transaction, style_id: &str) -> rusqlite::Result<Option<Style>> {
    tx.query_row(
        &format!("SELECT {STYLE_COLUMNS} FROM writing_styles WHERE id = ?1"),
        [style_id],
        Style::read,
    )
    .optional()
}

/// A style name with its whitespace collapsed, cut to 40 characters, and
/// unlike any other style's.
fn clean_name(
    tx: &Transaction,
    name: &str,
    exclude: Option<&str>,
) -> rusqlite::Result<Result<String, Refused>> {
    if py::strip(name).is_empty() {
        return invalid("Give the style a name");
    }
    let cleaned = py::head(&py::split(name).join(" "), MAX_NAME_CHARS).to_string();
    let mut statement = tx.prepare("SELECT name FROM writing_styles WHERE id != ?1")?;
    let taken = statement
        .query_map([exclude.unwrap_or("")], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let folded = py::casefold(&cleaned);
    if taken.iter().any(|name| py::casefold(name) == folded) {
        return invalid(format!("There's already a style called {cleaned}"));
    }
    Ok(Ok(cleaned))
}

/// The user's line breaks kept, trailing spaces on each line trimmed.
fn clean_description(text: &str) -> Result<String, Refused> {
    let cleaned = py::splitlines(py::strip(text))
        .into_iter()
        .map(py::rstrip)
        .collect::<Vec<_>>()
        .join("\n");
    if py::len(&cleaned) > MAX_DESCRIPTION_CHARS {
        return Err(Refused::Invalid(format!(
            "Keep the description under {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    Ok(cleaned)
}

/// Create the Personal style on first run, with the global settings in it,
/// and repair a database left without exactly one default. Idempotent.
pub fn ensure(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let defaults: Vec<(String, bool)> = {
        let mut statement =
            tx.prepare("SELECT id, is_default FROM writing_styles ORDER BY position")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get::<_, Option<bool>>(1)? == Some(true)))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if defaults.is_empty() {
        let saved: Option<(Option<String>, Option<bool>)> = tx
            .query_row(
                "SELECT punctuation_style, preserve_technical FROM capture_settings LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (punctuation, preserve) = saved.unwrap_or_default();
        // Dictation keeps working exactly as it did before styles.
        tx.execute(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style, \
             capitalize_first, smart_cleanup, preserve_technical, description, created_at, updated_at) \
             VALUES (?1, ?2, 0, 1, ?3, 1, 1, ?4, NULL, ?5, ?5)",
            params![
                MIGRATED_STYLE,
                MIGRATED_NAME,
                punctuation.filter(|style| !style.is_empty()).unwrap_or_else(|| "standard".into()),
                preserve.unwrap_or(true),
                now().format(STORED).to_string()
            ],
        )?;
    } else if defaults.iter().filter(|(_, default)| *default).count() != 1 {
        let keep = defaults
            .iter()
            .find(|(_, default)| *default)
            .unwrap_or(&defaults[0])
            .0
            .clone();
        tx.execute("UPDATE writing_styles SET is_default = (id = ?1)", [keep])?;
    }
    tx.commit()
}

/// A new style, starting from the default's settings with nothing learned.
pub fn create_style(
    conn: &mut Connection,
    name: &str,
    sync: Sync,
) -> rusqlite::Result<Result<Style, Refused>> {
    write(conn, sync, |tx, changes, at| {
        let count: i64 =
            tx.query_row("SELECT COUNT(*) FROM writing_styles", [], |row| row.get(0))?;
        if count >= MAX_STYLES {
            return invalid(format!(
                "Kass keeps at most {MAX_STYLES} styles, one cached prompt each"
            ));
        }
        let base = load(tx, None)?.default_style().clone();
        let last: Option<i64> = tx
            .query_row(
                "SELECT position FROM writing_styles ORDER BY position DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let name = match clean_name(tx, name, None)? {
            Ok(name) => name,
            Err(refused) => return Ok(Err(refused)),
        };
        let style = Style {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name,
            position: last.map_or(0, |last| last + 1),
            is_default: false,
            punctuation_style: base.punctuation_style,
            preserve_technical: base.preserve_technical,
            description: String::new(),
        };
        tx.execute(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style, \
             capitalize_first, smart_cleanup, preserve_technical, description, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 0, ?4, 1, 1, ?5, NULL, ?6, ?6)",
            params![
                style.id,
                style.name,
                style.position,
                style.punctuation_style,
                style.preserve_technical,
                at
            ],
        )?;
        changes.changed("style", &style.id);
        Ok(Ok(style))
    })
}

/// What a style update asks for; None leaves a setting as it is.
#[derive(Clone, Debug, Default)]
pub struct StylePatch {
    pub name: Option<String>,
    pub punctuation_style: Option<String>,
    pub preserve_technical: Option<bool>,
    pub description: Option<String>,
    /// Only true does anything: another style becomes the default by being
    /// made it.
    pub is_default: Option<bool>,
}

/// Rename a style, change its settings or description, or make it the
/// default.
pub fn update_style(
    conn: &mut Connection,
    style_id: &str,
    patch: StylePatch,
    sync: Sync,
) -> rusqlite::Result<Result<Style, Refused>> {
    write(conn, sync, |tx, changes, at| {
        let Some(mut style) = get_style(tx, style_id)? else {
            return Ok(Err(Refused::NotFound));
        };
        let (stored_punctuation, stored_description): (String, Option<String>) = tx.query_row(
            "SELECT punctuation_style, description FROM writing_styles WHERE id = ?1",
            [style_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut punctuation = stored_punctuation.clone();
        let mut description = stored_description.clone();
        let before = style.clone();
        if let Some(name) = &patch.name {
            match clean_name(tx, name, Some(style_id))? {
                Ok(name) => style.name = name,
                Err(refused) => return Ok(Err(refused)),
            }
        }
        if let Some(chosen) = patch.punctuation_style {
            if !PUNCTUATION_STYLES.contains(&chosen.as_str()) {
                return invalid("Unknown punctuation style");
            }
            style.punctuation_style = chosen.clone();
            punctuation = chosen;
        }
        if let Some(preserve) = patch.preserve_technical {
            style.preserve_technical = preserve;
        }
        if let Some(text) = &patch.description {
            match clean_description(text) {
                Ok(cleaned) => {
                    style.description = cleaned.clone();
                    description = Some(cleaned);
                }
                Err(refused) => return Ok(Err(refused)),
            }
        }
        if patch.is_default == Some(true) {
            let mut statement =
                tx.prepare("SELECT id FROM writing_styles WHERE id != ?1 AND is_default != 0")?;
            let others = statement
                .query_map([style_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for other in others {
                tx.execute(
                    "UPDATE writing_styles SET is_default = 0, updated_at = ?2 WHERE id = ?1",
                    params![other, at],
                )?;
                changes.changed("style", &other);
            }
            style.is_default = true;
        }
        let edited = style.name != before.name
            || punctuation != stored_punctuation
            || style.preserve_technical != before.preserve_technical
            || description != stored_description
            || style.is_default != before.is_default;
        if edited {
            tx.execute(
                "UPDATE writing_styles SET name = ?2, punctuation_style = ?3, preserve_technical = ?4, \
                 description = ?5, is_default = ?6, updated_at = ?7 WHERE id = ?1",
                params![
                    style_id,
                    style.name,
                    punctuation,
                    style.preserve_technical,
                    description,
                    style.is_default,
                    at
                ],
            )?;
            changes.changed("style", style_id);
        }
        Ok(Ok(style))
    })
}

/// Delete a style; its apps go back to the default. Returns the default's
/// id, which inherits the style's dictionary entries (the caller moves them,
/// `dictionary::store::move_style`) and what the worker regroups.
pub fn delete_style(
    conn: &mut Connection,
    style_id: &str,
    sync: Sync,
) -> rusqlite::Result<Result<String, Refused>> {
    write(conn, sync, |tx, changes, _| {
        let Some(style) = get_style(tx, style_id)? else {
            return Ok(Err(Refused::NotFound));
        };
        if style.is_default {
            return invalid("Make another style the default before deleting this one");
        }
        let mut statement = tx.prepare("SELECT bundle_id FROM app_styles WHERE style_id = ?1")?;
        let apps = statement
            .query_map([style_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tx.execute("DELETE FROM app_styles WHERE style_id = ?1", [style_id])?;
        for bundle in &apps {
            changes.deleted("app_style", bundle);
        }
        tx.execute("DELETE FROM writing_styles WHERE id = ?1", [style_id])?;
        changes.deleted("style", style_id);
        Ok(Ok(load(tx, None)?.default_style().id.clone()))
    })
}

/// What happens to the corrections an app's captures teach its current
/// style when it moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Corrections {
    /// They move to the new style with the app.
    Bring,
    /// They keep teaching the current one.
    Leave,
}

/// Put `bundle_id` in `style_id`; this also confirms a new app's style.
/// `teaching` is the style being taught (`teach`).
pub fn assign_app(
    conn: &mut Connection,
    bundle_id: &str,
    app_name: Option<&str>,
    style_id: &str,
    corrections: Corrections,
    teaching: Option<String>,
    sync: Sync,
) -> rusqlite::Result<Result<(), Refused>> {
    write(conn, sync, |tx, changes, at| {
        if get_style(tx, style_id)?.is_none() {
            return Ok(Err(Refused::NotFound));
        }
        let current = load(tx, teaching)?.for_app(Some(bundle_id)).id.clone();
        if current != style_id {
            // Corrections whose capture history retention deleted carry
            // their own app and style.
            for (table, kind) in [("captures", "capture"), ("capture_feedback", "feedback")] {
                let (filter, args, to) = match corrections {
                    Corrections::Leave => (
                        "teaches_style_id IS NULL",
                        vec![bundle_id],
                        Some(current.as_str()),
                    ),
                    Corrections::Bring => (
                        "teaches_style_id = ?2",
                        vec![bundle_id, current.as_str()],
                        None,
                    ),
                };
                let select =
                    format!("SELECT id FROM {table} WHERE app_bundle_id = ?1 AND {filter}");
                let ids = tx
                    .prepare(&select)?
                    .query_map(rusqlite::params_from_iter(args), |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let update = format!("UPDATE {table} SET teaches_style_id = ?2 WHERE id = ?1");
                for id in ids {
                    tx.execute(&update, params![id, to])?;
                    changes.changed(kind, &id);
                }
            }
        }
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT style_id, app_name FROM app_styles WHERE bundle_id = ?1",
                [bundle_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                tx.execute(
                    "INSERT INTO app_styles (bundle_id, app_name, style_id, updated_at) VALUES (?1, ?2, ?3, ?4)",
                    params![bundle_id, app_name, style_id, at],
                )?;
                changes.changed("app_style", bundle_id);
            }
            Some((was, name)) => {
                let named = app_name
                    .map(str::to_string)
                    .filter(|n| !n.is_empty())
                    .or(name.clone());
                if was != style_id || named != name {
                    tx.execute(
                        "UPDATE app_styles SET style_id = ?2, app_name = ?3, updated_at = ?4 WHERE bundle_id = ?1",
                        params![bundle_id, style_id, named, at],
                    )?;
                    changes.changed("app_style", bundle_id);
                }
            }
        }
        Ok(Ok(()))
    })
}

/// Keep the style each listed app uses now, so it stops being new. Returns
/// whether any app was added. `teaching` is the style being taught.
pub fn confirm_apps(
    conn: &mut Connection,
    apps: &[(String, Option<String>)],
    teaching: Option<String>,
    sync: Sync,
) -> rusqlite::Result<bool> {
    let added = write(conn, sync, |tx, changes, at| {
        let styles = load(tx, teaching)?;
        let mut added = false;
        for (bundle_id, app_name) in apps {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM app_styles WHERE bundle_id = ?1)",
                [bundle_id],
                |row| row.get(0),
            )?;
            if exists {
                continue;
            }
            let style = &styles.for_app(Some(bundle_id)).id;
            tx.execute(
                "INSERT INTO app_styles (bundle_id, app_name, style_id, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![bundle_id, app_name, style, at],
            )?;
            changes.changed("app_style", bundle_id);
            added = true;
        }
        Ok(Ok::<_, Refused>(added))
    })?;
    Ok(added.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../tests/kass_schema.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style, capitalize_first, \
             smart_cleanup, preserve_technical, description, created_at, updated_at) \
             VALUES ('personal', 'Personal', 0, 1, 'casual', 1, 1, 0, NULL, '2026-01-01 00:00:00.000000', NULL)",
            [],
        )
        .unwrap();
        conn
    }

    const OFF: Sync = Sync { standing_in: false };
    const ON: Sync = Sync { standing_in: true };

    fn capture(
        conn: &Connection,
        id: &str,
        bundle: &str,
        name: Option<&str>,
        category: Option<&str>,
        at: &str,
    ) {
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, app_bundle_id, app_name, \
             app_category, audio_deleted, created_at) VALUES (?1, '', 'dictation', '', ?2, ?3, ?4, 0, ?5)",
            params![id, bundle, name, category, at],
        )
        .unwrap();
    }

    fn feedback(conn: &Connection, id: &str, capture: &str, source: &str) {
        conn.execute(
            "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot, source, created_at) \
             VALUES (?1, ?2, 'refined', 'x', '', '{}', ?3, '2026-01-01 00:00:00.000000')",
            params![id, capture, source],
        )
        .unwrap();
    }

    fn changes(conn: &Connection) -> Vec<(String, String, bool)> {
        let mut statement = conn
            .prepare("SELECT kind, ref_id, deleted FROM sync_changes ORDER BY kind, ref_id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn empty_database_has_the_personal_style() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../tests/kass_schema.sql"))
            .unwrap();
        let styles = load(&conn, None).unwrap();
        assert_eq!(styles.styles, vec![Style::fallback()]);
        assert_eq!(styles.for_app(Some("any")).id, "personal");
    }

    #[test]
    fn new_style_copies_the_default_settings() {
        let mut conn = db();
        let chat = create_style(&mut conn, "  Chat   with\tfriends ", ON)
            .unwrap()
            .unwrap();
        assert_eq!(chat.name, "Chat with friends");
        assert_eq!(chat.position, 1);
        assert_eq!(chat.punctuation_style, "casual");
        assert!(!chat.preserve_technical);
        assert!(!chat.is_default);
        assert_eq!(
            changes(&conn),
            vec![("style".into(), chat.id.clone(), false)]
        );
        assert_eq!(
            create_style(&mut conn, "chat WITH friends", OFF).unwrap(),
            Err(Refused::Invalid(
                "There's already a style called chat WITH friends".into()
            ))
        );
        assert_eq!(
            create_style(&mut conn, "   ", OFF).unwrap(),
            Err(Refused::Invalid("Give the style a name".into()))
        );
        let long = "x".repeat(50);
        assert_eq!(
            create_style(&mut conn, &long, OFF)
                .unwrap()
                .unwrap()
                .name
                .len(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn styles_stop_at_six() {
        let mut conn = db();
        for n in 0..5 {
            create_style(&mut conn, &format!("Style {n}"), OFF)
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            create_style(&mut conn, "One more", OFF).unwrap(),
            Err(Refused::Invalid(
                "Kass keeps at most 6 styles, one cached prompt each".into()
            ))
        );
    }

    #[test]
    fn update_cleans_the_description_and_moves_the_default() {
        let mut conn = db();
        let chat = create_style(&mut conn, "Chat", OFF).unwrap().unwrap();
        let patch = StylePatch {
            description: Some("  Informal.   \nNo greetings.  \n".into()),
            is_default: Some(true),
            ..Default::default()
        };
        let updated = update_style(&mut conn, &chat.id, patch, ON)
            .unwrap()
            .unwrap();
        assert_eq!(updated.description, "Informal.\nNo greetings.");
        assert!(updated.is_default);
        let styles = load(&conn, None).unwrap();
        assert_eq!(styles.default_style().id, chat.id);
        assert!(!styles.get(Some("personal")).unwrap().is_default);
        assert_eq!(changes(&conn).len(), 2);

        let long = StylePatch {
            description: Some("x".repeat(601)),
            ..Default::default()
        };
        assert_eq!(
            update_style(&mut conn, &chat.id, long, OFF).unwrap(),
            Err(Refused::Invalid(
                "Keep the description under 600 characters".into()
            ))
        );
        let rename = StylePatch {
            name: Some("personal".into()),
            ..Default::default()
        };
        assert_eq!(
            update_style(&mut conn, &chat.id, rename, OFF).unwrap(),
            Err(Refused::Invalid(
                "There's already a style called personal".into()
            ))
        );
        let same = StylePatch {
            name: Some("CHAT".into()),
            ..Default::default()
        };
        assert_eq!(
            update_style(&mut conn, &chat.id, same, OFF)
                .unwrap()
                .unwrap()
                .name,
            "CHAT"
        );
        assert_eq!(
            update_style(&mut conn, "gone", StylePatch::default(), OFF).unwrap(),
            Err(Refused::NotFound)
        );
    }

    #[test]
    fn unchanged_update_records_nothing() {
        let mut conn = db();
        let patch = StylePatch {
            name: Some("Personal".into()),
            ..Default::default()
        };
        update_style(&mut conn, "personal", patch, ON)
            .unwrap()
            .unwrap();
        assert!(changes(&conn).is_empty());
    }

    #[test]
    fn deleting_a_style_frees_its_apps() {
        let mut conn = db();
        let chat = create_style(&mut conn, "Chat", OFF).unwrap().unwrap();
        assign_app(
            &mut conn,
            "net.whatsapp",
            Some("WhatsApp"),
            &chat.id,
            Corrections::Bring,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            delete_style(&mut conn, "personal", OFF).unwrap(),
            Err(Refused::Invalid(
                "Make another style the default before deleting this one".into()
            ))
        );
        assert_eq!(
            delete_style(&mut conn, &chat.id, ON).unwrap().unwrap(),
            "personal"
        );
        let styles = load(&conn, None).unwrap();
        assert_eq!(styles.styles.len(), 1);
        assert!(styles.apps.is_empty());
        assert_eq!(
            changes(&conn),
            vec![
                ("app_style".into(), "net.whatsapp".into(), true),
                ("style".into(), chat.id, true)
            ]
        );
        assert_eq!(
            delete_style(&mut conn, "gone", OFF).unwrap(),
            Err(Refused::NotFound)
        );
    }

    #[test]
    fn moving_an_app_brings_or_leaves_its_corrections() {
        let mut conn = db();
        let chat = create_style(&mut conn, "Chat", OFF).unwrap().unwrap();
        capture(
            &conn,
            "c1",
            "net.whatsapp",
            Some("WhatsApp"),
            None,
            "2026-01-01 00:00:00.000000",
        );
        feedback(&conn, "f1", "c1", "manual");
        let listed = listing(&conn, None).unwrap();
        assert_eq!(listed.apps[0].corrections, 1);
        assert_eq!(
            listed.apps[0].suggested_style_id.as_deref(),
            Some("personal")
        );
        assert!(!listed.apps[0].confirmed);

        assign_app(
            &mut conn,
            "net.whatsapp",
            None,
            &chat.id,
            Corrections::Leave,
            None,
            ON,
        )
        .unwrap()
        .unwrap();
        let teaches: Option<String> = conn
            .query_row(
                "SELECT teaches_style_id FROM captures WHERE id = 'c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(teaches.as_deref(), Some("personal"));
        let listed = listing(&conn, None).unwrap();
        assert_eq!(listed.apps[0].style_id, chat.id);
        assert_eq!(listed.apps[0].corrections, 0);
        assert!(listed.apps[0].confirmed);
        assert_eq!(listed.apps[0].suggested_style_id, None);
        assert_eq!(
            changes(&conn),
            vec![
                ("app_style".into(), "net.whatsapp".into(), false),
                ("capture".into(), "c1".into(), false)
            ]
        );

        // Corrections that teach the style an app leaves come along with it.
        capture(
            &conn,
            "c2",
            "net.whatsapp",
            None,
            None,
            "2026-01-02 00:00:00.000000",
        );
        feedback(&conn, "f2", "c2", "voice_fix");
        assert_eq!(listing(&conn, None).unwrap().apps[0].corrections, 1);
        assign_app(
            &mut conn,
            "net.whatsapp",
            None,
            "personal",
            Corrections::Leave,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();
        assign_app(
            &mut conn,
            "net.whatsapp",
            None,
            &chat.id,
            Corrections::Bring,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();
        let teaches = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT teaches_style_id FROM captures WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };
        // c1 taught Personal, the style left: it follows the app again.
        assert_eq!(teaches("c1"), None);
        // c2 was left teaching Chat, which the app is back in.
        assert_eq!(teaches("c2").as_deref(), Some(chat.id.as_str()));
        assert_eq!(listing(&conn, None).unwrap().apps[0].corrections, 2);
        assert_eq!(
            assign_app(
                &mut conn,
                "net.whatsapp",
                None,
                "gone",
                Corrections::Bring,
                None,
                OFF
            )
            .unwrap(),
            Err(Refused::NotFound)
        );
    }

    #[test]
    fn listing_counts_captures_and_keeps_the_newest_name() {
        let conn = db();
        capture(
            &conn,
            "a1",
            "com.a",
            Some("Old A"),
            None,
            "2026-01-01 00:00:00.000000",
        );
        capture(
            &conn,
            "a2",
            "com.a",
            Some("New A"),
            None,
            "2026-01-02 00:00:00.000000",
        );
        capture(
            &conn,
            "b1",
            "com.b",
            None,
            None,
            "2026-01-03 00:00:00.000000",
        );
        capture(
            &conn,
            "b2",
            "com.b",
            None,
            None,
            "2026-01-03 00:00:00.000000",
        );
        capture(
            &conn,
            "b3",
            "com.b",
            None,
            None,
            "2026-01-03 00:00:00.000000",
        );
        let apps = listing(&conn, None).unwrap().apps;
        let summary: Vec<_> = apps
            .iter()
            .map(|app| (app.bundle_id.as_str(), app.name.as_deref(), app.count))
            .collect();
        assert_eq!(
            summary,
            vec![("com.b", None, 3), ("com.a", Some("New A"), 2)]
        );
    }

    #[test]
    fn assigned_apps_without_captures_are_listed() {
        let mut conn = db();
        assign_app(
            &mut conn,
            "com.notes",
            Some("Notes"),
            "personal",
            Corrections::Bring,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();
        let apps = listing(&conn, None).unwrap().apps;
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name.as_deref(), Some("Notes"));
        assert_eq!(apps[0].count, 0);
        assert!(apps[0].confirmed);
    }

    #[test]
    fn suggestions_follow_the_category() {
        let mut conn = db();
        let chat = create_style(&mut conn, "Chat", OFF).unwrap().unwrap();
        capture(
            &conn,
            "1",
            "net.whatsapp",
            None,
            Some("social"),
            "2026-01-01 00:00:00.000000",
        );
        capture(
            &conn,
            "2",
            "com.signal",
            None,
            Some("social"),
            "2026-01-01 00:00:00.000000",
        );
        capture(
            &conn,
            "3",
            "com.mail",
            None,
            None,
            "2026-01-01 00:00:00.000000",
        );
        assign_app(
            &mut conn,
            "net.whatsapp",
            None,
            &chat.id,
            Corrections::Bring,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();
        let apps = listing(&conn, None).unwrap().apps;
        let suggested = |bundle: &str| {
            apps.iter()
                .find(|app| app.bundle_id == bundle)
                .unwrap()
                .suggested_style_id
                .clone()
        };
        assert_eq!(suggested("com.signal"), Some(chat.id.clone()));
        assert_eq!(suggested("com.mail").as_deref(), Some("personal"));
        assert_eq!(suggested("net.whatsapp"), None);
    }

    #[test]
    fn newest_category_wins() {
        let conn = db();
        capture(
            &conn,
            "1",
            "com.a",
            None,
            Some("old"),
            "2026-01-01 00:00:00.000000",
        );
        capture(
            &conn,
            "2",
            "com.a",
            None,
            Some("new"),
            "2026-01-02 00:00:00.000000",
        );
        capture(
            &conn,
            "3",
            "com.a",
            None,
            None,
            "2026-01-03 00:00:00.000000",
        );
        assert_eq!(
            app_categories(&conn).unwrap(),
            vec![("com.a".into(), Some("new".into()))]
        );
    }

    #[test]
    fn kass_follows_the_style_being_taught() {
        let mut conn = db();
        let chat = create_style(&mut conn, "Chat", OFF).unwrap().unwrap();
        let styles = load(&conn, Some(chat.id.clone())).unwrap();
        assert_eq!(styles.for_app(Some(KASS_BUNDLE)).id, chat.id);
        assert_eq!(styles.for_app(Some("com.other")).id, "personal");
        let styles = load(&conn, Some("gone".into())).unwrap();
        assert_eq!(styles.for_app(Some(KASS_BUNDLE)).id, "personal");
    }

    #[test]
    fn capture_apps_count_every_capture_and_name_apps_from_the_newest() {
        let mut conn = db();
        let chat = create_style(&mut conn, "Chat", OFF).unwrap().unwrap();
        capture(
            &conn,
            "1",
            "com.mail",
            Some("Post"),
            Some("productivity"),
            "2026-01-01 00:00:00.000000",
        );
        capture(
            &conn,
            "2",
            "com.mail",
            Some("Mail"),
            None,
            "2026-01-03 00:00:00.000000",
        );
        capture(
            &conn,
            "3",
            "com.mail",
            Some("Mail"),
            None,
            "2026-01-04 00:00:00.500000",
        );
        capture(
            &conn,
            "4",
            "net.whatsapp",
            Some("WhatsApp"),
            Some("social"),
            "2026-01-02 00:00:00.000000",
        );
        capture(
            &conn,
            "5",
            "com.signal",
            Some("Signal"),
            Some("social"),
            "2026-01-02 00:00:00.000000",
        );
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, transcript_raw, audio_deleted, created_at) \
             VALUES ('6', '', 'upload', '', 0, '2026-01-05 00:00:00.000000')",
            [],
        )
        .unwrap();
        assign_app(
            &mut conn,
            "net.whatsapp",
            None,
            &chat.id,
            Corrections::Bring,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();
        // Assigned without captures: on the Writing style page, not here.
        assign_app(
            &mut conn,
            "com.notes",
            Some("Notes"),
            &chat.id,
            Corrections::Bring,
            None,
            OFF,
        )
        .unwrap()
        .unwrap();

        let listed = capture_apps(&conn, None).unwrap();
        assert_eq!((listed.total, listed.unknown_count), (6, 1));
        let at = |text: &str| parse_stored(Some(text.into()));
        assert_eq!(
            listed.apps,
            vec![
                CaptureApp {
                    app_bundle_id: "com.mail".into(),
                    app_name: Some("Mail".into()),
                    count: 3,
                    last_captured_at: at("2026-01-04 00:00:00.500000"),
                    style_id: "personal".into(),
                    confirmed: false,
                    suggested_style_id: Some("personal".into()),
                },
                CaptureApp {
                    app_bundle_id: "com.signal".into(),
                    app_name: Some("Signal".into()),
                    count: 1,
                    last_captured_at: at("2026-01-02 00:00:00.000000"),
                    style_id: "personal".into(),
                    confirmed: false,
                    suggested_style_id: Some(chat.id.clone()),
                },
                CaptureApp {
                    app_bundle_id: "net.whatsapp".into(),
                    app_name: Some("WhatsApp".into()),
                    count: 1,
                    last_captured_at: at("2026-01-02 00:00:00.000000"),
                    style_id: chat.id.clone(),
                    confirmed: true,
                    suggested_style_id: None,
                },
            ]
        );
    }

    #[test]
    fn confirming_keeps_the_current_style() {
        let mut conn = db();
        let apps = vec![("com.a".to_string(), Some("A".to_string()))];
        assert!(confirm_apps(&mut conn, &apps, None, ON).unwrap());
        assert!(!confirm_apps(&mut conn, &apps, None, ON).unwrap());
        let styles = load(&conn, None).unwrap();
        assert_eq!(styles.assigned("com.a"), Some("personal"));
        assert_eq!(styles.app_names.get("com.a").map(String::as_str), Some("A"));
        assert_eq!(
            changes(&conn),
            vec![("app_style".into(), "com.a".into(), false)]
        );
    }
}
