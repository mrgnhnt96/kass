//! Correction notes: short rules summarized from a writing style's older
//! examples, kept in `correction-notes.json` (`learned`).
//!
//! Cleanup shows the model only a style's most recent examples, so without
//! these an example would stop counting once enough newer ones pushed it
//! out. While the models are idle, this job asks the cleanup model to fold
//! the oldest examples out of the prompt into the style's rules, replays the
//! examples with the new rules, and keeps them only when cleanup lands
//! closer to what was meant. The worker only generates; the prompt, the
//! choice and the file are this server's. Python:
//! `backend/services/correction_notes.py`.

use std::collections::HashSet;
use std::time::Duration;

use kass_text::correction_rules::loss;
use kass_text::py;
use kass_text::re;
use kass_text::refinement;
use kass_text::voice_commands::commands_alone;
use serde_json::{json, Value};

use crate::cleanup_inputs::{self, CleanupInputs, PersonalExample};
use crate::proxy::Proxy;
use crate::stream::models::{self, CleanupPrompt, Generate};
use crate::{learned, settings, styles, worker, AppState};

/// Fewest examples out of the prompt worth a summary.
pub const MIN_BATCH: usize = 10;
/// Most examples one summary takes, oldest first.
const MAX_BATCH: usize = 20;
/// Already summarized examples replayed too, so new rules can't undo what
/// older ones taught.
const RETAINED_CHECKS: usize = 8;
/// How much closer one more rule must bring cleanup, in edits, to be kept.
const MIN_GAIN: usize = 2;
const MAX_NOTES: usize = 12;
const MAX_NOTE_CHARS: usize = 160;
/// A rule sharing this many words in a row with an example restates it.
const COPIED_WORDS: usize = 5;

/// How often the job looks for a style to summarize.
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// How long the job rests after it failed.
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

const SUMMARY_PROMPT: &str = "\
You keep a short list of rules for cleaning up one person's dictation.

The user message has the current rules, then new examples of what the speaker said out loud and what they actually meant to write. Return the updated list of rules.
- A rule describes a habit that applies to anything the speaker might dictate: words they drop or replace, how they reorder or restructure, how they format lists, spellings they always want.
- Never write a rule about one example. Good: \"Turn items said in a row into a list, one item per line.\" Bad: \"If the speaker says 'here is a list of groceries', format it as a list.\"
- Never quote more than three words from an example.
- Keep a current rule unless an example shows it is wrong. Merge rules that say the same thing.
- Each rule is one short sentence. At most {max_notes} rules.
- Reply with only the rules, one per line, each starting with \"- \".";

/// The system prompt for a summary.
pub fn summary_prompt() -> String {
    SUMMARY_PROMPT.replace("{max_notes}", &MAX_NOTES.to_string())
}

