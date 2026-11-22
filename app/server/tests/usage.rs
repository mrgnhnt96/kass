//! Usage stats (`GET /captures/stats`) and the daily usage report against a
//! real kass.db, in Denver's time zone. Python: test_usage_stats and
//! test_usage_report, plus a parity check against what Python's
//! usage_stats.py and usage_report.py answered for the same database
//! (`usage/python.json`, from `usage/seed.sql`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, NaiveDateTime};
use kass_server::captures::store::Apps;
use kass_server::db::Db;
use kass_server::usage::report::{self, About, Machine, BATCH};
use kass_server::usage::stats::{usage_stats, Period, Stats, TYPING_WPM};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The Mac's zone, as SQLite's localtime and chrono both read it. Every test
/// sets the same one, so tests running side by side agree.
fn denver() {
    std::env::set_var("TZ", "America/Denver");
    extern "C" {
        fn tzset();
    }
    // SAFETY: tzset only rereads TZ, which every test sets to the same value.
    unsafe { tzset() };
}

fn at(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

/// Monday, Sep 28 2026, 10:00 in Denver (MDT, UTC-6).
fn now() -> NaiveDateTime {
    at("2026-09-28 10:00")
}

/// Thursday, Oct 1 2026, in Denver.
fn today() -> NaiveDate {
    day("2026-10-01")
}

/// A fresh kass.db with the settings row, onboarding done.
fn database() -> PathBuf {
    denver();
    let data = std::env::temp_dir().join(format!("kass-usage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data).unwrap();
    let path = data.join("kass.db");
    kass_server::db::prepare(&path).unwrap();
    let mut conn = Connection::open(&path).unwrap();
    kass_server::settings::get(&mut conn).unwrap();
    conn.execute("UPDATE capture_settings SET onboarding_completed = 1", [])
        .unwrap();
    path
}

fn stored(utc: NaiveDateTime) -> String {
    utc.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// A capture at naive UTC `utc`.
struct Capture {
    utc: NaiveDateTime,
    words: usize,
    raw: Option<usize>,
    duration_ms: i64,
    app: Option<&'static str>,
    source: &'static str,
    fixed: bool,
    text: &'static str,
}

impl Capture {
    fn at(utc: &str) -> Self {
        Capture {
            utc: at(utc),
            words: 10,
            raw: None,
            duration_ms: 6000,
            app: Some("com.apple.mail"),
            source: "dictation",
            fixed: false,
            text: "word",
        }
    }
    fn words(mut self, words: usize) -> Self {
        self.words = words;
        self
    }
    fn raw(mut self, raw: usize) -> Self {
        self.raw = Some(raw);
        self
    }
    fn ms(mut self, duration_ms: i64) -> Self {
        self.duration_ms = duration_ms;
        self
    }
    fn app(mut self, app: Option<&'static str>) -> Self {
        self.app = app;
        self
    }
    fn source(mut self, source: &'static str) -> Self {
        self.source = source;
        self
    }
    fn fixed(mut self) -> Self {
        self.fixed = true;
        self
    }
    fn text(mut self, text: &'static str) -> Self {
        self.text = text;
        self
    }

    fn add(self, conn: &Connection) {
        let id = uuid::Uuid::new_v4().to_string();
        let say = |n: usize| vec![self.text; n].join(" ");
        conn.execute(
            "INSERT INTO captures (id, audio_path, source, duration_ms, transcript_raw, \
             transcript_refined, app_bundle_id, app_name, audio_deleted, created_at) \
             VALUES (?1, 'captures/x.wav', ?2, ?3, ?4, ?5, ?6, ?6, 0, ?7)",
            params![
                id,
                self.source,
                self.duration_ms,
                say(self.raw.unwrap_or(self.words)),
                say(self.words),
                self.app,
                stored(self.utc)
            ],
        )
        .unwrap();
        if self.fixed {
            conn.execute(
                "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, \
                 snapshot) VALUES (?1, ?2, 'refined', 'x', '', '{}')",
                params![uuid::Uuid::new_v4().to_string(), id],
            )
            .unwrap();
        }
    }
}

fn stats(conn: &Connection, period: Period, apps: &Apps, now: NaiveDateTime) -> Value {
    let stats: Stats = usage_stats(conn, period, apps, now).unwrap();
    serde_json::to_value(stats).unwrap()
}

fn point<'a>(stats: &'a Value, start: &str) -> &'a Value {
    stats["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["start"] == start)
        .unwrap()
}

// -- stats --------------------------------------------------------------------

#[test]
fn groups_by_local_day_and_hour() {
    let conn = Connection::open(database()).unwrap();
    // 03:30 UTC on the 28th is 21:30 on Sunday the 27th in Denver.
    Capture::at("2026-09-28 03:30").words(5).add(&conn);
    // 06:30 UTC is 00:30 on Monday the 28th.
    Capture::at("2026-09-28 06:30").words(7).add(&conn);

    let week = stats(&conn, Period::Week, &Apps::All, now());
    assert_eq!(point(&week, "2026-09-27")["words"], 5);
    assert_eq!(point(&week, "2026-09-28")["words"], 7);
    assert_eq!(week["heatmap"][6][21], 5); // Sunday, 9p
    assert_eq!(week["heatmap"][0][0], 7); // Monday, 12a

    let today = stats(&conn, Period::Today, &Apps::All, now());
    assert_eq!(today["current"]["words"], 7);
    assert_eq!(today["previous"]["words"], 5);
    assert_eq!(point(&today, "2026-09-28T00:00")["words"], 7);
    assert_eq!(point(&today, "2026-09-28T00:00")["previous_words"], 0);
    assert_eq!(point(&today, "2026-09-28T21:00")["previous_words"], 5);
    // Hours still to come today have no value yet.
    assert_eq!(point(&today, "2026-09-28T10:00")["words"], 0);
    assert_eq!(point(&today, "2026-09-28T11:00")["words"], Value::Null);
}

#[test]
fn local_days_follow_daylight_saving() {
    let conn = Connection::open(database()).unwrap();
    // Denver leaves daylight saving on Nov 1 2026: 06:30 UTC on Nov 2 is 23:30
    // on Nov 1 (MST, UTC-7), where the summer offset would have said Nov 2.
    Capture::at("2026-11-02 06:30").words(4).add(&conn);

    let stats = stats(&conn, Period::Week, &Apps::All, at("2026-11-03 12:00"));
    assert_eq!(point(&stats, "2026-11-01")["words"], 4);
    assert_eq!(stats["heatmap"][6][23], 4);
}

#[test]
fn compares_with_the_equal_period_before() {
    let conn = Connection::open(database()).unwrap();
    Capture::at("2026-09-23 16:00").words(30).add(&conn); // this week
    Capture::at("2026-09-24 16:00").words(30).add(&conn);
    Capture::at("2026-09-16 16:00").words(40).add(&conn); // the week before
    Capture::at("2026-09-14 16:00").words(1000).add(&conn); // before that

    let stats = stats(&conn, Period::Week, &Apps::All, now());
    assert_eq!(stats["start"], "2026-09-22");
    assert_eq!(stats["end"], "2026-09-28");
    assert_eq!(stats["previous_start"], "2026-09-15");
    assert_eq!(stats["previous_end"], "2026-09-21");
    assert_eq!(stats["current"]["words"], 60);
    assert_eq!(stats["previous"]["words"], 40);
    let wed = point(&stats, "2026-09-23");
    assert_eq!(wed["previous_start"], "2026-09-16");
    assert_eq!(wed["previous_words"], 40);
}

#[test]
fn all_time_is_weekly_with_nothing_to_compare() {
    let conn = Connection::open(database()).unwrap();
    Capture::at("2026-09-09 16:00").words(3).add(&conn); // Wednesday of the week of Sep 7
    Capture::at("2026-09-28 16:00").words(5).add(&conn);

    let stats = stats(&conn, Period::All, &Apps::All, now());
    assert_eq!(stats["bucket"], "week");
    assert_eq!(stats["previous"], Value::Null);
    let series: Vec<(String, i64)> = stats["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["start"].as_str().unwrap().into(),
                p["words"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        series,
        [
            ("2026-09-07".into(), 3),
            ("2026-09-14".into(), 0),
            ("2026-09-21".into(), 0),
            ("2026-09-28".into(), 5),
        ]
    );
    assert_eq!(stats["current"]["weekdays_in_period"], 14); // Sep 9 through Sep 28
}

#[test]
fn counts_weekdays_and_weekend_days() {
    let conn = Connection::open(database()).unwrap();
    Capture::at("2026-09-22 16:00").add(&conn); // Tue
    Capture::at("2026-09-22 18:00").add(&conn); // Tue again
    Capture::at("2026-09-25 16:00").add(&conn); // Fri
    Capture::at("2026-09-27 16:00").words(12).add(&conn); // Sun

    let totals = &stats(&conn, Period::Week, &Apps::All, now())["current"];
    assert_eq!(totals["weekdays_dictated"], 2);
    assert_eq!(totals["weekdays_in_period"], 5);
    assert_eq!(
        totals["weekend_days"],
        json!([{"date": "2026-09-27", "words": 12}])
    );
}

#[test]
fn pace_is_the_median_of_raw_words_over_captures_of_two_seconds_and_up() {
    let conn = Connection::open(database()).unwrap();
    Capture::at("2026-09-28 14:00").raw(10).ms(6000).add(&conn); // 100 wpm
    Capture::at("2026-09-28 14:01").raw(20).ms(6000).add(&conn); // 200 wpm
    Capture::at("2026-09-28 14:02").raw(30).ms(6000).add(&conn); // 300 wpm
    Capture::at("2026-09-28 14:03").raw(10).ms(1000).add(&conn); // 600 wpm, too short

    let stats = stats(&conn, Period::Today, &Apps::All, now());
    assert_eq!(stats["current"]["pace_wpm"], 200);
}

#[test]
fn time_saved_is_typing_time_less_speaking_time() {
    let conn = Connection::open(database()).unwrap();
    Capture::at("2026-09-28 14:00")
        .words(80)
        .raw(90)
        .ms(30_000)
        .add(&conn);

    let totals = &stats(&conn, Period::Today, &Apps::All, now())["current"];
    let typing_ms = 80 * 60_000 / TYPING_WPM;
    assert_eq!(totals["time_saved_ms"], typing_ms - 30_000);
    assert_eq!(totals["speaking_ms"], 30_000);
}

#[test]
fn filters_to_an_app_and_to_unknown() {
    let conn = Connection::open(database()).unwrap();
    Capture::at("2026-09-28 14:00").words(10).fixed().add(&conn);
    Capture::at("2026-09-28 14:05").words(20).add(&conn);
    Capture::at("2026-09-28 14:10")
        .words(30)
        .app(Some("com.tinyspeck.slackmacgap"))
        .add(&conn);
    Capture::at("2026-09-28 14:15")
        .words(40)
        .app(None)
        .add(&conn);
    Capture::at("2026-09-28 14:20")
        .words(500)
        .source("command")
        .add(&conn);

    let everything = stats(&conn, Period::Today, &Apps::All, now());
    let mail = stats(
        &conn,
        Period::Today,
        &Apps::One("com.apple.mail".into()),
        now(),
    );
    let unknown = stats(&conn, Period::Today, &Apps::Unknown, now());

    assert_eq!(everything["current"]["words"], 100); // the command is left out
    assert_eq!(mail["current"]["words"], 30);
    assert_eq!(mail["current"]["captures"], 2);
    assert_eq!(mail["current"]["fixed_captures"], 1);
    assert_eq!(unknown["current"]["words"], 40);
    // Every app's totals and shares stay beside the filtered ones.
    assert_eq!(mail["all_apps"]["words"], 100);
    // Most words first, more captures breaking a tie, and no app last.
    assert_eq!(
        mail["apps"],
        json!([
            {"app_bundle_id": "com.apple.mail", "words": 30, "captures": 2},
            {"app_bundle_id": "com.tinyspeck.slackmacgap", "words": 30, "captures": 1},
            {"app_bundle_id": null, "words": 40, "captures": 1},
        ])
    );
}

#[test]
fn lengths_bin_and_quartiles() {
    let conn = Connection::open(database()).unwrap();
    for seconds in [1, 4, 4, 8, 15, 25, 50] {
        Capture::at("2026-09-28 14:00")
            .ms(seconds * 1000)
            .add(&conn);
    }

    let lengths = &stats(&conn, Period::Today, &Apps::All, now())["lengths"];
    assert_eq!(lengths["counts"], json!([1, 2, 1, 1, 1, 1]));
    assert_eq!(lengths["median_ms"], 8000);
    assert_eq!(lengths["p25_ms"], 4000);
    assert_eq!(lengths["p75_ms"], 20_000);
}

#[test]
fn empty_database() {
    let conn = Connection::open(database()).unwrap();
    let stats = stats(&conn, Period::All, &Apps::All, now());
    assert_eq!(stats["current"]["words"], 0);
    assert_eq!(stats["current"]["pace_wpm"], Value::Null);
    assert_eq!(stats["lengths"]["median_ms"], Value::Null);
    let starts: Vec<&Value> = stats["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["start"])
        .collect();
    assert_eq!(starts, [&json!("2026-09-28")]);
}

/// A GET against a running server, its status and JSON body.
async fn get(address: std::net::SocketAddr, path: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    let reply = String::from_utf8(reply).unwrap();
    let (head, body) = reply.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn endpoint_checks_the_period() {
    let database = database();
    {
        let conn = Connection::open(&database).unwrap();
        let utc = chrono::Utc::now().naive_utc();
        Capture {
            utc,
            ..Capture::at("2026-01-01 00:00")
        }
        .words(9)
        .app(None)
        .add(&conn);
    }
    let server = kass_server::start(kass_server::Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        // Nothing listens there: stats never ask the worker.
        worker: "127.0.0.1:9".parse().unwrap(),
        database,
        shared_server: None,
        voice_model: None,
        models_dir: None,
        app_version: "0.0.0-test".into(),
    })
    .await
    .unwrap();
    let address = server.address();

    let (status, body) = get(address, "/captures/stats?period=year").await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        json!({"detail": "period must be one of today, 7d, 30d, all"})
    );

    let (status, body) = get(address, "/captures/stats?unknown_app=maybe").await;
    assert_eq!(status, 422);
    assert_eq!(body["detail"][0]["loc"], json!(["query", "unknown_app"]));

    let (status, body) = get(address, "/captures/stats?period=today&unknown_app=true").await;
    assert_eq!(status, 200);
    assert_eq!(body["current"]["words"], 9);
    assert_eq!(body["typing_wpm"], TYPING_WPM);

    let (status, body) = get(address, "/captures/stats").await;
    assert_eq!(status, 200);
    assert_eq!(body["period"], "7d");
}

// -- the daily report ---------------------------------------------------------

/// What the report says about the Mac, as Python's tests patch it.
fn about() -> About {
    About {
        app_version: "1.2.3".into(),
        machine: Machine {
            chip: Some("Apple M2".into()),
            memory_gb: Some(16),
            os_version: Some("26.0".into()),
        },
        voice_training: false,
    }
}

/// Every batch posted; answers `ok`.
#[derive(Clone)]
struct Sink {
    ok: bool,
    batches: Arc<Mutex<Vec<Vec<Value>>>>,
}

impl Sink {
    fn new(ok: bool) -> Self {
        Sink {
            ok,
            batches: Arc::default(),
        }
    }

    fn batches(&self) -> Vec<Vec<Value>> {
        self.batches.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<Value> {
        self.batches().into_iter().flatten().collect()
    }

    fn days(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|e| e["event_properties"]["day"].as_str().unwrap().to_string())
            .collect()
    }
}

async fn send(db: &Db, today: NaiveDate, sink: &Sink) -> usize {
    send_in(db, today, BATCH, sink).await
}

async fn send_in(db: &Db, today: NaiveDate, batch: i64, sink: &Sink) -> usize {
    report::report(
        db,
        today,
        batch,
        || async { about() },
        |events| {
            let sink = sink.clone();
            async move {
                sink.batches.lock().unwrap().push(events);
                sink.ok
            }
        },
    )
    .await
    .unwrap()
}

fn take(conn: &Connection, utc: &str, outcome: &str, latency_ms: Option<i64>) {
    conn.execute(
        "INSERT INTO take_reports (created_at, mode, outcome, latency_ms) \
         VALUES (?1, 'dictation', ?2, ?3)",
        params![stored(at(utc)), outcome, latency_ms],
    )
    .unwrap();
}

fn sent_through(conn: &Connection) -> Option<String> {
    conn.query_row("SELECT usage_sent_through FROM capture_settings", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn set(conn: &Connection, assignment: &str) {
    conn.execute(&format!("UPDATE capture_settings SET {assignment}"), [])
        .unwrap();
}

fn takes_left(conn: &Connection) -> Vec<String> {
    let mut statement = conn
        .prepare("SELECT created_at FROM take_reports ORDER BY created_at")
        .unwrap();
    let rows = statement.query_map([], |r| r.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

#[tokio::test]
async fn sends_each_finished_day_with_its_numbers() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    // 9 pm Sep 29 local is 3 am Sep 30 UTC: it belongs to Sep 29.
    Capture::at("2026-09-30 03:00").words(12).add(&conn);
    Capture::at("2026-09-30 16:00").words(40).add(&conn);
    take(&conn, "2026-09-30 16:00", "delivered", Some(300));
    take(&conn, "2026-09-30 16:05", "delivered", Some(500));
    take(&conn, "2026-09-30 16:06", "failed", None);
    // Today isn't finished: it waits for tomorrow.
    Capture::at("2026-10-01 16:00").words(99).add(&conn);
    let db = Db::new(path);
    let sink = Sink::new(true);

    assert_eq!(send(&db, today(), &sink).await, 2);

    assert_eq!(sink.days(), ["2026-09-29", "2026-09-30"]);
    let sep30 = &sink.events()[1];
    let numbers = &sep30["event_properties"];
    assert_eq!(sep30["event_type"], "daily_usage");
    assert_eq!(numbers["words"], 40);
    assert_eq!(numbers["dictations"], 1);
    assert_eq!(numbers["speaking_seconds"], 6);
    assert_eq!(numbers["pace_wpm"], 400);
    assert_eq!(numbers["delivered_takes"], 2);
    assert_eq!(numbers["failed_takes"], 1);
    assert_eq!(numbers["latency_p50_ms"], 400);
    assert_eq!(
        sep30["insert_id"],
        format!("{}-2026-09-30", sep30["device_id"].as_str().unwrap())
    );
    assert_eq!(sep30["user_properties"]["chip"], "Apple M2");
    assert_eq!(sep30["app_version"], "1.2.3");
    assert!(sep30.get("ip").is_none());
    assert_eq!(sent_through(&conn).as_deref(), Some("2026-09-30"));
}

#[tokio::test]
async fn never_sends_text_or_app_names() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    Capture::at("2026-09-30 16:00")
        .text("secretword")
        .add(&conn);
    let sink = Sink::new(true);
    send(&Db::new(path), today(), &sink).await;
    let sent = serde_json::to_string(&sink.events()).unwrap();
    assert!(!sent.contains("secretword"));
    assert!(!sent.contains("com.apple.mail"));
}

#[tokio::test]
async fn catches_up_days_missed_while_away() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    let db = Db::new(path);
    Capture::at("2026-09-20 16:00").add(&conn);
    send(&db, day("2026-09-21"), &Sink::new(true)).await;
    // Kass wasn't running from the 22nd to the 30th; dictation on two of those
    // days (made through the batch path or an import) still counts.
    Capture::at("2026-09-23 16:00").add(&conn);
    Capture::at("2026-09-27 16:00").add(&conn);
    let sink = Sink::new(true);

    send(&db, today(), &sink).await;

    assert_eq!(sink.days(), ["2026-09-23", "2026-09-27"]);
    assert_eq!(sent_through(&conn).as_deref(), Some("2026-09-30"));
}

#[tokio::test]
async fn a_failed_upload_is_retried_next_time() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    let db = Db::new(path);
    Capture::at("2026-09-30 16:00").add(&conn);
    send(&db, today(), &Sink::new(false)).await;
    assert_eq!(sent_through(&conn), None);

    let sink = Sink::new(true);
    send(&db, today(), &sink).await;
    assert_eq!(sink.events().len(), 1);
}

#[tokio::test]
async fn waits_for_onboarding() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    set(&conn, "onboarding_completed = 0");
    Capture::at("2026-09-30 16:00").add(&conn);
    let sink = Sink::new(true);
    assert_eq!(send(&Db::new(path), today(), &sink).await, 0);
    assert!(sink.batches().is_empty());
}

#[tokio::test]
async fn days_with_sharing_off_are_never_sent() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    let db = Db::new(path);
    set(&conn, "share_usage = 0");
    Capture::at("2026-09-29 16:00").add(&conn);
    take(&conn, "2026-09-29 16:00", "delivered", Some(400));
    let sink = Sink::new(true);
    send(&db, today(), &sink).await;
    assert!(sink.batches().is_empty());
    // Take reports from days that will never be sent aren't kept.
    assert!(takes_left(&conn).is_empty());

    // Turned back on today: the server marks yesterday sent, so only today
    // onward goes.
    set(&conn, "share_usage = 1, usage_sent_through = '2026-09-30'");
    Capture::at("2026-10-01 16:00").add(&conn);
    send(&db, day("2026-10-02"), &sink).await;
    assert_eq!(sink.days(), ["2026-10-01"]);
}

#[tokio::test]
async fn long_catch_up_goes_in_batches() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    for date in 25..=29 {
        Capture::at(&format!("2026-09-{date} 16:00")).add(&conn);
    }
    let sink = Sink::new(true);
    assert_eq!(send_in(&Db::new(path), today(), 2, &sink).await, 5);
    assert_eq!(sink.batches().len(), 3);
}

#[tokio::test]
async fn device_id_is_random_and_kept() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    let db = Db::new(path);
    Capture::at("2026-09-29 16:00").add(&conn);
    Capture::at("2026-09-30 16:00").add(&conn);
    let sink = Sink::new(true);
    send(&db, day("2026-09-30"), &sink).await;
    send(&db, today(), &sink).await;
    let events = sink.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["device_id"], events[1]["device_id"]);
    assert_eq!(events[0]["device_id"].as_str().unwrap().len(), 36);
}

