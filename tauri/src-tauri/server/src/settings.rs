//! The capture settings row, history retention's counts, and take reports in
//! `kass.db`. Python: `backend/services/settings.py`,
//! `services/history_retention.py` (`status`, `count_expiring`) and
//! `services/usage_report.py` (`record_take`).
//!
//! The worker reads the same row for dictation; sweeping old captures and
//! reloading the speech model stay there, and the routes ask for them
//! (`crate::worker`).

use chrono::{Duration, NaiveDateTime};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dictionary::store::{now, Sync};

const STORED: &str = "%Y-%m-%d %H:%M:%S%.6f";
const SINGLETON_ID: i64 = 1;

/// The history retention choices in days; 0 keeps captures forever.
pub const RETENTION_DAYS: [i64; 5] = [0, 7, 30, 90, 365];

pub const MAX_TRANSFORMS: usize = 50;
pub const MAX_TRANSFORM_NAME_CHARS: usize = 60;
pub const MAX_INSTRUCTION_CHARS: usize = 2000;

fn stored(at: NaiveDateTime) -> String {
    at.format(STORED).to_string()
}

/// A saved Command Mode instruction, run by saying its name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub instruction: String,
}

/// One capture settings row, as `GET /settings/captures` answers it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Settings {
    pub stt_model: String,
    pub language: String,
    pub auto_refine: bool,
    pub llm_model: String,
    pub smart_cleanup: bool,
    pub self_correction: bool,
    pub preserve_technical: bool,
    pub punctuation_style: String,
    pub allow_auto_paste: bool,
    pub live_text: bool,
    pub voice_edits: bool,
    pub expressive: bool,
    pub sound_cues: bool,
    pub sound_cue_volume: f64,
    pub input_device_id: Option<String>,
    pub hotkey_enabled: bool,
    pub chord_push_to_talk_keys: Vec<String>,
    pub chord_toggle_to_talk_keys: Vec<String>,
    pub chord_command_keys: Vec<String>,
    pub command_transforms: Vec<Transform>,
    pub history_retention_days: i64,
    pub history_retention_confirmed: bool,
    pub onboarding_completed: bool,
    pub discard_audio: bool,
    pub share_usage: bool,
    pub shared_adapters: bool,
    #[serde(skip)]
    pub usage_sent_through: Option<String>,
}

fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// Right-hand modifiers, so the chords don't collide with left-hand shortcuts.
pub fn default_push_to_talk_chord() -> Vec<String> {
    keys(&["MetaRight", "AltGr"])
}

pub fn default_toggle_to_talk_chord() -> Vec<String> {
    keys(&["MetaRight", "AltGr", "Space"])
}

pub fn default_command_chord() -> Vec<String> {
    keys(&["MetaRight", "ShiftRight"])
}

pub fn default_transforms() -> Vec<Transform> {
    let transform = |id: &str, name: &str, instruction: &str| Transform {
        id: Some(id.to_string()),
        name: name.to_string(),
        instruction: instruction.to_string(),
    };
    vec![
        transform(
            "polish",
            "Polish",
            "Fix grammar, spelling and punctuation, and smooth out awkward phrasing so it reads \
             well. Keep my words, tone and meaning, and don't add anything new.",
        ),
        transform(
            "prompt-engineer",
            "Prompt Engineer",
            "Rewrite this as a clear, well-structured prompt for an AI assistant: the goal, the \
             relevant context, any constraints, and the output I expect. Keep every requirement \
             I mentioned and don't invent new ones.",
        ),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stt_model: "turbo".into(),
            language: "auto".into(),
            auto_refine: true,
            llm_model: "0.6B".into(),
            smart_cleanup: true,
            self_correction: true,
            preserve_technical: true,
            punctuation_style: "standard".into(),
            allow_auto_paste: true,
            live_text: false,
            voice_edits: true,
            expressive: true,
            sound_cues: true,
            sound_cue_volume: 0.5,
            input_device_id: None,
            hotkey_enabled: false,
            chord_push_to_talk_keys: default_push_to_talk_chord(),
            chord_toggle_to_talk_keys: default_toggle_to_talk_chord(),
            chord_command_keys: default_command_chord(),
            command_transforms: default_transforms(),
            history_retention_days: 30,
            history_retention_confirmed: false,
            onboarding_completed: false,
            discard_audio: false,
            share_usage: true,
            shared_adapters: true,
            usage_sent_through: None,
        }
    }
}

