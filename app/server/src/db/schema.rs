//! kass.db's tables: created on a new install, and an older install's
//! brought up to date, as the Python worker did until it stopped
//! (backend/database at 19662139: `models.py`, `migrations.py`).
//!
//! A new install gets each table exactly as SQLAlchemy's `create_all` wrote
//! it, text and all, so a database made here can't be told from one Python
//! made. An older one gets the columns it lacks added, with the same
//! `ALTER TABLE` Python ran, then any missing tables. Nothing is dropped,
//! no data is moved, and tables from removed features stay as they are.
//! Every step checks first, so running it again changes nothing.
//! `tests/kass_db.rs` checks both against what Python made of each version
//! of kass.db (`tests/kass_db/history.json`).

use std::collections::HashSet;

use rusqlite::{params, Connection, Transaction, TransactionBehavior};

/// A table of today's kass.db and its indexes, in SQLAlchemy's words.
struct Table {
    name: &'static str,
    create: &'static str,
    indexes: &'static [&'static str],
}

/// In the order `create_all` creates them: a table before those that refer
/// to it.
const TABLES: &[Table] = &[
    Table {
        name: "capture_settings",
        create: concat!(
            "CREATE TABLE capture_settings (\n",
            "\tid INTEGER NOT NULL, \n",
            "\tstt_model VARCHAR NOT NULL, \n",
            "\tlanguage VARCHAR NOT NULL, \n",
            "\tauto_refine BOOLEAN NOT NULL, \n",
            "\tllm_model VARCHAR NOT NULL, \n",
            "\tsmart_cleanup BOOLEAN NOT NULL, \n",
            "\tself_correction BOOLEAN NOT NULL, \n",
            "\tpreserve_technical BOOLEAN NOT NULL, \n",
            "\tpunctuation_style VARCHAR NOT NULL, \n",
            "\tallow_auto_paste BOOLEAN NOT NULL, \n",
            "\tlive_text BOOLEAN NOT NULL, \n",
            "\tvoice_edits BOOLEAN NOT NULL, \n",
            "\texpressive BOOLEAN NOT NULL, \n",
            "\tsound_cues BOOLEAN NOT NULL, \n",
            "\tsound_cue_volume FLOAT NOT NULL, \n",
            "\tinput_device_id VARCHAR, \n",
            "\thotkey_enabled BOOLEAN NOT NULL, \n",
            "\tchord_push_to_talk_keys JSON NOT NULL, \n",
            "\tchord_toggle_to_talk_keys JSON NOT NULL, \n",
            "\tchord_command_keys JSON NOT NULL, \n",
            "\tcommand_transforms JSON NOT NULL, \n",
            "\thistory_retention_days INTEGER NOT NULL, \n",
            "\thistory_retention_confirmed BOOLEAN NOT NULL, \n",
            "\tonboarding_completed BOOLEAN NOT NULL, \n",
            "\tdiscard_audio BOOLEAN NOT NULL, \n",
            "\tshare_usage BOOLEAN NOT NULL, \n",
            "\tusage_device_id VARCHAR, \n",
            "\tusage_sent_through VARCHAR, \n",
            "\tshared_adapters BOOLEAN NOT NULL, \n",
            "\tupdated_at DATETIME, \n",
            "\tPRIMARY KEY (id)\n",
            ")",
        ),
        indexes: &[],
    },
    Table {
        name: "captures",
        create: concat!(
            "CREATE TABLE captures (\n",
            "\tid VARCHAR NOT NULL, \n",
            "\taudio_path VARCHAR NOT NULL, \n",
            "\tsource VARCHAR NOT NULL, \n",
            "\tlanguage VARCHAR, \n",
            "\tduration_ms INTEGER, \n",
            "\ttranscript_raw TEXT NOT NULL, \n",
            "\ttranscript_refined TEXT, \n",
            "\tstt_model VARCHAR, \n",
            "\tllm_model VARCHAR, \n",
            "\trefinement_flags TEXT, \n",
            "\trefinement_review TEXT, \n",
            "\tapp_bundle_id VARCHAR, \n",
            "\tapp_name VARCHAR, \n",
            "\tcommand_selection TEXT, \n",
            "\tcommand_instruction TEXT, \n",
            "\tcommand_transform VARCHAR, \n",
            "\tstyle_id VARCHAR, \n",
            "\tapp_category VARCHAR, \n",
            "\tteaches_style_id VARCHAR, \n",
            "\tmac_id VARCHAR, \n",
            "\tmac_name VARCHAR, \n",
            "\taudio_deleted BOOLEAN NOT NULL, \n",
            "\tprosody TEXT, \n",
            "\tcreated_at DATETIME, \n",
            "\tPRIMARY KEY (id)\n",
            ")",
        ),
        indexes: &[],
    },
    Table {
        name: "dictionary_entries",
        create: concat!(
            "CREATE TABLE dictionary_entries (\n",
            "\tid VARCHAR NOT NULL, \n",
            "\tscope VARCHAR NOT NULL, \n",
            "\tscope_id VARCHAR NOT NULL, \n",
            "\tapp_name VARCHAR, \n",
            "\twritten VARCHAR NOT NULL, \n",
            "\tspoken VARCHAR, \n",
            "\t\"key\" VARCHAR NOT NULL, \n",
            "\tgroup_id VARCHAR, \n",
            "\tmatch_sound BOOLEAN DEFAULT '1' NOT NULL, \n",
            "\tsource VARCHAR, \n",
            "\tadded_by VARCHAR, \n",
            "\tphrase BOOLEAN DEFAULT '0' NOT NULL, \n",
            "\tcreated_at DATETIME, \n",
            "\tupdated_at DATETIME, \n",
            "\tPRIMARY KEY (id), \n",
            "\tUNIQUE (scope, scope_id, \"key\")\n",
            ")",
        ),
        indexes: &["CREATE INDEX ix_dictionary_entries_group_id ON dictionary_entries (group_id)"],
    },
    Table {
        name: "known_names",
        create: concat!(
            "CREATE TABLE known_names (\n",
            "\tname VARCHAR NOT NULL, \n",
            "\tlast_seen_at DATETIME NOT NULL, \n",
            "\tPRIMARY KEY (name)\n",
            ")",
        ),
        indexes: &[],
    },
    Table {
        name: "retired_captures",
        create: concat!(
            "CREATE TABLE retired_captures (\n",
            "\tcapture_id VARCHAR NOT NULL, \n",
            "\tcreated_at DATETIME NOT NULL, \n",
            "\tsource VARCHAR NOT NULL, \n",
            "\tapp_bundle_id VARCHAR, \n",
            "\twords INTEGER NOT NULL, \n",
            "\traw_words INTEGER NOT NULL, \n",
            "\tduration_ms INTEGER, \n",
            "\tfixed BOOLEAN NOT NULL, \n",
            "\tPRIMARY KEY (capture_id)\n",
            ")",
        ),
        indexes: &["CREATE INDEX ix_retired_captures_created_at ON retired_captures (created_at)"],
    },
    Table {
        name: "sync_changes",
        create: concat!(
            "CREATE TABLE sync_changes (\n",
            "\tkind VARCHAR NOT NULL, \n",
            "\tref_id VARCHAR NOT NULL, \n",
            "\tdeleted BOOLEAN NOT NULL, \n",
            "\tchanged_at DATETIME NOT NULL, \n",
            "\tPRIMARY KEY (kind, ref_id)\n",
            ")",
        ),
        indexes: &[],
    },
    Table {
        name: "take_reports",
        create: concat!(
            "CREATE TABLE take_reports (\n",
            "\tid INTEGER NOT NULL, \n",
            "\tcreated_at DATETIME NOT NULL, \n",
            "\tmode VARCHAR NOT NULL, \n",
            "\toutcome VARCHAR NOT NULL, \n",
            "\tlatency_ms INTEGER, \n",
            "\tPRIMARY KEY (id)\n",
            ")",
        ),
        indexes: &["CREATE INDEX ix_take_reports_created_at ON take_reports (created_at)"],
    },
    Table {
        name: "writing_styles",
        create: concat!(
            "CREATE TABLE writing_styles (\n",
            "\tid VARCHAR NOT NULL, \n",
            "\tname VARCHAR NOT NULL, \n",
            "\tposition INTEGER NOT NULL, \n",
            "\tis_default BOOLEAN NOT NULL, \n",
            "\tpunctuation_style VARCHAR NOT NULL, \n",
            "\tcapitalize_first BOOLEAN NOT NULL, \n",
            "\tsmart_cleanup BOOLEAN NOT NULL, \n",
            "\tpreserve_technical BOOLEAN NOT NULL, \n",
            "\tdescription TEXT, \n",
            "\tcreated_at DATETIME, \n",
            "\tupdated_at DATETIME, \n",
            "\tPRIMARY KEY (id)\n",
            ")",
        ),
        indexes: &[],
    },
    Table {
        name: "app_styles",
        create: concat!(
            "CREATE TABLE app_styles (\n",
            "\tbundle_id VARCHAR NOT NULL, \n",
            "\tapp_name VARCHAR, \n",
            "\tstyle_id VARCHAR NOT NULL, \n",
            "\tupdated_at DATETIME, \n",
            "\tPRIMARY KEY (bundle_id), \n",
            "\tFOREIGN KEY(style_id) REFERENCES writing_styles (id)\n",
            ")",
        ),
        indexes: &[],
    },
    Table {
        name: "capture_feedback",
        create: concat!(
            "CREATE TABLE capture_feedback (\n",
            "\tid VARCHAR NOT NULL, \n",
            "\tcapture_id VARCHAR NOT NULL, \n",
            "\ttarget VARCHAR NOT NULL, \n",
            "\texpected_text TEXT NOT NULL, \n",
            "\tnotes TEXT NOT NULL, \n",
            "\tsnapshot TEXT NOT NULL, \n",
            "\tsource VARCHAR DEFAULT 'manual' NOT NULL, \n",
            "\tfiled_by VARCHAR, \n",
            "\tapp_bundle_id VARCHAR, \n",
            "\tteaches_style_id VARCHAR, \n",
            "\tcreated_at DATETIME, \n",
            "\tPRIMARY KEY (id), \n",
            "\tFOREIGN KEY(capture_id) REFERENCES captures (id)\n",
            ")",
        ),
        indexes: &["CREATE INDEX ix_capture_feedback_capture_id ON capture_feedback (capture_id)"],
    },
];

