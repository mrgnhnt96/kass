//! Usage stats for the Captures summary card and the Insights tab
//! (`GET /captures/stats`). Python: `backend/services/usage_stats.py`.
//!
//! Everything is counted from the captures table with two queries: one sums
//! words, captures, speaking time and fixes per local hour and app, and one
//! reads each capture's length and raw word count for the medians. Command
//! captures are left out, since they rewrite a selection rather than dictate.
//! Captures history retention deleted count from their `retired_captures`
//! rows (docs/plans/HISTORY_RETENTION.md), so deleting history never lowers
//! a total.
//!
//! Days and hours are the Mac's local time. `created_at` is naive UTC, and
//! SQLite's `localtime` and `utc` modifiers convert it with the same zone
//! rules as the rest of the Mac, daylight saving included.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};
use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::captures::store::Apps;

/// Typing speed that time saved is measured against.
pub const TYPING_WPM: i64 = 40;
/// Shorter captures are mostly the start and stop of the recording, so their
/// pace is noise.
const MIN_PACE_MS: i64 = 2000;
/// "How long you talk" bins: under 3 s, 3-6 s, 6-10 s, 10-20 s, 20-40 s, 40 s
/// and up.
pub const LENGTH_BIN_EDGES_S: [i64; 5] = [3, 6, 10, 20, 40];

const WORDS_FN: &str = "vb_word_count";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Today,
    Week,
    Month,
    All,
}

impl Period {
    pub const NAMES: [&'static str; 4] = ["today", "7d", "30d", "all"];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "today" => Some(Period::Today),
            "7d" => Some(Period::Week),
            "30d" => Some(Period::Month),
            "all" => Some(Period::All),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Period::Today => "today",
            Period::Week => "7d",
            Period::Month => "30d",
            Period::All => "all",
        }
    }

    fn days(self) -> Option<i64> {
        match self {
            Period::Today => Some(1),
            Period::Week => Some(7),
            Period::Month => Some(30),
            Period::All => None,
        }
    }
}

fn matches(scope: &Apps, app: &Option<String>) -> bool {
    match scope {
        Apps::All => true,
        Apps::Unknown => app.is_none(),
        Apps::One(bundle_id) => app.as_deref() == Some(bundle_id),
    }
}

// -- the response (`UsageStatsResponse`) --------------------------------------

/// Words dictated on one local day.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Day {
    pub date: String,
    pub words: i64,
}

/// One period's dictation, for all apps or one. `pace_wpm` is the median
/// pace of captures of 2 s and longer (None when there are none).
/// `time_saved_ms` is the typing time the words would have taken, less the
/// time spent speaking them.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Totals {
    pub words: i64,
    pub captures: i64,
    pub speaking_ms: i64,
    pub pace_wpm: Option<i64>,
    pub time_saved_ms: i64,
    /// Captures with a saved correction: a lower bound on those that needed a fix.
    pub fixed_captures: i64,
    pub weekdays_dictated: i64,
    pub weekdays_in_period: i64,
    pub weekend_days: Vec<Day>,
    pub hours_dictated: i64,
}

/// One point of the words chart: an hour, a day or a week (`start`, local).
/// `words` is None for an hour still to come today. The previous period's
/// matching point is None for All time, which has nothing to compare with.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Point {
    pub start: String,
    pub words: Option<i64>,
    pub previous_start: Option<String>,
    pub previous_words: Option<i64>,
}

/// One app's share of the period. None is no app recorded.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct App {
    pub app_bundle_id: Option<String>,
    pub words: i64,
    pub captures: i64,
}

/// How long dictations run: counts per length bin, and the quartiles.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Lengths {
    pub bin_edges_s: Vec<i64>,
    pub counts: Vec<i64>,
    pub p25_ms: Option<i64>,
    pub median_ms: Option<i64>,
    pub p75_ms: Option<i64>,
}

/// One period's dictation. Dates are the Mac's local days. `current` and
/// everything after it follow the app filter; `all_apps` and `apps` are
/// always every app's, for comparison. `heatmap` is words by local weekday
/// (Monday first) and hour.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Stats {
    pub period: &'static str,
    pub bucket: &'static str,
    pub start: String,
    pub end: String,
    pub previous_start: Option<String>,
    pub previous_end: Option<String>,
    pub typing_wpm: i64,
    pub current: Totals,
    pub previous: Option<Totals>,
    pub all_apps: Totals,
    pub series: Vec<Point>,
    pub apps: Vec<App>,
    pub heatmap: Vec<Vec<i64>>,
    pub lengths: Lengths,
}