// -- transforms ---------------------------------------------------------------

/// A transform name as it sounds: its lowercase words, whatever the
/// punctuation. Python: `commands._spoken_key`.
pub fn spoken_key(text: &str) -> String {
    let folded: Vec<char> = caseless::default_case_fold_str(text).chars().collect();
    let word = |c: char| c.is_alphanumeric();
    let mut words = Vec::new();
    let mut at = 0;
    while at < folded.len() {
        if !word(folded[at]) {
            at += 1;
            continue;
        }
        let start = at;
        loop {
            while at < folded.len() && word(folded[at]) {
                at += 1;
            }
            let apostrophe = at < folded.len() && matches!(folded[at], '\'' | '\u{2019}');
            if apostrophe && at + 1 < folded.len() && word(folded[at + 1]) {
                at += 1;
            } else {
                break;
            }
        }
        words.push(folded[start..at].iter().collect::<String>());
    }
    words.join(" ").replace('\u{2019}', "'")
}

/// Check a transforms list from settings: named, instructed, unique by name.
/// The error is a message the settings UI shows. Python:
/// `commands.normalize_transforms`.
pub fn normalize_transforms(transforms: Vec<Transform>) -> Result<Vec<Transform>, String> {
    if transforms.len() > MAX_TRANSFORMS {
        return Err(format!("At most {MAX_TRANSFORMS} transforms"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(transforms.len());
    for transform in transforms {
        let name = transform.name.trim().to_string();
        let instruction = transform.instruction.trim().to_string();
        if name.is_empty() || instruction.is_empty() {
            return Err("Each transform needs a name and an instruction".into());
        }
        if name.chars().count() > MAX_TRANSFORM_NAME_CHARS {
            return Err(format!(
                "Transform names are at most {MAX_TRANSFORM_NAME_CHARS} characters"
            ));
        }
        if instruction.chars().count() > MAX_INSTRUCTION_CHARS {
            return Err(format!(
                "Transform instructions are at most {MAX_INSTRUCTION_CHARS} characters"
            ));
        }
        let key = spoken_key(&name);
        if key.is_empty() {
            return Err(format!(
                "'{name}' can't be said aloud; use letters or numbers"
            ));
        }
        if !seen.insert(key) {
            return Err(format!("Two transforms are called '{name}'"));
        }
        let id = transform
            .id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        result.push(Transform {
            id: Some(id),
            name,
            instruction,
        });
    }
    Ok(result)
}

// -- reading ------------------------------------------------------------------

const COLUMNS: &str = "stt_model, language, auto_refine, llm_model, smart_cleanup, \
    self_correction, preserve_technical, punctuation_style, allow_auto_paste, live_text, \
    voice_edits, expressive, sound_cues, sound_cue_volume, input_device_id, hotkey_enabled, \
    chord_push_to_talk_keys, chord_toggle_to_talk_keys, chord_command_keys, command_transforms, \
    history_retention_days, history_retention_confirmed, onboarding_completed, discard_audio, \
    share_usage, shared_adapters, usage_sent_through";

fn json_column<T: for<'a> Deserialize<'a>>(
    text: Option<String>,
    fallback: impl FnOnce() -> T,
) -> T {
    text.and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(fallback)
}

fn read_row(conn: &Connection) -> rusqlite::Result<Option<Settings>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM capture_settings WHERE id = ?1"),
        [SINGLETON_ID],
        |row| {
            Ok(Settings {
                stt_model: row.get(0)?,
                language: row.get(1)?,
                auto_refine: row.get(2)?,
                llm_model: row.get(3)?,
                smart_cleanup: row.get(4)?,
                self_correction: row.get(5)?,
                preserve_technical: row.get(6)?,
                punctuation_style: row.get(7)?,
                allow_auto_paste: row.get(8)?,
                live_text: row.get(9)?,
                voice_edits: row.get(10)?,
                expressive: row.get(11)?,
                sound_cues: row.get(12)?,
                sound_cue_volume: row.get(13)?,
                input_device_id: row.get(14)?,
                hotkey_enabled: row.get(15)?,
                chord_push_to_talk_keys: json_column(row.get(16)?, default_push_to_talk_chord),
                chord_toggle_to_talk_keys: json_column(row.get(17)?, default_toggle_to_talk_chord),
                chord_command_keys: json_column(row.get(18)?, default_command_chord),
                command_transforms: json_column(row.get(19)?, default_transforms),
                history_retention_days: row.get(20)?,
                history_retention_confirmed: row.get(21)?,
                onboarding_completed: row.get(22)?,
                discard_audio: row.get(23)?,
                share_usage: row.get(24)?,
                shared_adapters: row.get(25)?,
                usage_sent_through: row.get(26)?,
            })
        },
    )
    .optional()
}

/// One column's value as it's written to `kass.db`.
fn column_value(settings: &Settings, column: &str) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    let text = |text: &str| Sql::Text(text.to_string());
    let flag = |flag: bool| Sql::Integer(flag.into());
    let json = |value: Value| Sql::Text(value.to_string());
    let optional =
        |text: &Option<String>| text.as_ref().map_or(Sql::Null, |t| Sql::Text(t.clone()));
    match column {
        "stt_model" => text(&settings.stt_model),
        "language" => text(&settings.language),
        "auto_refine" => flag(settings.auto_refine),
        "llm_model" => text(&settings.llm_model),
        "smart_cleanup" => flag(settings.smart_cleanup),
        "self_correction" => flag(settings.self_correction),
        "preserve_technical" => flag(settings.preserve_technical),
        "punctuation_style" => text(&settings.punctuation_style),
        "allow_auto_paste" => flag(settings.allow_auto_paste),
        "live_text" => flag(settings.live_text),
        "voice_edits" => flag(settings.voice_edits),
        "expressive" => flag(settings.expressive),
        "sound_cues" => flag(settings.sound_cues),
        "sound_cue_volume" => Sql::Real(settings.sound_cue_volume),
        "input_device_id" => optional(&settings.input_device_id),
        "hotkey_enabled" => flag(settings.hotkey_enabled),
        "chord_push_to_talk_keys" => json(serde_json::json!(settings.chord_push_to_talk_keys)),
        "chord_toggle_to_talk_keys" => json(serde_json::json!(settings.chord_toggle_to_talk_keys)),
        "chord_command_keys" => json(serde_json::json!(settings.chord_command_keys)),
        "command_transforms" => json(serde_json::json!(settings.command_transforms)),
        "history_retention_days" => Sql::Integer(settings.history_retention_days),
        "history_retention_confirmed" => flag(settings.history_retention_confirmed),
        "onboarding_completed" => flag(settings.onboarding_completed),
        "discard_audio" => flag(settings.discard_audio),
        "share_usage" => flag(settings.share_usage),
        "shared_adapters" => flag(settings.shared_adapters),
        "usage_sent_through" => optional(&settings.usage_sent_through),
        other => unreachable!("not a capture settings column: {other}"),
    }
}

