//! Anonymous daily usage reports, sent to Amplitude when the user shares
//! usage (`capture_settings.share_usage`, on by default and offered at the
//! end of onboarding). Python: `backend/services/usage_report.py`.
//!
//! One `daily_usage` event per local day with dictation: counts, words,
//! speaking time, time saved, pace, how fast takes landed and how many
//! failed. The setup it ran on (chip, memory, macOS, models, which features
//! are on) goes along as user properties. Never any text, audio, dictionary
//! words, style rules or app names, and no IP address (none is set on the
//! event).
//!
//! Only finished days are sent. `usage_sent_through` is the last day sent,
//! so days the Mac was off or Kass wasn't running are caught up on the next
//! run, in order. Each event's insert id is the install id plus its day, so
//! a day sent twice (a crash between the upload and saving the marker)
//! counts once. A first report catches up on every earlier day; turning
//! sharing back on starts from that day (`settings::update`), so the days it
//! was off are never sent.
//!
//! Runs a minute after the server starts, then hourly. Only in a release
//! build: a development build never sends unless `KASS_USAGE_REPORTS=1`, and
//! `KASS_USAGE_REPORTS=0` stops any build from sending. Not while standing
//! in for a shared Kass: that one reports.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration as Wait;

use chrono::{Duration, Local, NaiveDate, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::stats::{self, quantile, round, Totals};
use crate::db::{Db, DbError};
use crate::dictionary::store::{now, stored};
use crate::{settings, AppState};

/// Public by design: it ships in every client, like the site's.
const API_KEY: &str = "15288b16e4a64d54978fa9d86adddad1";
const ENDPOINT: &str = "https://api2.amplitude.com/2/httpapi";
const EVENT: &str = "daily_usage";
/// Amplitude takes far more per request; small batches keep a long catch-up
/// light.
pub const BATCH: i64 = 100;
const FIRST_RUN_DELAY: Wait = Wait::from_secs(60);
const RUN_EVERY: Wait = Wait::from_secs(60 * 60);
const POST_TIMEOUT: Wait = Wait::from_secs(15);
/// Take reports never wait longer than this to be sent.
const KEEP_TAKE_REPORTS_DAYS: i64 = 60;
const DICTIONARY_BUCKETS: [(i64, &str); 4] =
    [(0, "0"), (10, "1-10"), (50, "11-50"), (200, "51-200")];

/// Whether this build sends: `KASS_USAGE_REPORTS` when it's set (`1` sends),
/// else whether it's a release build.
pub fn sending_allowed(flag: Option<&OsStr>, release: bool) -> bool {
    match flag {
        Some(flag) => flag == "1",
        None => release,
    }
}

/// The Mac's chip, memory and macOS version.
#[derive(Clone, Debug, Default)]
pub struct Machine {
    pub chip: Option<String>,
    pub memory_gb: Option<i64>,
    pub os_version: Option<String>,
}

fn sysctl(name: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// This Mac, read once.
pub fn machine() -> Machine {
    static MACHINE: OnceLock<Machine> = OnceLock::new();
    MACHINE
        .get_or_init(|| Machine {
            chip: sysctl("machdep.cpu.brand_string"),
            memory_gb: sysctl("hw.memsize")
                .filter(|bytes| !bytes.is_empty() && bytes.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|bytes| bytes.parse::<u64>().ok())
                .map(|bytes| round(bytes as f64 / (1u64 << 30) as f64)),
            os_version: sysctl("kern.osproductversion"),
        })
        .clone()
}

/// What a report says about where it runs, besides the settings.
#[derive(Clone, Debug)]
pub struct About {
    pub app_version: String,
    pub machine: Machine,
    /// A trained voice adapter is in use.
    pub voice_training: bool,
}

pub fn dictionary_bucket(count: i64) -> &'static str {
    DICTIONARY_BUCKETS
        .iter()
        .find(|(most, _)| count <= *most)
        .map_or("200+", |(_, label)| label)
}

/// Which models and features are in use, without anything the user wrote.
fn setup_properties(
    conn: &Connection,
    row: &settings::Settings,
    about: &About,
) -> rusqlite::Result<Value> {
    let count = |table: &str| -> rusqlite::Result<i64> {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
    };
    Ok(json!({
        "speech_model": row.stt_model,
        "cleanup_model": row.llm_model,
        "auto_refine": row.auto_refine,
        "live_text": row.live_text,
        "voice_edits": row.voice_edits,
        "command_mode": !row.chord_command_keys.is_empty(),
        "sound_cues": row.sound_cues,
        "keep_recordings": !row.discard_audio,
        "history_days": row.history_retention_days,
        "styles": count("writing_styles")?,
        "dictionary_size": dictionary_bucket(count("dictionary_entries")?),
        "voice_training": about.voice_training,
        "chip": about.machine.chip,
        "memory_gb": about.machine.memory_gb,
    }))
}

fn iso(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

fn parse_day(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
}

/// The first local day with dictation or a take report.
fn first_day(conn: &Connection) -> rusqlite::Result<Option<NaiveDate>> {
    let dictated = stats::first_day(conn)?;
    let first_take: Option<String> = conn
        .query_row(
            "SELECT date(created_at, 'localtime') FROM take_reports ORDER BY created_at LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let took = first_take.as_deref().and_then(parse_day);
    Ok(match (dictated, took) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    })
}

/// One take report: how it ended and how fast.
struct Take {
    mode: String,
    outcome: String,
    latency_ms: Option<i64>,
}

fn takes_by_day(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
) -> rusqlite::Result<BTreeMap<NaiveDate, Vec<Take>>> {
    let mut statement = conn.prepare(
        "SELECT date(created_at, 'localtime'), mode, outcome, latency_ms FROM take_reports \
         WHERE created_at >= datetime(?1, 'utc') AND created_at < datetime(?2, 'utc')",
    )?;
    let mut rows = statement.query(params![iso(start), iso(end + Duration::days(1))])?;
    let mut days: BTreeMap<NaiveDate, Vec<Take>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let Some(day) = row
            .get::<_, Option<String>>(0)?
            .as_deref()
            .and_then(parse_day)
        else {
            continue;
        };
        days.entry(day).or_default().push(Take {
            mode: row.get(1)?,
            outcome: row.get(2)?,
            latency_ms: row.get(3)?,
        });
    }
    Ok(days)
}

/// One day's numbers. `totals` is None on a day with takes but no saved
/// dictation.
fn day_properties(totals: Option<&Totals>, takes: &[Take]) -> Value {
    let delivered = |t: &&Take| t.outcome == "delivered";
    let mut latencies: Vec<i64> = takes
        .iter()
        .filter(delivered)
        .filter_map(|t| t.latency_ms)
        .collect();
    latencies.sort_unstable();
    json!({
        "dictations": totals.map_or(0, |t| t.captures),
        "words": totals.map_or(0, |t| t.words),
        "speaking_seconds": totals.map_or(0, |t| round(t.speaking_ms as f64 / 1000.0)),
        "time_saved_seconds": totals.map_or(0, |t| round(t.time_saved_ms as f64 / 1000.0)),
        "pace_wpm": totals.and_then(|t| t.pace_wpm),
        "fixed_dictations": totals.map_or(0, |t| t.fixed_captures),
        "delivered_takes": takes.iter().filter(delivered).count(),
        "failed_takes": takes.iter().filter(|t| t.outcome == "failed").count(),
        "command_takes": takes.iter().filter(|t| t.mode == "command").count(),
        "latency_p50_ms": quantile(&latencies, 0.5),
        "latency_p95_ms": quantile(&latencies, 0.95),
    })
}

/// The finished days not yet sent, oldest first, or None.
pub fn pending_days(
    sent_through: Option<&str>,
    first: Option<NaiveDate>,
    today: NaiveDate,
) -> Option<(NaiveDate, NaiveDate)> {
    let start = match sent_through.and_then(parse_day) {
        Some(sent) => sent + Duration::days(1),
        None => first?,
    };
    let end = today - Duration::days(1);
    (start <= end).then_some((start, end))
}

/// Local noon of `day`, in milliseconds since the epoch.
fn noon_ms(day: NaiveDate) -> i64 {
    let noon = day.and_hms_opt(12, 0, 0).expect("noon exists");
    Local.from_local_datetime(&noon).earliest().map_or_else(
        || noon.and_utc().timestamp_millis(),
        |at| at.timestamp_millis(),
    )
}

/// A `daily_usage` event for each day from `start` to `end` with any
/// dictation or take.
fn build_events(
    conn: &Connection,
    row: &settings::Settings,
    device_id: &str,
    start: NaiveDate,
    end: NaiveDate,
    about: &About,
) -> rusqlite::Result<Vec<Value>> {
    let totals = stats::daily_totals(conn, start, end)?;
    let takes = takes_by_day(conn, start, end)?;
    let user_properties = setup_properties(conn, row, about)?;
    let mut days: Vec<NaiveDate> = totals.keys().chain(takes.keys()).copied().collect();
    days.sort_unstable();
    days.dedup();
    Ok(days
        .into_iter()
        .map(|day| {
            let mut properties = json!({ "day": iso(day) });
            let numbers = day_properties(totals.get(&day), takes.get(&day).map_or(&[], |t| t));
            if let (Value::Object(properties), Value::Object(numbers)) = (&mut properties, numbers)
            {
                properties.extend(numbers);
            }
            json!({
                "event_type": EVENT,
                "device_id": device_id,
                "time": noon_ms(day),
                "insert_id": format!("{device_id}-{}", iso(day)),
                "event_properties": properties,
                "user_properties": user_properties,
                "app_version": about.app_version,
                "platform": "macOS",
                "os_name": "macOS",
                "os_version": about.machine.os_version,
                "device_manufacturer": "Apple",
                "device_model": about.machine.chip,
            })
        })
        .collect())
}

fn forget_takes_before(conn: &Connection, day: NaiveDate) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM take_reports WHERE created_at < datetime(?1, 'utc')",
        [iso(day)],
    )?;
    Ok(())
}