// -- reading ------------------------------------------------------------------

/// One local hour of one app's dictation.
#[derive(Clone)]
struct Slot {
    day: NaiveDate,
    hour: u32,
    app: Option<String>,
    captures: i64,
    words: i64,
    speaking_ms: i64,
    fixed: i64,
}

/// One capture's length and raw word count, for the medians.
#[derive(Clone)]
struct Recording {
    day: NaiveDate,
    app: Option<String>,
    duration_ms: i64,
    raw_words: i64,
}

struct Window {
    start: NaiveDate,
    /// Inclusive.
    end: NaiveDate,
    slots: Vec<Slot>,
    recordings: Vec<Recording>,
}

impl Window {
    fn new(start: NaiveDate, end: NaiveDate) -> Self {
        Window {
            start,
            end,
            slots: Vec::new(),
            recordings: Vec::new(),
        }
    }

    /// This window's share of everything loaded.
    fn split(mut self, slots: &[Slot], recordings: &[Recording]) -> Self {
        let inside = |day: NaiveDate| self.start <= day && day <= self.end;
        self.slots = slots.iter().filter(|s| inside(s.day)).cloned().collect();
        self.recordings = recordings
            .iter()
            .filter(|r| inside(r.day))
            .cloned()
            .collect();
        self
    }
}

/// Python's `str.split()`: runs of anything `str.isspace()` calls space,
/// which is Unicode's White_Space plus the four ASCII separators.
fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Words as the Captures inspector counts them: runs of non-space.
pub fn word_count(text: &str) -> i64 {
    text.split(is_python_space)
        .filter(|word| !word.is_empty())
        .count() as i64
}

/// Lets SQL count words, so transcripts never leave SQLite.
fn register_word_count(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        WORDS_FN,
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(match ctx.get_raw(0) {
                ValueRef::Text(text) => word_count(&String::from_utf8_lossy(text)),
                _ => 0,
            })
        },
    )
}

fn day(text: &str) -> rusqlite::Result<NaiveDate> {
    text.get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("not a date: {text}").into(),
            )
        })
}