const ALL_COLUMNS: [&str; 27] = [
    "stt_model",
    "language",
    "auto_refine",
    "llm_model",
    "smart_cleanup",
    "self_correction",
    "preserve_technical",
    "punctuation_style",
    "allow_auto_paste",
    "live_text",
    "voice_edits",
    "expressive",
    "sound_cues",
    "sound_cue_volume",
    "input_device_id",
    "hotkey_enabled",
    "chord_push_to_talk_keys",
    "chord_toggle_to_talk_keys",
    "chord_command_keys",
    "command_transforms",
    "history_retention_days",
    "history_retention_confirmed",
    "onboarding_completed",
    "discard_audio",
    "share_usage",
    "shared_adapters",
    "usage_sent_through",
];

/// Settings that belong to one Mac: changing only these changes nothing a
/// shared Kass keeps (`services/sync/rows.py`, `OWN_SETTINGS`).
const OWN_COLUMNS: [&str; 2] = ["input_device_id", "usage_sent_through"];

/// The settings row, made with the defaults the first time it's read.
/// Making it isn't something the user changed, so it isn't recorded.
fn read_or_create(conn: &Connection) -> rusqlite::Result<Settings> {
    if let Some(settings) = read_row(conn)? {
        return Ok(settings);
    }
    let settings = Settings::default();
    let values: Vec<_> = ALL_COLUMNS
        .iter()
        .map(|column| column_value(&settings, column))
        .chain([rusqlite::types::Value::Text(stored(now()))])
        .collect();
    let marks = (2..=values.len() + 1)
        .map(|at| format!("?{at}"))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO capture_settings (id, {}, updated_at) VALUES (?1, {marks})",
            ALL_COLUMNS.join(", ")
        ),
        params_from_iter(
            std::iter::once(rusqlite::types::Value::Integer(SINGLETON_ID)).chain(values),
        ),
    )?;
    Ok(read_row(conn)?.unwrap_or(settings))
}