fn set_usage_column(conn: &Connection, column: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("UPDATE capture_settings SET {column} = ?1, updated_at = ?2 WHERE id = 1"),
        params![value, stored(now())],
    )?;
    Ok(())
}

/// What a run sends: the settings, the install id and the days.
struct Plan {
    row: settings::Settings,
    device_id: String,
    start: NaiveDate,
    end: NaiveDate,
}

/// What a run sends, if anything. Clears take reports too old to send, or
/// from days that will never be sent, and makes the install id the first
/// time.
fn plan(conn: &mut Connection, today: NaiveDate) -> rusqlite::Result<Option<Plan>> {
    let row = settings::get(conn)?;
    forget_takes_before(conn, today - Duration::days(KEEP_TAKE_REPORTS_DAYS))?;
    if !row.share_usage {
        // Nothing from a day with sharing off is ever sent.
        forget_takes_before(conn, today)?;
        return Ok(None);
    }
    if !row.onboarding_completed {
        return Ok(None);
    }
    let saved: Option<String> = conn.query_row(
        "SELECT usage_device_id FROM capture_settings WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    let device_id = match saved.filter(|id| !id.is_empty()) {
        Some(id) => id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            set_usage_column(conn, "usage_device_id", &id)?;
            id
        }
    };
    let days = pending_days(row.usage_sent_through.as_deref(), first_day(conn)?, today);
    Ok(days.map(|(start, end)| Plan {
        row,
        device_id,
        start,
        end,
    }))
}