fn iso(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

/// The two tables counted: captures still saved, and those history
/// retention deleted. A capture still in the table was never deleted;
/// it's counted once, from there.
struct Table {
    from: &'static str,
    count: &'static str,
    words: String,
    fixed: &'static str,
    raw_words: String,
}

fn tables() -> [Table; 2] {
    [
        Table {
            from: "captures c WHERE c.source != 'command'",
            count: "count(c.id)",
            words: format!(
                "{WORDS_FN}(coalesce(nullif(trim(c.transcript_refined), ''), c.transcript_raw))"
            ),
            fixed: "CASE WHEN EXISTS (SELECT * FROM capture_feedback f \
                    WHERE f.capture_id = c.id) THEN 1 ELSE 0 END",
            raw_words: format!("{WORDS_FN}(c.transcript_raw)"),
        },
        Table {
            from: "retired_captures c WHERE c.source != 'command' \
                   AND NOT EXISTS (SELECT * FROM captures WHERE captures.id = c.capture_id)",
            count: "count(c.capture_id)",
            words: "c.words".into(),
            fixed: "CASE WHEN c.fixed THEN 1 ELSE 0 END",
            raw_words: "c.raw_words".into(),
        },
    ]
}

/// Local midnight at the start of `?n`, as the naive UTC the tables store.
fn local_midnight(n: usize) -> String {
    format!("datetime(?{n}, 'utc')")
}

/// Every app's dictation from `start` to `end`, local days inclusive. A slot
/// may repeat, one from each table; everything that reads slots adds them up.
fn load(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
) -> rusqlite::Result<(Vec<Slot>, Vec<Recording>)> {
    register_word_count(conn)?;
    let window = format!(
        "c.created_at >= {} AND c.created_at < {}",
        local_midnight(1),
        local_midnight(2)
    );
    let bounds = params![iso(start), iso(end + Duration::days(1))];
    let mut slots = Vec::new();
    let mut recordings = Vec::new();
    for table in tables() {
        let grouped = format!(
            "SELECT strftime('%Y-%m-%d %H', c.created_at, 'localtime') AS slot, c.app_bundle_id, \
             {}, sum({}), sum(coalesce(c.duration_ms, 0)), sum({}) \
             FROM {} AND {window} GROUP BY slot, c.app_bundle_id",
            table.count, table.words, table.fixed, table.from
        );
        let mut statement = conn.prepare(&grouped)?;
        let mut rows = statement.query(bounds)?;
        while let Some(row) = rows.next()? {
            let slot: String = row.get(0)?;
            slots.push(Slot {
                day: day(&slot)?,
                hour: slot.get(11..13).and_then(|h| h.parse().ok()).unwrap_or(0),
                app: row.get(1)?,
                captures: row.get(2)?,
                words: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                speaking_ms: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                fixed: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            });
        }
        let lengths = format!(
            "SELECT date(c.created_at, 'localtime'), c.app_bundle_id, c.duration_ms, {} \
             FROM {} AND {window} AND c.duration_ms > 0",
            table.raw_words, table.from
        );
        let mut statement = conn.prepare(&lengths)?;
        let mut rows = statement.query(bounds)?;
        while let Some(row) = rows.next()? {
            let local: String = row.get(0)?;
            recordings.push(Recording {
                day: day(&local)?,
                app: row.get(1)?,
                duration_ms: row.get(2)?,
                raw_words: row.get(3)?,
            });
        }
    }
    Ok((slots, recordings))
}

/// The local day of the first dictation still counted, if any.
pub fn first_day(conn: &Connection) -> rusqlite::Result<Option<NaiveDate>> {
    let mut first: Option<NaiveDate> = None;
    for table in tables() {
        let sql = format!(
            "SELECT min(datetime(c.created_at, 'localtime')) FROM {}",
            table.from
        );
        let found: Option<String> = conn.query_row(&sql, [], |row| row.get(0))?;
        if let Some(found) = found {
            let found = day(&found)?;
            first = Some(first.map_or(found, |first| first.min(found)));
        }
    }
    Ok(first)
}

// -- counting -----------------------------------------------------------------

/// Python's `round()`: halves go to the even neighbour.
pub(crate) fn round(value: f64) -> i64 {
    value.round_ties_even() as i64
}

fn days(start: NaiveDate, end: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    start.iter_days().take_while(move |day| *day <= end)
}

fn weekday(day: NaiveDate) -> bool {
    day.weekday().num_days_from_monday() < 5
}

/// `statistics.median`: the middle value, or the mean of the middle two.
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    })
}

/// Linear-interpolated quantile of sorted `values`.
pub(crate) fn quantile(values: &[i64], q: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let at = (values.len() - 1) as f64 * q;
    let low = at as usize;
    let high = (low + 1).min(values.len() - 1);
    let gap = (values[high] - values[low]) as f64;
    Some(round(values[low] as f64 + gap * (at - low as f64)))
}

fn totals(window: &Window, scope: &Apps) -> Totals {
    let slots: Vec<&Slot> = window
        .slots
        .iter()
        .filter(|s| matches(scope, &s.app))
        .collect();
    let words: i64 = slots.iter().map(|s| s.words).sum();
    let speaking_ms: i64 = slots.iter().map(|s| s.speaking_ms).sum();
    let paces: Vec<f64> = window
        .recordings
        .iter()
        .filter(|r| matches(scope, &r.app) && r.duration_ms >= MIN_PACE_MS && r.raw_words != 0)
        .map(|r| r.raw_words as f64 / (r.duration_ms as f64 / 60_000.0))
        .collect();
    let mut by_day: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for s in &slots {
        *by_day.entry(s.day).or_default() += s.words;
    }
    // Every slot holds at least one capture, so these are the days dictated on.
    let hours: BTreeSet<(NaiveDate, u32)> = slots
        .iter()
        .filter(|s| s.captures != 0)
        .map(|s| (s.day, s.hour))
        .collect();
    Totals {
        words,
        captures: slots.iter().map(|s| s.captures).sum(),
        speaking_ms,
        pace_wpm: median(paces).map(round),
        time_saved_ms: (words * 60_000 / TYPING_WPM - speaking_ms).max(0),
        fixed_captures: slots.iter().map(|s| s.fixed).sum(),
        weekdays_dictated: by_day.keys().filter(|d| weekday(**d)).count() as i64,
        weekdays_in_period: days(window.start, window.end)
            .filter(|d| weekday(*d))
            .count() as i64,
        weekend_days: by_day
            .iter()
            .filter(|(d, _)| !weekday(**d))
            .map(|(d, words)| Day {
                date: iso(*d),
                words: *words,
            })
            .collect(),
        hours_dictated: hours.len() as i64,
    }
}

