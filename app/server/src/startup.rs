//! What the worker loads and warms when dictation starts on this Mac
//! (`POST /worker/models/startup`, `backend/services/model_startup.py`): the
//! saved models, and the cleanup prompts dictation will send, so the first
//! dictation reuses them from the model's cache.

use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::cleanup_inputs;
use crate::settings;
use crate::stream::session::cleanup_prompt;
use crate::styles;

/// The startup request for the settings and styles in `conn`; `data_dir` is
/// the folder kass.db is in. Its prompts are the default style's first,
/// which the worker runs once, then those of the other styles apps are in,
/// which it caches in the background. None when cleanup is off.
pub fn body(
    conn: &mut Connection,
    data_dir: &Path,
    teaching: Option<String>,
) -> rusqlite::Result<Value> {
    let saved = settings::get(conn)?;
    let mut prompts = Vec::new();
    if saved.auto_refine {
        let snapshot = styles::load(conn, teaching)?;
        let default = snapshot.default_style().id.clone();
        let assigned: Vec<&str> = snapshot.apps.iter().map(|(_, id)| id.as_str()).collect();
        let others = snapshot
            .styles
            .iter()
            .filter(|style| style.id != default && assigned.contains(&style.id.as_str()))
            .map(|style| style.id.clone());
        for id in std::iter::once(default.clone()).chain(others) {
            let inputs =
                cleanup_inputs::load(conn, data_dir, &snapshot, Some(&id), saved.self_correction)?;
            prompts.push(cleanup_prompt(&inputs, &saved.llm_model));
        }
    }
    Ok(json!({
        "stt_model": saved.stt_model,
        "llm_model": saved.llm_model,
        "auto_refine": saved.auto_refine,
        "language": saved.language,
        "shared_adapters": saved.shared_adapters,
        "prompts": prompts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data folder whose kass.db has three styles, one of them in an app.
    fn database() -> (Dir, Connection) {
        let dir = Dir(std::env::temp_dir().join(format!("kass-startup-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("kass.db");
        crate::db::prepare(&path).unwrap();
        crate::db::seed(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "UPDATE writing_styles SET description = 'Short and warm.' WHERE is_default = 1;
             INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
                 capitalize_first, smart_cleanup, preserve_technical, description)
             VALUES ('chat', 'Chat', 1, 0, 'minimal', 1, 1, 1, NULL),
                    ('unused', 'Unused', 2, 0, 'standard', 1, 1, 1, NULL);
             INSERT INTO app_styles (bundle_id, app_name, style_id)
             VALUES ('com.tinyspeck.slackmacgap', 'Slack', 'chat');",
        )
        .unwrap();
        (dir, conn)
    }

    /// A folder of its own, gone when dropped.
    struct Dir(std::path::PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sends_the_saved_models_and_the_prompts_dictation_uses() {
        let (dir, mut conn) = database();
        let sent = body(&mut conn, &dir.0, None).unwrap();
        let saved = settings::get(&mut conn).unwrap();
        assert_eq!(sent["stt_model"], json!(saved.stt_model));
        assert_eq!(sent["llm_model"], json!(saved.llm_model));
        assert_eq!(sent["language"], json!(saved.language));
        assert_eq!(sent["shared_adapters"], json!(saved.shared_adapters));
        let prompts = sent["prompts"].as_array().unwrap();
        // The default style, then the styles apps are in; never an unused one.
        let styles: Vec<_> = prompts.iter().map(|p| &p["flags"]["style"]).collect();
        let snapshot = styles::load(&conn, None).unwrap();
        assert_eq!(
            styles,
            [&json!(snapshot.default_style().id), &json!("chat")]
        );
        // The same prompt a dictation in that style builds, cache key and all.
        let inputs = cleanup_inputs::load(
            &conn,
            &dir.0,
            &snapshot,
            Some("chat"),
            saved.self_correction,
        )
        .unwrap();
        let chat = cleanup_prompt(&inputs, &saved.llm_model);
        assert_eq!(prompts[1], serde_json::to_value(&chat).unwrap());
        assert!(prompts[0]["system"]
            .as_str()
            .unwrap()
            .contains("Short and warm."));
    }

    #[test]
    fn sends_no_prompts_with_cleanup_off() {
        let (dir, mut conn) = database();
        conn.execute("UPDATE capture_settings SET auto_refine = 0", [])
            .unwrap();
        let sent = body(&mut conn, &dir.0, None).unwrap();
        assert_eq!(sent["auto_refine"], json!(false));
        assert_eq!(sent["prompts"], json!([]));
    }
}