/// The capture settings, made with the defaults if there are none yet.
pub fn get(conn: &mut Connection) -> rusqlite::Result<Settings> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let settings = read_or_create(&tx)?;
    tx.commit()?;
    Ok(settings)
}

// -- writing ------------------------------------------------------------------

/// What a `PUT /settings/captures` sets. A null for a setting that can't be
/// empty is dropped before it gets here; `input_device_id` can be cleared.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub stt_model: Option<String>,
    pub language: Option<String>,
    pub auto_refine: Option<bool>,
    pub llm_model: Option<String>,
    pub smart_cleanup: Option<bool>,
    pub self_correction: Option<bool>,
    pub preserve_technical: Option<bool>,
    pub punctuation_style: Option<String>,
    pub allow_auto_paste: Option<bool>,
    pub live_text: Option<bool>,
    pub voice_edits: Option<bool>,
    pub expressive: Option<bool>,
    pub sound_cues: Option<bool>,
    pub sound_cue_volume: Option<f64>,
    pub input_device_id: Option<Option<String>>,
    pub hotkey_enabled: Option<bool>,
    pub chord_push_to_talk_keys: Option<Vec<String>>,
    pub chord_toggle_to_talk_keys: Option<Vec<String>>,
    pub chord_command_keys: Option<Vec<String>>,
    pub command_transforms: Option<Vec<Transform>>,
    pub history_retention_days: Option<i64>,
    pub onboarding_completed: Option<bool>,
    pub discard_audio: Option<bool>,
    pub share_usage: Option<bool>,
    pub shared_adapters: Option<bool>,
}

fn record_settings(conn: &Connection, sync: Sync, at: NaiveDateTime) -> rusqlite::Result<()> {
    if !sync.standing_in {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO sync_changes (kind, ref_id, deleted, changed_at) VALUES ('settings', ?1, 0, ?2) \
         ON CONFLICT (kind, ref_id) DO UPDATE SET deleted = excluded.deleted, changed_at = excluded.changed_at",
        params![SINGLETON_ID.to_string(), stored(at)],
    )?;
    Ok(())
}