fn sum_by<K: std::hash::Hash + Eq>(
    slots: &[Slot],
    scope: &Apps,
    key: impl Fn(&Slot) -> K,
) -> HashMap<K, i64> {
    let mut sums = HashMap::new();
    for s in slots.iter().filter(|s| matches(scope, &s.app)) {
        *sums.entry(key(s)).or_default() += s.words;
    }
    sums
}

fn monday(day: NaiveDate) -> NaiveDate {
    day - Duration::days(day.weekday().num_days_from_monday().into())
}

/// The words chart: hours for Today, days for 7 and 30 days, weeks for All
/// time.
fn series(
    period: Period,
    current: &Window,
    previous: Option<&Window>,
    scope: &Apps,
    now: NaiveDateTime,
) -> Vec<Point> {
    let mine = |key: &dyn Fn(&Slot) -> NaiveDate| sum_by(&current.slots, scope, key);
    match (period, previous) {
        (Period::Today, previous) => {
            let by_hour = sum_by(&current.slots, scope, |s| s.hour);
            let prev_by_hour = previous.map(|p| sum_by(&p.slots, scope, |s| s.hour));
            (0..24)
                .map(|hour| Point {
                    start: format!("{}T{hour:02}:00", iso(current.start)),
                    words: (hour <= now.hour()).then(|| by_hour.get(&hour).copied().unwrap_or(0)),
                    previous_start: previous.map(|p| format!("{}T{hour:02}:00", iso(p.start))),
                    previous_words: prev_by_hour
                        .as_ref()
                        .map(|sums| sums.get(&hour).copied().unwrap_or(0)),
                })
                .collect()
        }
        (Period::All, _) => {
            let by_week = mine(&|s| monday(s.day));
            let mut week = monday(current.start);
            let mut points = Vec::new();
            while week <= current.end {
                points.push(Point {
                    start: iso(week),
                    words: Some(by_week.get(&week).copied().unwrap_or(0)),
                    previous_start: None,
                    previous_words: None,
                });
                week += Duration::days(7);
            }
            points
        }
        (_, previous) => {
            let by_day = mine(&|s| s.day);
            let prev_by_day = previous.map(|p| sum_by(&p.slots, scope, |s| s.day));
            let shift = previous.map(|p| current.start - p.start);
            days(current.start, current.end)
                .map(|day| Point {
                    start: iso(day),
                    words: Some(by_day.get(&day).copied().unwrap_or(0)),
                    previous_start: shift.map(|shift| iso(day - shift)),
                    previous_words: shift
                        .zip(prev_by_day.as_ref())
                        .map(|(shift, sums)| sums.get(&(day - shift)).copied().unwrap_or(0)),
                })
                .collect()
        }
    }
}

fn lengths(window: &Window, scope: &Apps) -> Lengths {
    let mut durations: Vec<i64> = window
        .recordings
        .iter()
        .filter(|r| matches(scope, &r.app))
        .map(|r| r.duration_ms)
        .collect();
    durations.sort_unstable();
    let mut counts = vec![0; LENGTH_BIN_EDGES_S.len() + 1];
    for ms in &durations {
        counts[LENGTH_BIN_EDGES_S
            .iter()
            .filter(|edge| *ms >= *edge * 1000)
            .count()] += 1;
    }
    Lengths {
        bin_edges_s: LENGTH_BIN_EDGES_S.to_vec(),
        counts,
        p25_ms: quantile(&durations, 0.25),
        median_ms: quantile(&durations, 0.5),
        p75_ms: quantile(&durations, 0.75),
    }
}