/// The columns each table gained after it first shipped, in the order
/// Python added them, each in the words of its `ALTER TABLE ... ADD COLUMN`.
/// A column's `DEFAULT` is what an existing row gets: an install from before
/// onboarding existed was already set up, so it never sees onboarding.
const ADDED_COLUMNS: &[(&str, &[(&str, &str)])] = &[
    (
        "capture_settings",
        &[
            (
                "allow_auto_paste",
                "allow_auto_paste BOOLEAN NOT NULL DEFAULT 1",
            ),
            ("input_device_id", "input_device_id VARCHAR"),
            (
                "chord_push_to_talk_keys",
                r#"chord_push_to_talk_keys TEXT NOT NULL DEFAULT '["MetaRight", "AltGr"]'"#,
            ),
            (
                "chord_toggle_to_talk_keys",
                r#"chord_toggle_to_talk_keys TEXT NOT NULL DEFAULT '["MetaRight", "AltGr", "Space"]'"#,
            ),
            (
                "hotkey_enabled",
                "hotkey_enabled BOOLEAN NOT NULL DEFAULT 0",
            ),
            ("live_text", "live_text BOOLEAN NOT NULL DEFAULT 0"),
            ("voice_edits", "voice_edits BOOLEAN NOT NULL DEFAULT 1"),
            ("expressive", "expressive BOOLEAN NOT NULL DEFAULT 1"),
            ("sound_cues", "sound_cues BOOLEAN NOT NULL DEFAULT 1"),
            (
                "sound_cue_volume",
                "sound_cue_volume FLOAT NOT NULL DEFAULT 0.5",
            ),
            (
                "chord_command_keys",
                r#"chord_command_keys TEXT NOT NULL DEFAULT '["MetaRight", "ShiftRight"]'"#,
            ),
            (
                "command_transforms",
                concat!(
                    "command_transforms TEXT NOT NULL DEFAULT '[",
                    r#"{"id": "polish", "name": "Polish", "instruction": "Fix grammar, spelling and punctuation, and smooth out awkward phrasing so it reads well. Keep my words, tone and meaning, and don''t add anything new."}, "#,
                    r#"{"id": "prompt-engineer", "name": "Prompt Engineer", "instruction": "Rewrite this as a clear, well-structured prompt for an AI assistant: the goal, the relevant context, any constraints, and the output I expect. Keep every requirement I mentioned and don''t invent new ones."}"#,
                    "]'",
                ),
            ),
            (
                "punctuation_style",
                "punctuation_style VARCHAR NOT NULL DEFAULT 'standard'",
            ),
            (
                "history_retention_days",
                "history_retention_days INTEGER NOT NULL DEFAULT 30",
            ),
            // Existing installs are asked before their first deletion.
            (
                "history_retention_confirmed",
                "history_retention_confirmed BOOLEAN NOT NULL DEFAULT 0",
            ),
            (
                "onboarding_completed",
                "onboarding_completed BOOLEAN NOT NULL DEFAULT 1",
            ),
            ("discard_audio", "discard_audio BOOLEAN NOT NULL DEFAULT 0"),
            ("share_usage", "share_usage BOOLEAN NOT NULL DEFAULT 1"),
            ("usage_device_id", "usage_device_id VARCHAR"),
            ("usage_sent_through", "usage_sent_through VARCHAR"),
            (
                "shared_adapters",
                "shared_adapters BOOLEAN NOT NULL DEFAULT 1",
            ),
        ],
    ),
    (
        "captures",
        &[
            ("refinement_review", "refinement_review TEXT"),
            ("app_bundle_id", "app_bundle_id VARCHAR"),
            ("app_name", "app_name VARCHAR"),
            ("command_selection", "command_selection TEXT"),
            ("command_instruction", "command_instruction TEXT"),
            ("command_transform", "command_transform VARCHAR"),
            ("style_id", "style_id VARCHAR"),
            ("app_category", "app_category VARCHAR"),
            ("teaches_style_id", "teaches_style_id VARCHAR"),
            ("audio_deleted", "audio_deleted BOOLEAN NOT NULL DEFAULT 0"),
            ("prosody", "prosody TEXT"),
            ("mac_id", "mac_id VARCHAR"),
            ("mac_name", "mac_name VARCHAR"),
        ],
    ),
    (
        "capture_feedback",
        &[
            ("app_bundle_id", "app_bundle_id VARCHAR"),
            ("teaches_style_id", "teaches_style_id VARCHAR"),
            ("source", "source VARCHAR NOT NULL DEFAULT 'manual'"),
            ("filed_by", "filed_by VARCHAR"),
        ],
    ),
    (
        "dictionary_entries",
        &[
            ("group_id", "group_id VARCHAR"),
            // Existing entries keep matching by sound, as they always have.
            ("match_sound", "match_sound BOOLEAN NOT NULL DEFAULT 1"),
            ("source", "source VARCHAR"),
            ("added_by", "added_by VARCHAR"),
            ("phrase", "phrase BOOLEAN NOT NULL DEFAULT 0"),
            ("updated_at", "updated_at DATETIME"),
        ],
    ),
    (
        "writing_styles",
        &[
            ("description", "description TEXT"),
            ("updated_at", "updated_at DATETIME"),
        ],
    ),
];

