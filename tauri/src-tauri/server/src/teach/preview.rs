//! What a finished teach session shows: one of the user's own dictations
//! cleaned up before and after the run. Python: `_preview` and `_clean` in
//! `backend/routes/writing_style.py`.

use std::sync::Arc;

use kass_text::content_check;
use kass_text::phrase_seams::Names;
use kass_text::py;
use kass_text::refinement;
use kass_text::voice_commands::commands_alone;
use rusqlite::params;

use crate::cleanup_inputs::{self, CleanupInputs};
use crate::stream::models::{self, CleanupPrompt, Generate};
use crate::styles::{self, KASS_BUNDLE};
use crate::{learned, settings, AppState};

/// A preview needs something to restructure; shorter dictations rarely have
/// it.
const MIN_WORDS: usize = 12;
const CANDIDATES: i64 = 50;

/// What the preview cleans up, and with what.
struct Source {
    /// The newest dictation in one of the style's apps with enough words:
    /// what it became then, and what was said.
    dictation: Option<(String, String)>,
    inputs: CleanupInputs,
    names: Arc<Names>,
    model: String,
}

/// The newest dictation in `style` cleaned up before and after this run.
/// With none, `fallback` (the longest reply dictated this run) cleaned up
/// without and with the user's examples; with neither, nothing.
pub async fn preview(
    state: &AppState,
    style: &str,
    fallback: Option<String>,
) -> Result<(Option<String>, Option<String>), String> {
    let dir = learned::data_dir(state);
    let known_names = Arc::clone(&state.known_names);
    let style = style.to_string();
    let source = state
        .db
        .call(move |conn| {
            let settings = settings::get(conn)?;
            let snapshot = styles::load(conn, None)?;
            let mut query = conn.prepare(
                "SELECT transcript_raw, transcript_refined, app_bundle_id FROM captures \
                 WHERE transcript_raw != '' AND source != 'command' \
                 AND (app_bundle_id IS NULL OR app_bundle_id != ?1) \
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = query
                .query_map(params![KASS_BUNDLE, CANDIDATES], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let dictation = rows.into_iter().find_map(|(raw, refined, app)| {
                (py::split(&raw).len() >= MIN_WORDS && snapshot.for_app(app.as_deref()).id == style)
                    .then(|| {
                        (
                            refined
                                .filter(|r| !r.is_empty())
                                .unwrap_or_else(|| raw.clone()),
                            raw,
                        )
                    })
            });
            let inputs = cleanup_inputs::load(
                conn,
                &dir,
                &snapshot,
                Some(&style),
                settings.self_correction,
            )?;
            Ok(Source {
                dictation,
                inputs,
                names: known_names.get(conn)?,
                model: settings.llm_model,
            })
        })
        .await
        .map_err(|error| error.to_string())?;
    if let Some((then, raw)) = &source.dictation {
        return Ok((
            Some(then.clone()),
            Some(clean(state, &source, raw, true).await),
        ));
    }
    let Some(fallback) = fallback else {
        return Ok((None, None));
    };
    Ok((
        Some(clean(state, &source, &fallback, false).await),
        Some(clean(state, &source, &fallback, true).await),
    ))
}

/// `said` cleaned up the way dictation in the style would, without the
/// user's examples and notes unless `use_examples`; what was said when the
/// model can't.
async fn clean(state: &AppState, source: &Source, said: &str, use_examples: bool) -> String {
    let inputs = &source.inputs;
    let refined = match refine(state, source, said, use_examples).await {
        Ok(refined) => refined,
        Err(error) => {
            eprintln!("[teach] preview cleanup failed; showing what was said: {error}");
            return said.to_string();
        }
    };
    let (refined, _) = content_check::check_refinement(
        said,
        &refined,
        &inputs.flags,
        &refinement::shown_examples(&inputs.personal),
        "",
        Some(&inputs.punctuation),
    );
    refinement::style_first_word(&refined, &inputs.flags, &source.names)
}

async fn refine(
    state: &AppState,
    source: &Source,
    transcript: &str,
    use_examples: bool,
) -> Result<String, String> {
    let inputs = &source.inputs;
    if let Some(alone) = commands_alone(transcript) {
        return Ok(alone);
    }
    let flags = &inputs.flags;
    let (cleaned, explicit) =
        refinement::prepare_refinement(transcript, flags, Some(&inputs.punctuation));
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    let (personal, notes) = if use_examples {
        (inputs.personal.as_slice(), inputs.notes.as_deref())
    } else {
        (&[][..], None)
    };
    let (system, examples) =
        refinement::prompt(flags, &inputs.profile, &inputs.description, personal, notes);
    let prompt = CleanupPrompt {
        model_size: source.model.clone(),
        flags: Some(flags.to_json()),
        system,
        examples,
        cache_key: Some(refinement::cache_key(flags, use_examples, false)),
    };
    let mut done = String::new();
    for (line, breaks) in refinement::lines(&cleaned) {
        if !py::strip(line).is_empty() {
            let request = Generate {
                id: uuid::Uuid::new_v4().to_string(),
                prompt: refinement::without_final_period(line),
                hint: Some(String::new()),
                max_tokens: 2048,
                temperature: 0.2,
                cleanup: prompt.clone(),
            };
            let generated = models::generate(&state.proxy, &request, |_| {}).await?;
            done.push_str(&refinement::finish_line(&generated.text));
        }
        done.push_str(breaks);
    }
    Ok(refinement::finish(
        transcript,
        &done,
        flags,
        &inputs.profile,
        Some(&inputs.punctuation),
    ))
}