fn heatmap(window: &Window, scope: &Apps) -> Vec<Vec<i64>> {
    let mut grid = vec![vec![0; 24]; 7];
    for s in window.slots.iter().filter(|s| matches(scope, &s.app)) {
        grid[s.day.weekday().num_days_from_monday() as usize][s.hour as usize] += s.words;
    }
    grid
}

fn apps(window: &Window) -> Vec<App> {
    let mut sums: HashMap<Option<String>, (i64, i64)> = HashMap::new();
    for s in &window.slots {
        let sum = sums.entry(s.app.clone()).or_default();
        sum.0 += s.words;
        sum.1 += s.captures;
    }
    let mut apps: Vec<App> = sums
        .into_iter()
        .map(|(app_bundle_id, (words, captures))| App {
            app_bundle_id,
            words,
            captures,
        })
        .collect();
    // Unknown app last, like the app list.
    apps.sort_by(|a, b| {
        (
            a.app_bundle_id.is_none(),
            -a.words,
            -a.captures,
            &a.app_bundle_id,
        )
            .cmp(&(
                b.app_bundle_id.is_none(),
                -b.words,
                -b.captures,
                &b.app_bundle_id,
            ))
    });
    apps
}

// -- answers ------------------------------------------------------------------

/// Every app's totals for each local day from `start` to `end` that has
/// dictation.
pub fn daily_totals(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
) -> rusqlite::Result<BTreeMap<NaiveDate, Totals>> {
    let (slots, recordings) = load(conn, start, end)?;
    let dictated: BTreeSet<NaiveDate> = slots.iter().map(|s| s.day).collect();
    Ok(dictated
        .into_iter()
        .map(|day| {
            let window = Window::new(day, day).split(&slots, &recordings);
            (day, totals(&window, &Apps::All))
        })
        .collect())
}

/// One period's dictation, through today: totals, the words chart, where,
/// when and how long. `now` is the Mac's local time. Every period but All
/// time comes with the equal period before it.
pub fn usage_stats(
    conn: &Connection,
    period: Period,
    scope: &Apps,
    now: NaiveDateTime,
) -> rusqlite::Result<Stats> {
    let today = now.date();
    let (current, previous, load_from) = match period.days() {
        None => {
            let start = first_day(conn)?.unwrap_or(today).min(today);
            (Window::new(start, today), None, start)
        }
        Some(days) => {
            let start = today - Duration::days(days - 1);
            let previous = Window::new(start - Duration::days(days), start - Duration::days(1));
            let load_from = previous.start;
            (Window::new(start, today), Some(previous), load_from)
        }
    };
    let (slots, recordings) = load(conn, load_from, today)?;
    let current = current.split(&slots, &recordings);
    let previous = previous.map(|p| p.split(&slots, &recordings));

    Ok(Stats {
        period: period.name(),
        bucket: match period {
            Period::Today => "hour",
            Period::All => "week",
            _ => "day",
        },
        start: iso(current.start),
        end: iso(current.end),
        previous_start: previous.as_ref().map(|p| iso(p.start)),
        previous_end: previous.as_ref().map(|p| iso(p.end)),
        typing_wpm: TYPING_WPM,
        current: totals(&current, scope),
        previous: previous.as_ref().map(|p| totals(p, scope)),
        all_apps: totals(&current, &Apps::All),
        series: series(period, &current, previous.as_ref(), scope, now),
        apps: apps(&current),
        heatmap: heatmap(&current, scope),
        lengths: lengths(&current, scope),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_where_python_splits() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("  two\u{a0}words "), 2);
        assert_eq!(word_count("a\u{1f}b\u{3000}c\nd"), 4);
        assert_eq!(word_count("don't-stop"), 1);
    }

    #[test]
    fn rounds_halves_to_even_like_python() {
        assert_eq!(round(2.5), 2);
        assert_eq!(round(3.5), 4);
        assert_eq!(round(-0.5), 0);
        assert_eq!(quantile(&[1, 2], 0.5), Some(2));
        assert_eq!(quantile(&[1, 4], 0.5), Some(2));
        assert_eq!(quantile(&[], 0.5), None);
        assert_eq!(median(vec![3.0, 1.0, 2.0, 10.0]), Some(2.5));
    }
}