/// Kass was Herga (com.mrgnhnt.herga), and before that Voicebox, with
/// bundle id sh.voicebox.app and then, briefly, com.mrgnhnt.voicebox. Data
/// recorded against its own window keeps meaning Kass under the new one.
const OLD_BUNDLES: &[&str] = &[
    "sh.voicebox.app",
    "com.mrgnhnt.voicebox",
    "com.mrgnhnt.herga",
];
const BUNDLE: &str = "com.mrgnhnt.kass";

/// Bring kass.db up to today's tables, in one transaction: added columns,
/// Kass's renamed bundle id, then missing tables.
pub(crate) fn migrate(connection: &mut Connection) -> rusqlite::Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let tables = table_names(&tx)?;
    for (table, added) in ADDED_COLUMNS {
        if !tables.contains(*table) {
            continue;
        }
        let columns = column_names(&tx, table)?;
        for (column, definition) in *added {
            if !columns.contains(*column) {
                tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition}"))?;
            }
        }
    }
    rename_own_bundle_id(&tx, &tables)?;
    for table in TABLES {
        if tables.contains(table.name) {
            continue;
        }
        tx.execute_batch(table.create)?;
        for index in table.indexes {
            tx.execute_batch(index)?;
        }
    }
    tx.commit()
}

fn rename_own_bundle_id(tx: &Transaction, tables: &HashSet<String>) -> rusqlite::Result<()> {
    for old in OLD_BUNDLES {
        for table in ["captures", "capture_feedback", "retired_captures"] {
            if tables.contains(table) && column_names(tx, table)?.contains("app_bundle_id") {
                tx.execute(
                    &format!("UPDATE {table} SET app_bundle_id = ?1 WHERE app_bundle_id = ?2"),
                    params![BUNDLE, old],
                )?;
            }
        }
        // Keyed tables: a row already under the new id wins over the old one.
        if tables.contains("app_styles") {
            tx.execute(
                "UPDATE OR IGNORE app_styles SET bundle_id = ?1 WHERE bundle_id = ?2",
                params![BUNDLE, old],
            )?;
            tx.execute("DELETE FROM app_styles WHERE bundle_id = ?1", params![old])?;
        }
        if tables.contains("dictionary_entries") {
            tx.execute(
                "UPDATE OR IGNORE dictionary_entries SET scope_id = ?1 \
                 WHERE scope = 'app' AND scope_id = ?2",
                params![BUNDLE, old],
            )?;
            tx.execute(
                "DELETE FROM dictionary_entries WHERE scope = 'app' AND scope_id = ?1",
                params![old],
            )?;
        }
    }
    Ok(())
}

fn table_names(connection: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names = statement.query_map([], |row| row.get(0))?;
    names.collect()
}

fn column_names(connection: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = connection.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let names = statement.query_map([table], |row| row.get(0))?;
    names.collect()
}