/// The user message for a summary of `batch` into the `current` rules.
pub fn summary_request(current: &[String], batch: &[PersonalExample]) -> String {
    let rules = if current.is_empty() {
        "(none yet)".to_string()
    } else {
        current
            .iter()
            .map(|note| format!("- {note}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let examples = batch
        .iter()
        .map(|example| format!("Said: {}\nMeant: {}", example.said, example.meant))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("Current rules:\n{rules}\n\nNew examples:\n\n{examples}")
}

/// Whether `note` is about exclamation marks. "!" is written only where the
/// speaker said it with more energy (`prosody`) or taught it with their own
/// edits; a rule would have cleanup add it where none was heard. Such a note
/// is never learned, and one saved before is dropped when the notes are read.
pub fn about_exclamation(note: &str) -> bool {
    note.contains('!') || py::casefold(note).contains("exclamation")
}

/// Every `COPIED_WORDS` words in a row in `text`, casefolded.
fn runs(text: &str) -> HashSet<Vec<String>> {
    let folded = py::casefold(text);
    let words: Vec<String> = re!(r"\w+")
        .findall(&folded)
        .into_iter()
        .map(String::from)
        .collect();
    words
        .windows(COPIED_WORDS)
        .map(<[String]>::to_vec)
        .collect()
}

/// The general rules in a summary reply. Anything that isn't a short rule is
/// dropped, and so is a rule that copies one of `examples` instead of
/// describing a habit.
pub fn parse(reply: &str, examples: &[PersonalExample]) -> Vec<String> {
    let mut copied = HashSet::new();
    for example in examples {
        copied.extend(runs(&example.said));
        copied.extend(runs(&example.meant));
    }
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();
    for line in py::splitlines(reply) {
        let Some(found) = re!(r"\s*(?:[-*•]|\d+[.)])\s+(.+)").match_(line) else {
            continue;
        };
        let Some(rule) = found.get(1) else {
            continue;
        };
        let note = py::strip(py::strip_chars(py::strip(rule.as_str()), "\""));
        let key = py::casefold(note);
        if note.is_empty()
            || py::len(note) > MAX_NOTE_CHARS
            || seen.contains(&key)
            || about_exclamation(note)
            || !runs(note).is_disjoint(&copied)
        {
            continue;
        }
        seen.insert(key);
        parsed.push(note.to_string());
    }
    parsed.truncate(MAX_NOTES);
    parsed
}

/// What a summary runs on: the cleanup model, and whether a dictation came
/// since the job started.
#[allow(async_fn_in_trait)]
pub(crate) trait Model {
    /// The reply to a summary request, from the base model.
    async fn summarize(&mut self, system: &str, prompt: &str) -> Result<String, String>;
    /// The style's cleanup of `said`, with `notes` as its rules.
    async fn clean(&mut self, said: &str, notes: &[String]) -> Result<String, String>;
    async fn interrupted(&mut self) -> bool;
}

/// A finished summary, to save.
#[derive(Debug, PartialEq)]
pub(crate) struct Summary {
    pub chosen: Vec<String>,
    pub metrics: Value,
}

/// How far cleanup with `notes` lands from what was meant, summed over
/// `checks`; None when a dictation interrupts.
async fn distance(
    model: &mut impl Model,
    checks: &[PersonalExample],
    notes: &[String],
) -> Result<Option<usize>, String> {
    let mut total = 0;
    for example in checks {
        if model.interrupted().await {
            return Ok(None);
        }
        let text = model.clean(&example.said, notes).await?;
        total += loss(&text, &example.meant);
    }
    Ok(Some(total))
}

/// Fold `batch` (the oldest examples out of the prompt) into the `current`
/// rules. `retained` are examples earlier summaries took, replayed too.
/// None when a dictation interrupts.
pub(crate) async fn summarize(
    model: &mut impl Model,
    current: &[String],
    batch: &[PersonalExample],
    retained: &[PersonalExample],
) -> Result<Option<Summary>, String> {
    let reply = model
        .summarize(&summary_prompt(), &summary_request(current, batch))
        .await?;
    let candidate = parse(&reply, batch);
    if model.interrupted().await {
        return Ok(None);
    }
    let checks: Vec<PersonalExample> = batch.iter().chain(retained).cloned().collect();
    let Some(baseline) = distance(model, &checks, current).await? else {
        return Ok(None);
    };
    let (mut chosen, mut best) = (current.to_vec(), baseline);
    if !candidate.is_empty() && candidate != current {
        let Some(whole) = distance(model, &checks, &candidate).await? else {
            return Ok(None);
        };
        if whole <= baseline {
            (chosen, best) = (candidate, whole);
        } else {
            // One bad rule can sink a list of good ones, so add the new
            // rules one at a time, each kept only when it clearly helps.
            for note in candidate {
                if chosen.contains(&note) || chosen.len() >= MAX_NOTES {
                    continue;
                }
                let trial: Vec<String> = chosen.iter().cloned().chain([note]).collect();
                let Some(distance) = distance(model, &checks, &trial).await? else {
                    return Ok(None);
                };
                if distance + MIN_GAIN <= best {
                    (chosen, best) = (trial, distance);
                }
            }
        }
    }
    let unchanged: usize = checks
        .iter()
        .map(|example| loss(&example.said, &example.meant))
        .sum();
    let metrics = json!({
        "examples": batch.len(),
        "checks": checks.len(),
        "baseline_distance": baseline,
        "chosen_distance": best,
        "unchanged_distance": unchanged,
    });
    Ok(Some(Summary { chosen, metrics }))
}

// -- the job ---------------------------------------------------------------------

/// One style's summary to run.
struct Work {
    style_id: String,
    current: Vec<String>,
    batch: Vec<PersonalExample>,
    retained: Vec<PersonalExample>,
    inputs: CleanupInputs,
    model: String,
}

/// The cleanup model in the worker, as a summary uses it.
struct InWorker<'a> {
    proxy: &'a Proxy,
    inputs: &'a CleanupInputs,
    model: &'a str,
    /// The worker's foreground generation when the job started.
    generation: u64,
}

impl Model for InWorker<'_> {
    async fn summarize(&mut self, system: &str, prompt: &str) -> Result<String, String> {
        let request = Generate {
            id: uuid::Uuid::new_v4().to_string(),
            prompt: prompt.to_string(),
            hint: None,
            max_tokens: 1024,
            temperature: 0.2,
            cleanup: CleanupPrompt {
                model_size: self.model.to_string(),
                flags: None,
                system: system.to_string(),
                examples: Vec::new(),
                cache_key: None,
            },
        };
        Ok(models::generate_in_background(self.proxy, &request)
            .await?
            .text)
    }

    /// As a dictation's cleanup (`stream::session`), with `notes` for the
    /// style's own and a prompt cache of its own.
    async fn clean(&mut self, said: &str, notes: &[String]) -> Result<String, String> {
        if let Some(alone) = commands_alone(said) {
            return Ok(alone);
        }
        let inputs = self.inputs;
        let flags = &inputs.flags;
        let (cleaned, explicit) =
            refinement::prepare_refinement(said, flags, Some(&inputs.punctuation));
        if let Some(explicit) = explicit {
            return Ok(explicit);
        }
        let section = cleanup_inputs::notes_section(notes);
        let (system, examples) = refinement::prompt(
            flags,
            &inputs.profile,
            &inputs.description,
            &inputs.personal,
            section.as_deref(),
        );
        let cleanup = CleanupPrompt {
            model_size: self.model.to_string(),
            flags: Some(flags.to_json()),
            system,
            examples,
            cache_key: Some(refinement::cache_key(flags, true, true)),
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
                    cleanup: cleanup.clone(),
                };
                let generated = models::generate_in_background(self.proxy, &request).await?;
                done.push_str(&refinement::finish_line(&generated.text));
            }
            done.push_str(breaks);
        }
        Ok(refinement::finish(
            said,
            &done,
            flags,
            &inputs.profile,
            Some(&inputs.punctuation),
        ))
    }

    async fn interrupted(&mut self) -> bool {
        worker::foreground_generation(self.proxy).await != Some(self.generation)
    }
}