/// Send every finished day not yet sent, `batch` days at a time (`BATCH`):
/// `about` is asked once, when there is something to send, and `post` sends
/// one batch, saying whether it went. Returns how many events went. The
/// database is free while a batch is on its way.
pub async fn report<A, AF, P, PF>(
    db: &Db,
    today: NaiveDate,
    batch: i64,
    about: A,
    mut post: P,
) -> Result<usize, DbError>
where
    A: FnOnce() -> AF,
    AF: Future<Output = About>,
    P: FnMut(Vec<Value>) -> PF,
    PF: Future<Output = bool>,
{
    let Some(Plan {
        row,
        device_id,
        mut start,
        end,
    }) = db.call(move |conn| plan(conn, today)).await?
    else {
        return Ok(0);
    };
    let about = about().await;
    let mut sent = 0;
    while start <= end {
        let batch_end = end.min(start + Duration::days(batch.max(1) - 1));
        let (row, id, info) = (row.clone(), device_id.clone(), about.clone());
        let events = db
            .call(move |conn| build_events(conn, &row, &id, start, batch_end, &info))
            .await?;
        let count = events.len();
        if count > 0 && !post(events).await {
            return Ok(sent);
        }
        sent += count;
        db.call(move |conn| {
            let tx = conn.transaction()?;
            set_usage_column(&tx, "usage_sent_through", &iso(batch_end))?;
            forget_takes_before(&tx, batch_end + Duration::days(1))?;
            tx.commit()
        })
        .await?;
        start = batch_end + Duration::days(1);
    }
    Ok(sent)
}