#[tokio::test]
async fn sent_take_reports_are_deleted() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    take(&conn, "2026-09-30 16:00", "delivered", Some(400));
    take(&conn, "2026-10-01 16:00", "delivered", Some(400));
    send(&Db::new(path), today(), &Sink::new(true)).await;
    assert_eq!(takes_left(&conn), ["2026-10-01 16:00:00.000000"]);
}

// -- parity with Python -------------------------------------------------------

/// The same database Python answered for: captures across the end of
/// daylight saving, retired captures (one still saved, so counted once),
/// commands, blank refined text, Unicode spaces, takes and settings.
#[tokio::test]
async fn answers_what_python_answered() {
    let path = database();
    let conn = Connection::open(&path).unwrap();
    conn.execute("DELETE FROM capture_settings", []).unwrap();
    // The dump lists tables by name, so feedback comes before its captures.
    conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    conn.execute_batch(include_str!("usage/seed.sql")).unwrap();
    let python: Value = serde_json::from_str(include_str!("usage/python.json")).unwrap();
    let now = NaiveDateTime::parse_from_str(python["now"].as_str().unwrap(), "%Y-%m-%dT%H:%M:%S")
        .unwrap();

    for period in ["today", "7d", "30d", "all"] {
        for (name, apps) in [
            ("all", Apps::All),
            ("mail", Apps::One("com.apple.mail".into())),
            ("unknown", Apps::Unknown),
        ] {
            let key = format!("{period}/{name}");
            let rust = stats(&conn, Period::parse(period).unwrap(), &apps, now);
            assert_eq!(rust, python["stats"][&key], "{key}");
        }
    }

    let sink = Sink::new(true);
    let sent = report::report(
        &Db::new(path),
        now.date(),
        BATCH,
        || async {
            About {
                app_version: "9.9.9".into(),
                voice_training: true,
                ..about()
            }
        },
        |events| {
            let sink = sink.clone();
            async move {
                sink.batches.lock().unwrap().push(events);
                true
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(sent, python["sent"].as_u64().unwrap() as usize);
    assert_eq!(json!(sink.batches()), python["batches"]);
    assert_eq!(
        sent_through(&conn),
        python["sent_through"].as_str().map(String::from)
    );
    assert_eq!(
        takes_left(&conn).len(),
        python["takes_left"].as_u64().unwrap() as usize
    );
}