/// The first style with a batch to summarize, when cleanup is on.
fn find_work(
    conn: &mut rusqlite::Connection,
    dir: &std::path::Path,
    teaching: Option<String>,
) -> rusqlite::Result<Option<Work>> {
    let saved = settings::get(conn)?;
    if !saved.auto_refine {
        return Ok(None);
    }
    let snapshot = styles::load(conn, teaching)?;
    let writing = cleanup_inputs::read_writing_style(dir);
    let notes = learned::read_notes(dir);
    for style in &snapshot.styles {
        let all = cleanup_inputs::all_examples(conn, &writing, &snapshot, &style.id)?;
        let mut batch = learned::pending(&notes, &style.id, &all);
        if batch.len() < MIN_BATCH {
            continue;
        }
        batch.truncate(MAX_BATCH);
        let considered = learned::considered(&notes, &style.id);
        let retained = all
            .into_iter()
            .filter(|example| considered.contains(&example.id))
            .take(RETAINED_CHECKS)
            .collect();
        let inputs =
            cleanup_inputs::load(conn, dir, &snapshot, Some(&style.id), saved.self_correction)?;
        return Ok(Some(Work {
            style_id: style.id.clone(),
            current: learned::current_notes(&notes, &style.id),
            batch,
            retained,
            inputs,
            model: saved.llm_model.clone(),
        }));
    }
    Ok(None)
}

