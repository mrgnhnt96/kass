//! Replays every call recorded from the Python text stages
//! (`tests/golden/python.json`) and checks kass-text returns the same. The
//! Python stages and their recorder, `tests/golden/record.py`, were deleted
//! once the Rust server ran cleanup; the recording stays as the spec.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use kass_text::content_check::{self, Verdict};
use kass_text::correction_rules::{self, Rule, Rules};
use kass_text::dictionary::{self, Common, Dictionary, Term};
use kass_text::lexicon::{self, WordData};
use kass_text::phrase_seams::{self, Names};
use kass_text::refinement::{self, Flags};
use kass_text::spoken_punctuation::{self as punctuation, Example, Learned};
use kass_text::voice_edits::{self, Declined, Instruction, Planned, Word};
use kass_text::writing_style::{self, Counts, Habits, Profile};
use kass_text::{
    dictation_edits, laughter, sentence_tail, spelling, spoken_case, spoken_cleanup,
    spoken_corrections, text_merge, voice_commands,
};
use serde_json::{json, Map, Value};

fn text<'a>(args: &'a Value, name: &str) -> &'a str {
    args[name]
        .as_str()
        .unwrap_or_else(|| panic!("argument {name} is not a string: {}", args[name]))
}

fn opt_text<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args[name].as_str()
}

fn names(args: &Value) -> Names {
    args["names"].as_array().map_or_else(Names::new, |names| {
        names
            .iter()
            .map(|name| name.as_str().unwrap().to_string())
            .collect()
    })
}

fn learned(args: &Value) -> Option<Learned> {
    let value = &args["learned"];
    if value.is_null() {
        return None;
    }
    let aliases = value["aliases"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, item)| (key.clone(), item.as_str().unwrap().to_string()))
        .collect();
    let kept = value["kept"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect();
    Some(Learned { aliases, kept })
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

fn verdict_value(verdict: &Verdict) -> Value {
    json!({"outcome": verdict.outcome, "added": verdict.added, "missing": verdict.missing, "reason": verdict.reason})
}

fn learned_value(learned: &Learned) -> Value {
    json!({"aliases": learned.aliases, "kept": learned.kept})
}

fn pairs(value: &Value) -> Vec<(String, String)> {
    value.as_array().map_or_else(Vec::new, |pairs| {
        pairs
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_string(),
                    pair[1].as_str().unwrap().to_string(),
                )
            })
            .collect()
    })
}

fn shared_pairs(args: &Value, examples: &[Value]) -> Vec<(String, String)> {
    match args["examples"].as_u64() {
        Some(index) => pairs(&examples[index as usize]),
        None => pairs(&args["examples"]),
    }
}

fn flags(args: &Value) -> Flags {
    Flags::from_json(&args["flags"])
}

fn profile(args: &Value) -> Profile {
    Profile::from_json(&args["profile"])
}

fn rules(value: &Value) -> Rules {
    Rules::new(
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| Rule::from_json(rule).expect("a valid rule"))
            .collect(),
    )
}

fn rule_example(value: &Value) -> correction_rules::Example {
    correction_rules::Example {
        id: text(value, "id").to_string(),
        capture_id: text(value, "capture_id").to_string(),
        original: text(value, "original").to_string(),
        expected: text(value, "expected").to_string(),
        language: opt_text(value, "language").map(String::from),
        source: text(value, "source").to_string(),
    }
}

/// `evaluate(..., every_report=True)`, the job's only call. The timing is
/// the machine's, so the median is left out and a call where Python's
/// latency gate failed (a test slows the clock) isn't replayed.
fn evaluate(args: &Value) -> Option<Value> {
    let examples: Vec<_> = args["examples"]
        .as_array()?
        .iter()
        .map(rule_example)
        .collect();
    let active: Vec<Rule> = args["active"]
        .as_array()?
        .iter()
        .map(|rule| Rule::from_json(rule).expect("a valid rule"))
        .collect();
    let blocked = args.get("blocked").map(strings).unwrap_or_default();
    let (rules, metrics) = correction_rules::evaluate(&examples, &active, &blocked);
    let mut metrics = metrics.to_json();
    metrics["median_rule_ms"] = Value::Null;
    let rules: Vec<Value> = rules.iter().map(Rule::to_json).collect();
    Some(json!([rules, metrics]))
}

