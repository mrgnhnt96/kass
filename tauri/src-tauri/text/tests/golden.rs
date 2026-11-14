//! Replays every call recorded from the Python text stages
//! (`tests/golden/python.json`, made by `tests/golden/record.py`) and checks
//! kass-text returns the same.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use kass_text::content_check::{self, Verdict};
use kass_text::lexicon::{self, WordData};
use kass_text::phrase_seams::{self, Names};
use kass_text::spoken_punctuation::{self as punctuation, Example, Learned};
use kass_text::voice_edits::{self, Declined, Instruction, Planned, Word};
use kass_text::{
    dictation_edits, laughter, sentence_tail, spoken_case, spoken_cleanup, spoken_corrections,
    text_merge, voice_commands,
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
        let Some(out) = call(function, args, examples) else {
            *missing.entry(function).or_default() += 1;
            continue;
        };
        if same(&out, &recorded_call["out"]) {
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