/// Summarize one style's examples if the models are idle and enough have
/// left its prompt. The saved outcome and metrics, or None when there was
/// nothing to do or a dictation interrupted.
pub async fn run_once(state: &AppState) -> Result<Option<Value>, String> {
    let Some(generation) = worker::idle_generation(&state.proxy).await else {
        return Ok(None);
    };
    let teaching = state.teach.teaching_style();
    let dir = learned::data_dir(state);
    let work = state
        .db
        .call(move |conn| find_work(conn, &dir, teaching))
        .await
        .map_err(|error| error.to_string())?;
    let Some(work) = work else {
        return Ok(None);
    };
    let mut model = InWorker {
        proxy: &state.proxy,
        inputs: &work.inputs,
        model: &work.model,
        generation,
    };
    let Some(summary) = summarize(&mut model, &work.current, &work.batch, &work.retained).await?
    else {
        return Ok(None);
    };
    let batch: Vec<String> = work
        .batch
        .iter()
        .map(|example| example.id.clone())
        .collect();
    let at = now();
    let style_id = work.style_id.clone();
    let metrics = summary.metrics.clone();
    let updated = learned::change(state, None, move |_, files| {
        Ok(files.summarized(&style_id, &summary.chosen, &batch, metrics, &at))
    })
    .await?;
    let outcome = if updated { "updated" } else { "no_change" };
    eprintln!(
        "[notes] correction notes for {} {}: {}",
        work.style_id,
        if updated { "updated" } else { "unchanged" },
        summary.metrics
    );
    Ok(Some(
        json!({ "style": work.style_id, "outcome": outcome, "metrics": summary.metrics }),
    ))
}