/// Exactly what leaves the Mac, less the public API key, so anyone can
/// check it in the log.
fn logged(events: &[Value]) -> String {
    format!("[usage] Sending usage to {ENDPOINT}: {}", json!(events))
}

fn upload(events: Vec<Value>) -> Value {
    json!({ "api_key": API_KEY, "events": events })
}

/// Send one batch to Amplitude, saying whether it was taken.
async fn post(client: &reqwest::Client, events: Vec<Value>) -> bool {
    eprintln!("{}", logged(&events));
    let body = upload(events);
    let response = match client
        .post(ENDPOINT)
        .header("content-type", "application/json")
        .body(body.to_string())
        .timeout(POST_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            eprintln!("[usage] Usage report not sent: {error}");
            return false;
        }
    };
    let status = response.status();
    if status.as_u16() != 200 {
        let text = response.text().await.unwrap_or_default();
        let text: String = text.chars().take(200).collect();
        eprintln!("[usage] Usage report refused: HTTP {status} {text}");
        return false;
    }
    true
}

async fn voice_training_on(state: &AppState) -> bool {
    crate::model_improvement::adapter_in_use(state).await
}

/// A minute after the server starts, so it never competes with loading
/// models, then hourly.
pub async fn watch(state: AppState, app_version: String) {
    let flag = std::env::var_os("KASS_USAGE_REPORTS");
    if state.sync.standing_in || !sending_allowed(flag.as_deref(), !cfg!(debug_assertions)) {
        return;
    }
    let client = reqwest::Client::new();
    tokio::time::sleep(FIRST_RUN_DELAY).await;
    loop {
        if state.db.path().exists() {
            let about = || async {
                About {
                    app_version: app_version.clone(),
                    machine: tokio::task::spawn_blocking(machine)
                        .await
                        .unwrap_or_default(),
                    voice_training: voice_training_on(&state).await,
                }
            };
            let today = Local::now().date_naive();
            match report(&state.db, today, BATCH, about, |events| {
                post(&client, events)
            })
            .await
            {
                Ok(0) => {}
                Ok(sent) => eprintln!("[usage] Sent {sent} day(s) of usage"),
                Err(error) => eprintln!("[usage] Usage report failed; trying again later: {error}"),
            }
        }
        tokio::time::sleep(RUN_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        parse_day(text).unwrap()
    }

    #[test]
    fn the_exact_payload_is_logged_before_sending() {
        let events = vec![json!({"event_type": "daily_usage", "event_properties": {"words": 10}})];
        let line = logged(&events);
        let (head, payload) = line.split_once(": ").unwrap();
        assert_eq!(head, format!("[usage] Sending usage to {ENDPOINT}"));
        let payload: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(payload, upload(events)["events"]);
        assert!(!line.contains(API_KEY));
    }

    #[test]
    fn dictionary_size_is_a_range() {
        assert_eq!(dictionary_bucket(0), "0");
        assert_eq!(dictionary_bucket(7), "1-10");
        assert_eq!(dictionary_bucket(50), "11-50");
        assert_eq!(dictionary_bucket(500), "200+");
    }

    #[test]
    fn dev_builds_do_not_send() {
        assert!(!sending_allowed(None, false));
        assert!(sending_allowed(None, true));
        assert!(sending_allowed(Some(OsStr::new("1")), false));
        assert!(!sending_allowed(Some(OsStr::new("0")), true));
        assert!(!sending_allowed(Some(OsStr::new("")), true));
    }

    #[test]
    fn only_finished_days_are_pending() {
        let today = day("2026-10-01");
        assert_eq!(pending_days(None, None, today), None);
        assert_eq!(
            pending_days(None, Some(day("2026-09-20")), today),
            Some((day("2026-09-20"), day("2026-09-30")))
        );
        assert_eq!(
            pending_days(Some("2026-09-27"), Some(day("2026-09-20")), today),
            Some((day("2026-09-28"), day("2026-09-30")))
        );
        assert_eq!(pending_days(Some("2026-09-30"), None, today), None);
        assert_eq!(pending_days(None, Some(today), today), None);
    }
}