/// Python counts offsets in characters; kass-text in bytes.
fn chars(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

fn word_value(word: &Word, source: &str) -> Value {
    json!({
        "type": "Word",
        "text": word.text,
        "start": chars(source, word.start),
        "end": chars(source, word.end),
        "key": word.key,
        "opens": word.opens,
        "marked": word.marked,
    })
}

fn words_value(words: &[Word], source: &str) -> Value {
    Value::Array(words.iter().map(|word| word_value(word, source)).collect())
}

fn declined_value(declined: &Declined) -> Value {
    json!({"type": "Declined", "message": declined.message})
}

fn instruction_value(instruction: &Instruction) -> Value {
    let said = &instruction.said;
    json!({
        "type": "Instruction",
        "find": words_value(&instruction.find, said),
        "write": words_value(&instruction.write, said),
        "spelled": instruction.spelled,
        "either": instruction.either,
        "alike": instruction.alike,
        "place": instruction.place,
        "said": said,
    })
}

fn planned_value(planned: &Planned) -> Value {
    json!({
        "type": "Planned",
        "before": planned.before,
        "after": planned.after,
        "instruction": planned.instruction,
        "replaced": planned.replaced,
        "spelled": planned.spelled,
    })
}

/// The words recorded as common, deciding common words the way Python did.
fn common(args: &Value) -> Common {
    let words: HashSet<String> = strings(&args["common"]).into_iter().collect();
    Arc::new(move |word: &str| words.contains(word))
}

fn string_pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_string(),
                pair[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn dictionary_value(found: &Dictionary) -> Value {
    let mut replacements: Vec<[&str; 2]> = found
        .replacements
        .iter()
        .map(|(spoken, written)| [spoken.as_str(), written.as_str()])
        .collect();
    replacements.sort();
    let mut exact: Vec<&String> = found.exact.iter().collect();
    exact.sort();
    let spellings: Vec<[&str; 2]> = found
        .spellings_in_order()
        .map(|(key, term)| [key, term])
        .collect();
    json!({
        "replacements": replacements,
        "spellings": spellings,
        "exact": exact,
        "terms": found.terms,
        "names": found.names,
        "span": found.span,
    })
}

/// The Rust result for one recorded call, or `None` for a function not ported.
fn call(function: &str, args: &Value, examples: &[Value]) -> Option<Value> {
    let value = match function {
        "content_check._copied_example" => {
            let shown: Vec<(String, String)> = examples
                [args["examples"].as_u64().unwrap() as usize]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_str().unwrap().to_string(),
                        pair[1].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            json!(content_check::copied_example(
                text(args, "said"),
                text(args, "cleaned"),
                &shown
            ))
        }
        "content_check.check" => verdict_value(&content_check::check(
            text(args, "said"),
            text(args, "cleaned"),
            args["allow_retractions"].as_bool().unwrap_or(false),
            opt_text(args, "retractable").unwrap_or(""),
        )),
        "content_check.summarize_reviews" => {
            let verdicts: Vec<Verdict> = args["verdicts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|verdict| Verdict {
                    outcome: text(verdict, "outcome").to_string(),
                    added: strings(&verdict["added"]),
                    missing: strings(&verdict["missing"]),
                    reason: opt_text(verdict, "reason").map(String::from),
                })
                .collect();
            content_check::summarize_reviews(&verdicts).map_or(Value::Null, |summary| {
                json!({"outcome": summary.outcome, "added": summary.added, "missing": summary.missing, "reasons": summary.reasons})
            })
        }

        "Dictionary.apply" => {
            let parts = &args["dictionary"];
            let found = Dictionary::of(
                string_pairs(&parts["replacements"]),
                string_pairs(&parts["spellings"]),
                strings(&parts["exact"]),
                common(args),
            );
            json!(found.apply(text(args, "text")))
        }
        "dictionary.build" => {
            let active: Vec<Term> = args["resolved"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| !entry["overridden"].as_bool().unwrap())
                .map(|entry| Term {
                    written: text(entry, "written").to_string(),
                    spoken: opt_text(entry, "spoken").map(String::from),
                    match_sound: entry["match_sound"].as_bool().unwrap(),
                    phrase: entry["phrase"].as_bool().unwrap(),
                })
                .collect();
            let exact_spelling = args["exact_spelling"].as_bool().unwrap_or(true);
            dictionary_value(&dictionary::build(&active, exact_spelling))
        }
        "dictionary.misheard_terms" => json!(dictionary::misheard_terms_with(
            text(args, "original"),
            text(args, "corrected"),
            &*common(args),
        )),
        "dictionary.corrected_words" => json!(dictionary::corrected_words_with(
            text(args, "original"),
            text(args, "corrected"),
            common(args),
        )),
        "dictionary.sounds_alike" => json!(dictionary::sounds_alike(
            text(args, "heard"),
            text(args, "written")
        )),

        "dictation_edits.apply_dictation_edits" => json!(dictation_edits::apply_dictation_edits(
            text(args, "text"),
            args["formatting"].as_bool().unwrap_or(false),
            args["corrections"].as_bool().unwrap_or(false)
        )),
        "dictation_edits.apply_line_breaks" => {
            json!(dictation_edits::apply_line_breaks(text(args, "text")))
        }
        "dictation_edits.apply_spoken_marks" => {
            json!(dictation_edits::apply_spoken_marks(text(args, "text")))
        }

        "laughter.is_laughter" => json!(laughter::is_laughter(text(args, "text"))),
        "laughter.join_laughter" => json!(laughter::join_laughter(text(args, "text"))),

        "phrase_seams._common_word" => json!(lexicon::common_word(text(args, "word"))),
        "phrase_seams.close_phrase" => json!(phrase_seams::close_phrase(text(args, "text"))),
        "phrase_seams.continue_after_seam" => {
            let (before, phrase) = phrase_seams::continue_after_seam(
                text(args, "before"),
                text(args, "phrase"),
                opt_text(args, "earlier").unwrap_or(""),
                &names(args),
            );
            json!([before, phrase])
        }
        "phrase_seams.continue_phrase" => json!(phrase_seams::continue_phrase(
            text(args, "phrase"),
            opt_text(args, "earlier").unwrap_or(""),
            &names(args)
        )),
        "phrase_seams.continues_sentence" => {
            json!(phrase_seams::continues_sentence(text(args, "before")))
        }
        "phrase_seams.join_phrases" => json!(phrase_seams::join_phrases(
            text(args, "previous"),
            text(args, "phrase"),
            text(args, "raw_phrase"),
            opt_text(args, "style").unwrap_or("standard")
        )),
        "phrase_seams.keep_raw_capitals" => {
            json!(phrase_seams::keep_raw_capitals(
                text(args, "text"),
                text(args, "raw"),
                &names(args)
            ))
        }
        "phrase_seams.match_raw_start" => json!(phrase_seams::match_raw_start(
            text(args, "text"),
            text(args, "raw")
        )),
        "phrase_seams.mid_sentence_capitals" => {
            json!(phrase_seams::mid_sentence_capitals(text(args, "text")))
        }
        "phrase_seams.open_phrase" => json!(phrase_seams::open_phrase(
            text(args, "text"),
            text(args, "raw")
        )),
        "phrase_seams.strip_pause_mark" => {
            json!(phrase_seams::strip_pause_mark(text(args, "text")))
        }

        "spoken_case.apply_spoken_case" => {
            json!(spoken_case::apply_spoken_case(text(args, "text")))
        }
        "spoken_cleanup.apply_spoken_cleanup" => {
            json!(spoken_cleanup::apply_spoken_cleanup(text(args, "text")))
        }
        "spoken_corrections.apply_spoken_corrections" => {
            json!(spoken_corrections::apply_spoken_corrections(text(
                args, "text"
            )))
        }
        "spoken_corrections.retracted_by" => {
            json!(spoken_corrections::retracted_by(
                text(args, "phrase"),
                text(args, "earlier")
            ))
        }

        "spoken_punctuation.apply_spoken_punctuation" => {
            json!(punctuation::apply_spoken_punctuation(
                text(args, "text"),
                learned(args).as_ref()
            ))
        }
        "spoken_punctuation.keep_spoken_punctuation" => {
            json!(punctuation::keep_spoken_punctuation(
                text(args, "said"),
                text(args, "text"),
                learned(args).as_ref()
            ))
        }
        "spoken_punctuation.learn" => {
            let examples: Vec<Example> = args["examples"]
                .as_array()
                .unwrap()
                .iter()
                .map(|example| Example {
                    original: text(example, "original").to_string(),
                    expected: text(example, "expected").to_string(),
                    capture_id: text(example, "capture_id").to_string(),
                    source: text(example, "source").to_string(),
                })
                .collect();
            learned_value(&punctuation::learn(&examples))
        }
        "spoken_punctuation.period_after_closers" => {
            json!(punctuation::period_after_closers(text(args, "text")))
        }

        "sentence_tail.settle" => sentence_tail::settle(text(args, "raw"), text(args, "cleaned"))
            .map_or(Value::Null, |settled| {
                json!({
                    "type": "Settled",
                    "committed": settled.committed,
                    "gap": settled.gap,
                    "open_cleaned": settled.open_cleaned,
                    "open_raw": settled.open_raw,
                    "committed_raw": settled.committed_raw,
                })
            }),
        "text_merge.take_back" => {
            json!(text_merge::take_back(
                text(args, "before"),
                text(args, "removed"),
                text(args, "newer")
            ))
        }

        "voice_commands.commands_alone" => {
            json!(voice_commands::commands_alone(text(args, "text")))
        }
        "voice_commands.count_commands" => {
            json!(voice_commands::count_commands(text(args, "text")))
        }
        "voice_commands.mark_commands" => json!(voice_commands::mark_commands(text(args, "text"))),

        "voice_edits.changes_end" => {
            let planned = &args["planned"];
            let planned = Planned {
                before: text(planned, "before").to_string(),
                after: text(planned, "after").to_string(),
                instruction: text(planned, "instruction").to_string(),
                replaced: text(planned, "replaced").to_string(),
                spelled: opt_text(planned, "spelled").map(String::from),
            };
            json!(voice_edits::changes_end(
                &planned,
                args["chars"].as_u64().unwrap() as usize
            ))
        }
        "voice_edits.corrected" => {
            json!(voice_edits::corrected(
                text(args, "output"),
                text(args, "before"),
                text(args, "after")
            ))
        }
        "content_check.check_refinement" => {
            let (kept, verdict) = content_check::check_refinement(
                text(args, "said"),
                text(args, "refined"),
                &flags(args),
                &shared_pairs(args, examples),
                opt_text(args, "retractable").unwrap_or(""),
                learned(args).as_ref(),
            );
            json!([kept, verdict_value(&verdict)])
        }

        "spelling.join_spelling" => json!(spelling::join_spelling(text(args, "text"))),
        "spelling.respell" => json!(spelling::respell(text(args, "text"))),
        "spelling.distance" => json!(spelling::distance(
            text(args, "a"),
            text(args, "b"),
            args["limit"].as_u64().unwrap() as usize
        )),
        "spelling.sounds_like" => json!(spelling::sounds_like(
            text(args, "letters"),
            text(args, "word")
        )),
        "spelling.shaped" => json!(spelling::shaped(text(args, "letters"), text(args, "like"))),
        "spelling.is_spelled" => json!(spelling::is_spelled(text(args, "text"))),

        "refinement.collapse_repetitive_artifacts" => {
            json!(refinement::collapse_repetitive_artifacts(
                text(args, "text"),
                args["min_run"]
                    .as_u64()
                    .map_or(refinement::REPETITION_RUN_THRESHOLD, |run| run as usize),
            ))
        }
        "refinement.strip_stt_artifacts" => {
            json!(refinement::strip_stt_artifacts(text(args, "text")))
        }
        "refinement.prepare_refinement" => json!(refinement::prepare_refinement(
            text(args, "transcript"),
            &flags(args),
            learned(args).as_ref(),
        )),
        "refinement.keep_said_punctuation" => json!(refinement::keep_said_punctuation(
            text(args, "said"),
            text(args, "text"),
            learned(args).as_ref(),
        )),
        "refinement.build_refinement_prompt" => json!(refinement::build_refinement_prompt(
            &flags(args),
            &profile(args),
            text(args, "description"),
            args["personal"].as_bool().unwrap_or(false),
            opt_text(args, "notes"),
        )),
        "refinement.refinement_examples" => json!(refinement::refinement_examples(
            &flags(args),
            &profile(args),
            &pairs(&args["personal"]),
        )),
        "refinement._without_final_period" => {
            json!(refinement::without_final_period(text(args, "text")))
        }
        "refinement.style_first_word" => json!(refinement::style_first_word(
            text(args, "text"),
            &flags(args),
            &names(args),
        )),
        "refinement._cache_key" => json!(refinement::cache_key(
            &flags(args),
            args["use_personal_examples"].as_bool().unwrap_or(true),
            !args["correction_notes"].is_null(),
        )),
        "refinement._prompt" => json!(refinement::prompt(
            &flags(args),
            &profile(args),
            text(args, "description"),
            &pairs(&args["personal"]),
            opt_text(args, "notes"),
        )),

        "writing_style.observe" => writing_style::observe(
            text(args, "shown"),
            text(args, "written"),
            args["deliberate"].as_bool().unwrap_or(true),
        )
        .to_json(),
        "writing_style.decide" => {
            writing_style::decide(&Counts::from_json(&args["counts"])).to_json()
        }
        "writing_style.summary" => {
            json!(writing_style::summary(&Habits::from_json(&args["habits"])))
        }
        "writing_style.apply_style" => json!(writing_style::apply_style(
            text(args, "text"),
            &Habits::from_json(&args["habits"]),
        )),
        "writing_style.habits" => profile(args).habits().to_json(),
        "writing_style.is_ready" => json!(profile(args).is_ready()),
        "writing_style.prompt_section" => json!(profile(args).prompt_section()),
        "writing_style.prompt_example" => json!(profile(args).prompt_example()),
        "writing_style.apply_learned" => json!(profile(args).apply_learned(text(args, "text"))),
        "writing_style._as_spoken" => json!(writing_style::as_spoken(text(args, "text"))),

        "correction_rules.words" => json!(correction_rules::words(text(args, "text"))),
        "correction_rules.apply_rules" => {
            json!(rules(&args["compiled"]).apply(text(args, "text"), opt_text(args, "language")))
        }
        "correction_rules.candidate" => {
            let rule = correction_rules::candidate(&rule_example(&args["example"]));
            rule.map_or(Value::Null, |rule| rule.to_json())
        }
        "correction_rules.loss" => {
            json!(correction_rules::loss(
                text(args, "actual"),
                text(args, "expected")
            ))
        }
        "correction_rules.evaluate" => return evaluate(args),
        "correction_learning.apply_learned_corrections" => json!(rules(&args["rules"])
            .apply_learned_corrections(text(args, "text"), opt_text(args, "language"))),

        "voice_edits.opening" => json!(voice_edits::opening(text(args, "text"))),
        "voice_edits.parse" => match voice_edits::parse(text(args, "said")) {
            None => Value::Null,
            Some(Ok(instruction)) => instruction_value(&instruction),
            Some(Err(declined)) => declined_value(&declined),
        },
        "voice_edits.plan" => match voice_edits::plan(text(args, "said"), opt_text(args, "take")) {
            None => Value::Null,
            Some(Ok(planned)) => planned_value(&planned),
            Some(Err(declined)) => declined_value(&declined),
        },
        "voice_edits.starts_edit" => json!(voice_edits::starts_edit(text(args, "text"))),
        "voice_edits.words" => {
            words_value(&voice_edits::words(text(args, "text")), text(args, "text"))
        }

        _ => return None,
    };
    Some(value)
}

/// Floats compare to within rounding; everything else exactly.
fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) if a.is_f64() || b.is_f64() => {
            (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 1e-9
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, x)| b.get(key).is_some_and(|y| same(x, y)))
        }
        _ => left == right,
    }
}