/// Python's `datetime.now(UTC).isoformat()`.
fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Look for a style to summarize every minute. A stand-in summarizes
/// nothing: the shared Kass learns.
pub async fn watch(state: AppState) {
    if state.sync.standing_in {
        return;
    }
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        if !state.db.path().exists() {
            continue;
        }
        if let Err(error) = run_once(&state).await {
            eprintln!(
                "[notes] the correction notes job failed; keeping the current notes: {error}"
            );
            tokio::time::sleep(RETRY_AFTER).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "Write Kass as one word.";
    const BAD: &str = "Never say Kass.";

    fn example(id: &str, said: &str, meant: &str) -> PersonalExample {
        PersonalExample {
            id: id.into(),
            source: "correction",
            said: said.into(),
            meant: meant.into(),
            created_at: None,
            app_bundle_id: None,
            app_name: None,
        }
    }

    fn kass_corrections(range: std::ops::Range<usize>) -> Vec<PersonalExample> {
        range
            .map(|i| {
                example(
                    &format!("correction:f{i}"),
                    &format!("open cass number {i}"),
                    &format!("open Kass number {i}"),
                )
            })
            .collect()
    }

    /// Summarizes to `reply`; cleans up "cass" only when a rule says to.
    struct Fake {
        reply: String,
        summaries: Vec<String>,
        interrupted: bool,
    }

    fn fake(reply: &str) -> Fake {
        Fake {
            reply: reply.into(),
            summaries: Vec::new(),
            interrupted: false,
        }
    }

    impl Model for Fake {
        async fn summarize(&mut self, system: &str, prompt: &str) -> Result<String, String> {
            assert!(system.starts_with("You keep a short list of rules"));
            self.summaries.push(prompt.to_string());
            Ok(self.reply.clone())
        }

        async fn clean(&mut self, said: &str, notes: &[String]) -> Result<String, String> {
            let mut text = said.to_string();
            if notes.iter().any(|n| n == BAD) {
                text = text.replace("cass", "the desktop dictation app");
            }
            if notes.iter().any(|n| n == RULE) {
                text = text.replace("cass", "Kass");
            }
            Ok(text)
        }

        async fn interrupted(&mut self) -> bool {
            self.interrupted
        }
    }

    #[tokio::test]
    async fn a_rule_that_helps_is_kept() {
        let mut model = fake(&format!("Here are the rules:\n- {RULE}\n- {RULE}"));
        let summary = summarize(&mut model, &[], &kass_corrections(0..3), &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.chosen, vec![RULE.to_string()]);
        assert_eq!(summary.metrics["examples"], 3);
        assert_eq!(summary.metrics["chosen_distance"], 0);
        assert!(model.summaries[0].starts_with("Current rules:\n(none yet)\n\nNew examples:\n\nSaid: open cass number 0\nMeant: open Kass number 0\n\nSaid:"));
    }

    #[tokio::test]
    async fn a_rule_that_makes_cleanup_worse_is_rejected() {
        let mut model = fake(&format!("- {BAD}"));
        let summary = summarize(&mut model, &[], &kass_corrections(0..3), &[])
            .await
            .unwrap()
            .unwrap();
        assert!(summary.chosen.is_empty());
    }

    #[tokio::test]
    async fn a_bad_rule_does_not_sink_a_good_one() {
        let mut model = fake(&format!("- {BAD}\n- {RULE}"));
        let summary = summarize(&mut model, &[], &kass_corrections(0..3), &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.chosen, vec![RULE.to_string()]);
    }

    #[tokio::test]
    async fn older_lessons_are_checked_when_the_rules_change() {
        let batch: Vec<PersonalExample> = (5..8)
            .map(|i| {
                example(
                    &format!("correction:f{i}"),
                    &format!("the build is broken {i}"),
                    &format!("The build is broken {i}"),
                )
            })
            .collect();
        // Dropping the old rule would undo what earlier corrections taught.
        let mut model = fake("- Capitalize the first word.");
        let summary = summarize(&mut model, &[RULE.into()], &batch, &kass_corrections(0..3))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.chosen, vec![RULE.to_string()]);
        assert_eq!(summary.metrics["checks"], 6);
        assert!(model.summaries[0].starts_with(&format!("Current rules:\n- {RULE}\n\n")));
    }

    #[tokio::test]
    async fn a_dictation_interrupts_the_summary() {
        let mut model = fake(&format!("- {RULE}"));
        model.interrupted = true;
        let summary = summarize(&mut model, &[], &kass_corrections(0..3), &[])
            .await
            .unwrap();
        assert_eq!(summary, None);
    }

    #[test]
    fn parse_keeps_short_rules_only() {
        let reply = format!(
            "Rules:\n- Drop 'basically'.\n* Use numbered lists.\n1. Keep it short.\n- {}\nprose",
            "x".repeat(200)
        );
        assert_eq!(
            parse(&reply, &[]),
            vec!["Drop 'basically'.", "Use numbered lists.", "Keep it short."]
        );
        assert_eq!(
            parse("- \"Quoted rule.\"\n2) Same\n• same", &[]),
            vec!["Quoted rule.", "Same"]
        );
    }

    #[test]
    fn rules_that_restate_an_example_are_dropped() {
        let examples = [example(
            "e",
            "here is a list of groceries bananas peppers",
            "Groceries:\n- Bananas\n- Peppers",
        )];
        let reply = "- If the speaker says 'here is a list of groceries', format it as a list.\n\
                     - Turn items said in a row into a list, one item per line.";
        assert_eq!(
            parse(reply, &examples),
            vec!["Turn items said in a row into a list, one item per line."]
        );
    }

    #[test]
    fn rules_about_exclamation_marks_are_never_learned() {
        let reply = "- Add an exclamation mark to statements that end with a period.\n\
                     - End excited sentences with !\n\
                     - Use EXCLAMATION points for emphasis.\n\
                     - Drop 'basically'.";
        assert_eq!(parse(reply, &[]), vec!["Drop 'basically'."]);
    }

    /// What Python's `parse` gave for the same replies.
    #[test]
    fn parse_matches_python() {
        let reply = "  -   Rule one.  \r\n2) Two\u{2028}• Three\n10. \"Ten\" \n-no space\n- \n*\tTabbed rule";
        assert_eq!(
            parse(reply, &[]),
            vec!["Rule one.", "Two", "Three", "Ten", "Tabbed rule"]
        );
        assert_eq!(
            parse("- ÉCRIRE Straße\n- écrire STRASSE\n- Keep it", &[]),
            vec!["ÉCRIRE Straße", "Keep it"]
        );
        let examples = [example("e", "Open Cass number one two three", "x")];
        assert_eq!(
            parse("- open cass number one two\n- Fine rule", &examples),
            vec!["Fine rule"]
        );
    }

    #[test]
    fn at_most_twelve_rules() {
        let reply: String = (0..20).map(|i| format!("- Rule {i}.\n")).collect();
        assert_eq!(parse(&reply, &[]).len(), MAX_NOTES);
    }

    #[test]
    fn the_summary_prompt_asks_for_twelve_rules() {
        assert!(summary_prompt().contains("At most 12 rules."));
        assert!(!summary_prompt().contains("{max_notes}"));
    }
}