/// Write the columns that differ from `before`, stamp `updated_at`, and
/// record the change for a shared Kass when one it keeps changed.
fn save(
    conn: &Connection,
    before: &Settings,
    after: &Settings,
    sync: Sync,
) -> rusqlite::Result<()> {
    let changed: Vec<&str> = ALL_COLUMNS
        .iter()
        .copied()
        .filter(|column| column_value(before, column) != column_value(after, column))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let at = now();
    let sets = changed
        .iter()
        .enumerate()
        .map(|(at, column)| format!("{column} = ?{}", at + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let values = changed
        .iter()
        .map(|column| column_value(after, column))
        .chain([
            rusqlite::types::Value::Text(stored(at)),
            rusqlite::types::Value::Integer(SINGLETON_ID),
        ]);
    conn.execute(
        &format!(
            "UPDATE capture_settings SET {sets}, updated_at = ?{} WHERE id = ?{}",
            changed.len() + 1,
            changed.len() + 2
        ),
        params_from_iter(values),
    )?;
    if changed.iter().any(|column| !OWN_COLUMNS.contains(column)) {
        record_settings(conn, sync, at)?;
    }
    Ok(())
}

/// The local day before today, as usage reports name days.
fn yesterday() -> String {
    (chrono::Local::now().date_naive() - Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

/// Apply `patch` and return the saved settings. Errs with a message for the
/// settings UI when the transforms can't be saved.
pub fn update(
    conn: &mut Connection,
    patch: Patch,
    sync: Sync,
) -> rusqlite::Result<Result<Settings, String>> {
    let transforms = match patch.command_transforms {
        Some(transforms) => match normalize_transforms(transforms) {
            Ok(transforms) => Some(transforms),
            Err(message) => return Ok(Err(message)),
        },
        None => None,
    };
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let before = read_or_create(&tx)?;
    let mut after = before.clone();
    macro_rules! set {
        ($($field:ident),*) => {$(
            if let Some(value) = patch.$field {
                after.$field = value;
            }
        )*};
    }
    set!(
        stt_model,
        language,
        auto_refine,
        llm_model,
        smart_cleanup,
        self_correction,
        preserve_technical,
        punctuation_style,
        allow_auto_paste,
        live_text,
        voice_edits,
        expressive,
        sound_cues,
        sound_cue_volume,
        input_device_id,
        hotkey_enabled,
        chord_push_to_talk_keys,
        chord_toggle_to_talk_keys,
        chord_command_keys,
        history_retention_days,
        onboarding_completed,
        discard_audio,
        share_usage,
        shared_adapters
    );
    if let Some(transforms) = transforms {
        after.command_transforms = transforms;
    }
    if patch.history_retention_days.is_some() {
        // Choosing a window, in Settings or when asked, lets the sweep delete.
        after.history_retention_confirmed = true;
    }
    if patch.share_usage == Some(true) && !before.share_usage {
        // Turned back on: the days it was off are never sent (usage_report.py).
        after.usage_sent_through = Some(yesterday());
    }
    save(&tx, &before, &after, sync)?;
    tx.commit()?;
    Ok(Ok(after))
}

// -- history retention --------------------------------------------------------

/// Captures created before this are past a window of `days`; None keeps all.
pub fn cutoff(days: i64, at: NaiveDateTime) -> Option<NaiveDateTime> {
    (days > 0).then(|| at - Duration::days(days))
}

/// How many captures a window of `days` would delete now. The newest
/// capture, the current take, is never deleted.
pub fn count_expiring(conn: &Connection, days: i64) -> rusqlite::Result<i64> {
    let Some(before) = cutoff(days, now()) else {
        return Ok(0);
    };
    conn.query_row(
        "SELECT count(*) FROM captures WHERE created_at < ?1 AND id IS NOT ( \
             SELECT id FROM captures ORDER BY created_at DESC, id DESC LIMIT 1)",
        [stored(before)],
        |row| row.get(0),
    )
}

/// The window, whether it's confirmed, and how many captures it would
/// delete. An unconfirmed window with nothing to delete is confirmed here:
/// asking only matters when something would go.
pub fn retention_status(conn: &mut Connection, sync: Sync) -> rusqlite::Result<(i64, bool, i64)> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let before = read_or_create(&tx)?;
    let expiring = count_expiring(&tx, before.history_retention_days)?;
    let mut after = before.clone();
    if !before.history_retention_confirmed && expiring == 0 {
        after.history_retention_confirmed = true;
        save(&tx, &before, &after, sync)?;
    }
    tx.commit()?;
    Ok((
        after.history_retention_days,
        after.history_retention_confirmed,
        expiring,
    ))
}

// -- take reports -------------------------------------------------------------

/// Save how one take ended, for the daily usage report.
pub fn record_take(
    conn: &mut Connection,
    mode: &str,
    outcome: &str,
    latency_ms: Option<i64>,
    sync: Sync,
) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let at = now();
    tx.execute(
        "INSERT INTO take_reports (created_at, mode, outcome, latency_ms) VALUES (?1, ?2, ?3, ?4)",
        params![stored(at), mode, outcome, latency_ms],
    )?;
    if sync.standing_in {
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO sync_changes (kind, ref_id, deleted, changed_at) VALUES ('take', ?1, 0, ?2) \
             ON CONFLICT (kind, ref_id) DO UPDATE SET deleted = excluded.deleted, changed_at = excluded.changed_at",
            params![id.to_string(), stored(at)],
        )?;
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transform(name: &str, instruction: &str) -> Transform {
        Transform {
            id: None,
            name: name.into(),
            instruction: instruction.into(),
        }
    }

    #[test]
    fn spoken_keys_keep_words_and_apostrophes() {
        assert_eq!(spoken_key("Prompt-Engineer!"), "prompt engineer");
        assert_eq!(spoken_key("Don\u{2019}t  Stop"), "don't stop");
        assert_eq!(spoken_key("snake_case"), "snake case");
        assert_eq!(spoken_key("'quoted'"), "quoted");
        assert_eq!(spoken_key("...!"), "");
        assert_eq!(spoken_key("STRASSE straße"), "strasse strasse");
    }

    #[test]
    fn transforms_are_trimmed_and_given_ids() {
        let saved = normalize_transforms(vec![
            transform("  Polish ", " Fix it. "),
            Transform {
                id: Some(" keep ".into()),
                ..transform("Shorter", "Cut it.")
            },
        ])
        .unwrap();
        assert_eq!(saved[0].name, "Polish");
        assert_eq!(saved[0].instruction, "Fix it.");
        assert_eq!(saved[0].id.as_ref().unwrap().len(), 36);
        assert_eq!(saved[1].id.as_deref(), Some("keep"));
    }

    #[test]
    fn transforms_that_cant_be_saved_say_why() {
        let refused = |transforms: Vec<Transform>| normalize_transforms(transforms).unwrap_err();
        assert_eq!(
            refused(vec![transform(" ", "x")]),
            "Each transform needs a name and an instruction"
        );
        assert_eq!(
            refused(vec![transform(&"n".repeat(61), "x")]),
            "Transform names are at most 60 characters"
        );
        assert_eq!(
            refused(vec![transform("n", &"i".repeat(2001))]),
            "Transform instructions are at most 2000 characters"
        );
        assert_eq!(
            refused(vec![transform("!!", "x")]),
            "'!!' can't be said aloud; use letters or numbers"
        );
        assert_eq!(
            refused(vec![transform("Polish", "x"), transform("polish!", "y")]),
            "Two transforms are called 'polish!'"
        );
        assert_eq!(
            refused((0..51).map(|n| transform(&format!("t{n}"), "x")).collect()),
            "At most 50 transforms"
        );
    }

    #[test]
    fn a_window_of_zero_keeps_everything() {
        let at = NaiveDateTime::parse_from_str("2026-01-31 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert_eq!(cutoff(0, at), None);
        assert_eq!(
            cutoff(30, at),
            Some(
                NaiveDateTime::parse_from_str("2026-01-01 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap()
            )
        );
    }
}