#[test]
fn matches_the_python_text_stages() {
    // The recording left Whisper's vocabulary out, so common words are
    // decided by the system word list alone.
    let words = fs::read_to_string("/usr/share/dict/words").expect("macOS word list");
    let _ = lexicon::install(WordData::new(HashMap::new(), &words));

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/python.json");
    let recorded: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let examples = recorded["examples"].as_array().unwrap();
    let calls = recorded["calls"].as_array().unwrap();

    let mut missing: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut matched = 0;
    for recorded_call in calls {
        let function = recorded_call["fn"].as_str().unwrap();
        let args = &recorded_call["args"];
        let mut expected = recorded_call["out"].clone();
        if function == "correction_rules.evaluate" {
            // Only the every-report path is ported: the job never holds
            // reports back.
            if args["every_report"] != json!(true) || expected[1]["latency_passed"] == json!(false)
            {
                continue;
            }
            expected[1]["median_rule_ms"] = Value::Null;
        }
        let Some(out) = call(function, args, examples) else {
            *missing.entry(function).or_default() += 1;
            continue;
        };
        if same(&out, &expected) {
            matched += 1;
        } else {
            let mut failure = Map::new();
            failure.insert("fn".into(), json!(function));
            failure.insert("args".into(), args.clone());
            failure.insert("python".into(), recorded_call["out"].clone());
            failure.insert("rust".into(), out);
            failures.push(Value::Object(failure));
        }
    }

    let mut by_function: BTreeMap<&str, usize> = BTreeMap::new();
    for failure in &failures {
        *by_function
            .entry(failure["fn"].as_str().unwrap())
            .or_default() += 1;
    }
    for failure in failures.iter().take(40) {
        eprintln!("{}\n", serde_json::to_string(failure).unwrap());
    }
    eprintln!("matched {matched} of {} calls", calls.len());
    eprintln!("mismatches by function: {by_function:?}");
    eprintln!("not ported: {missing:?}");
    assert!(
        failures.is_empty(),
        "{} calls differ from Python",
        failures.len()
    );
    assert!(missing.is_empty(), "functions not ported: {missing:?}");
}