/// The job over a real kass.db, in front of a stand-in worker that is idle
/// and generates.
#[cfg(test)]
mod job_tests {
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    use super::*;

    const RULE: &str = "Write Kass as one word.";

    #[derive(Clone, Default)]
    struct Worker {
        busy: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        /// A dictation comes right after the summary.
        dictate_after_summary: Arc<AtomicBool>,
        reply: Arc<Mutex<String>>,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn idle(State(worker): State<Worker>) -> Json<Value> {
        Json(json!({
            "idle": !worker.busy.load(Ordering::SeqCst),
            "generation": worker.generation.load(Ordering::SeqCst),
        }))
    }

    /// The summary reply; a cleanup writes "cass" as Kass only when a rule
    /// says so.
    async fn generate(State(worker): State<Worker>, Json(request): Json<Value>) -> String {
        worker.requests.lock().unwrap().push(request.clone());
        let system = request["system"].as_str().unwrap_or("");
        let prompt = request["prompt"].as_str().unwrap_or("");
        let text = if system.starts_with("You keep a short list of rules") {
            if worker.dictate_after_summary.load(Ordering::SeqCst) {
                worker.generation.fetch_add(1, Ordering::SeqCst);
            }
            worker.reply.lock().unwrap().clone()
        } else if system.contains("Never say Kass.") {
            prompt.replace("cass", "the desktop dictation app")
        } else if system.contains(RULE) {
            prompt.replace("cass", "Kass")
        } else {
            prompt.to_string()
        };
        format!(
            "{}\n",
            json!({ "text": text, "model": "0.6B", "stopped": false, "seconds": 0.01 })
        )
    }

    async fn start_worker(worker: Worker) -> SocketAddr {
        let app = Router::new()
            .route("/worker/learning/idle", post(idle))
            .route("/worker/learning/generate", post(generate))
            .with_state(worker);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        address
    }

    /// A kass.db with the default style and `count` corrections of "cass"
    /// to Kass, oldest first.
    fn database(count: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kass-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kass.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../tests/kass_schema.sql"))
            .unwrap();
        conn.execute_batch(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
                 capitalize_first, smart_cleanup, preserve_technical)
             VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1);",
        )
        .unwrap();
        for i in 0..count {
            let said = format!("open cass number {i}");
            let created_at = format!("2026-11-01 10:{:02}:00.000000", i);
            conn.execute(
                "INSERT INTO captures (id, audio_path, source, transcript_raw, transcript_refined,
                     audio_deleted, created_at)
                 VALUES (?1, 'a.wav', 'dictation', ?2, ?2, 0, ?3)",
                (format!("c{i}"), &said, &created_at),
            )
            .unwrap();
            let snapshot = json!({"transcript_raw": said, "transcript_refined": said});
            conn.execute(
                "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes,
                     snapshot, source, created_at)
                 VALUES (?1, ?2, 'refined', ?3, '', ?4, 'manual', ?5)",
                (
                    format!("f{i}"),
                    format!("c{i}"),
                    format!("open Kass number {i}"),
                    snapshot.to_string(),
                    &created_at,
                ),
            )
            .unwrap();
        }
        crate::db::prepare(&path).unwrap();
        path
    }

    async fn kass(count: usize, reply: &str) -> (AppState, Worker) {
        let worker = Worker::default();
        *worker.reply.lock().unwrap() = reply.to_string();
        let state = crate::test_state(start_worker(worker.clone()).await, database(count));
        (state, worker)
    }

    fn notes_file(state: &AppState) -> Value {
        learned::read_notes(&learned::data_dir(state))
    }

    #[tokio::test]
    async fn the_oldest_examples_become_a_rule_cleanup_follows() {
        // 16 corrections fit the prompt; the 10 before them are summarized.
        let (state, worker) = kass(26, &format!("Here are the rules:\n- {RULE}")).await;
        let saved = run_once(&state).await.unwrap().unwrap();
        assert_eq!(saved["outcome"], "updated");
        assert_eq!(saved["metrics"]["examples"], 10);
        assert_eq!(saved["metrics"]["checks"], 10);
        let notes = &notes_file(&state)["styles"]["personal"];
        assert_eq!(notes["notes"], json!([RULE]));
        let considered: Vec<String> = (0..10).map(|i| format!("correction:f{i}")).collect();
        let mut sorted = considered.clone();
        sorted.sort();
        assert_eq!(notes["considered_ids"], json!(sorted));
        assert_eq!(notes["history"], json!([{ "notes": [] }]));
        assert!(notes["last_run"].as_str().unwrap().ends_with("+00:00"));

        let requests = worker.requests.lock().unwrap().clone();
        // The summary runs on the base model, with no prompt cache.
        assert_eq!(requests[0]["flags"], Value::Null);
        assert_eq!(requests[0]["cache_key"], Value::Null);
        assert_eq!(requests[0]["max_tokens"], 1024);
        let request = requests[0]["prompt"].as_str().unwrap();
        assert!(request.starts_with("Current rules:\n(none yet)\n\nNew examples:\n\nSaid: open cass number 0\nMeant: open Kass number 0\n\n"));
        // Replays clean up as the style does, in a prompt cache of their own.
        assert_eq!(requests[1]["cache_key"], "cleanup:personal:candidate-rules");
        assert_eq!(requests[1]["flags"]["style"], "personal");
        assert_eq!(requests[1]["hint"], "");
        let examples = requests[1]["examples"].as_array().unwrap();
        assert_eq!(examples.last().unwrap()[1], "open Kass number 25");
        assert!(!examples
            .iter()
            .any(|example| example[1] == "open Kass number 9"));

        // Those examples aren't summarized again.
        assert_eq!(run_once(&state).await.unwrap(), None);
    }

    #[tokio::test]
    async fn waits_for_a_full_batch() {
        let (state, worker) = kass(25, &format!("- {RULE}")).await;
        assert_eq!(run_once(&state).await.unwrap(), None);
        assert!(worker.requests.lock().unwrap().is_empty());
        assert_eq!(notes_file(&state)["styles"]["personal"], Value::Null);
    }

    #[tokio::test]
    async fn nothing_runs_while_the_models_are_busy() {
        let (state, worker) = kass(26, &format!("- {RULE}")).await;
        worker.busy.store(true, Ordering::SeqCst);
        assert_eq!(run_once(&state).await.unwrap(), None);
        assert!(worker.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dictation_interrupts_the_summary_without_saving() {
        let (state, worker) = kass(26, &format!("- {RULE}")).await;
        worker.dictate_after_summary.store(true, Ordering::SeqCst);
        assert_eq!(run_once(&state).await.unwrap(), None);
        assert_eq!(worker.requests.lock().unwrap().len(), 1);
        assert_eq!(notes_file(&state)["styles"]["personal"], Value::Null);
    }

    #[tokio::test]
    async fn a_rejected_summary_still_counts_its_examples_considered() {
        let (state, _) = kass(26, "- Never say Kass.").await;
        let saved = run_once(&state).await.unwrap().unwrap();
        assert_eq!(saved["outcome"], "no_change");
        let notes = &notes_file(&state)["styles"]["personal"];
        assert_eq!(notes["notes"], json!([]));
        assert_eq!(notes["outcome"], "no_change");
        assert_eq!(notes["considered_ids"].as_array().unwrap().len(), 10);
        assert_eq!(notes["history"], json!([]));
    }

    #[tokio::test]
    async fn nothing_runs_with_cleanup_off() {
        let (state, worker) = kass(26, &format!("- {RULE}")).await;
        let updated = state
            .db
            .call(|conn| {
                settings::get(conn)?;
                conn.execute("UPDATE capture_settings SET auto_refine = 0", [])
            })
            .await
            .unwrap();
        assert_eq!(updated, 1);
        assert_eq!(run_once(&state).await.unwrap(), None);
        assert!(worker.requests.lock().unwrap().is_empty());
    }
}
