//! Parity with the React app's i18next `translation.json`.
//!
//! The JSON is deleted together with the React app. When that happens these
//! tests fail on purpose: delete this file (and its `mod parity;` line) in
//! the same change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use fluent_bundle::{FluentArgs, FluentValue};
use serde_json::Value;

use super::message_ids;
use crate::bundle::LOCALES;
use crate::id::strip_plural_suffix;
use crate::{fluent_id, has, t_args};

const JSON_PATH: &str = "../../app/src/i18n/locales/en/translation.json";

/// Counts every plural set is checked with: i18next's `_zero` (0), `_one`
/// (1) and `_other` (5).
const PLURAL_COUNTS: [i64; 3] = [0, 1, 5];
/// The count used for a plain string that interpolates `{{count}}`.
const PLAIN_COUNT: i64 = 7;

/// One Fluent message's JSON source: a plain string or a plural set.
#[derive(Default)]
struct Source {
    /// Plural category (`one`, `other`, …) → string; `None` for a plain string.
    forms: BTreeMap<Option<String>, String>,
    keys: Vec<String>,
}

/// Appended to failures caused by translation.json and the Fluent files drifting.
const RESYNC: &str = "\nAfter editing translation.json, regenerate the Fluent files: \
     scripts/translations-to-fluent.py app/src/i18n/locales/en/translation.json \
     crates/kass-i18n/locales/en";

fn load_sources() -> BTreeMap<String, Source> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(JSON_PATH);
    let json = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is gone ({e}). If the React app was removed, delete \
             crates/kass-i18n/src/tests/parity.rs along with it.",
            path.display()
        )
    });
    let root: Value = serde_json::from_str(&json).expect("translation.json is valid JSON");
    let mut leaves = Vec::new();
    flatten(&root, String::new(), &mut leaves);

    let mut sources: BTreeMap<String, Source> = BTreeMap::new();
    for (key, value) in leaves {
        let category = strip_plural_suffix(&key).map(|base| key[base.len() + 1..].to_owned());
        let id = fluent_id(&key);
        let source = sources.entry(id.clone()).or_default();
        if let Some(previous) = source.forms.insert(category, value) {
            panic!("`{key}` maps to `{id}`, which another key already uses (value {previous:?})");
        }
        source.keys.push(key);
    }
    for (id, source) in &sources {
        let plural = source.forms.keys().any(Option::is_some);
        assert!(
            !plural || !source.forms.contains_key(&None),
            "`{id}` has both a plain string and plural forms: {:?}",
            source.keys
        );
        assert!(
            !plural || source.forms.contains_key(&Some("other".to_owned())),
            "plural set `{id}` has no _other form: {:?}",
            source.keys
        );
    }
    sources
}

fn flatten(value: &Value, path: String, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                flatten(child, child_path, out);
            }
        }
        Value::String(text) => out.push((path, text.clone())),
        other => panic!(
            "`{path}` is not a string or object: {other}. Map it in the .ftl files and here."
        ),
    }
}

/// `{{name}}` placeholders in an i18next string.
fn variables(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else {
            break;
        };
        names.push(rest[start + 2..start + 2 + len].trim().to_owned());
        rest = &rest[start + 2 + len + 2..];
    }
    names
}

/// i18next's interpolation with `escapeValue: false`: each `{{name}}` is
/// replaced by its value's string form.
fn interpolate(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else {
            break;
        };
        let name = rest[start + 2..start + 2 + len].trim();
        out.push_str(&rest[..start]);
        out.push_str(values.get(name).map_or("", String::as_str));
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    out
}

/// i18next's English form choice: `_zero` when count is 0 and it exists,
/// then `Intl.PluralRules('en')`.
fn i18next_form(forms: &BTreeMap<Option<String>, String>, count: i64) -> &str {
    let category = |c: &str| forms.get(&Some(c.to_owned()));
    let zero = if count == 0 { category("zero") } else { None };
    let english = if count == 1 { category("one") } else { None };
    zero.or(english).or_else(|| category("other")).unwrap()
}

/// Sample arguments: every variable a sample string, `count` a number.
fn sample_args(
    names: &BTreeSet<String>,
    count: i64,
) -> (FluentArgs<'static>, BTreeMap<String, String>) {
    let mut args = FluentArgs::new();
    let mut strings = BTreeMap::new();
    for name in names {
        if name == "count" {
            args.set(name.clone(), FluentValue::from(count));
            strings.insert(name.clone(), count.to_string());
        } else {
            let sample = format!("‹{name} sample›");
            args.set(name.clone(), FluentValue::from(sample.clone()));
            strings.insert(name.clone(), sample);
        }
    }
    (args, strings)
}

#[test]
fn every_json_string_formats_like_i18next() {
    let sources = load_sources();
    let mut failures = Vec::new();
    for (id, source) in &sources {
        if !has(id) {
            failures.push(format!("{:?}: no Fluent message `{id}`", source.keys));
            continue;
        }
        let names: BTreeSet<String> = source
            .forms
            .values()
            .flat_map(|text| variables(text))
            .collect();
        let plural = source.forms.keys().any(Option::is_some);
        let counts: &[i64] = if plural {
            &PLURAL_COUNTS
        } else {
            &[PLAIN_COUNT]
        };
        for &count in counts {
            let mut names = names.clone();
            if plural {
                names.insert("count".to_owned());
            }
            let (args, strings) = sample_args(&names, count);
            let json = if plural {
                i18next_form(&source.forms, count)
            } else {
                &source.forms[&None]
            };
            let expected = interpolate(json, &strings);
            let actual = t_args(id, &args);
            if actual != expected {
                failures.push(format!(
                    "`{id}` (count {count}):\n  i18next: {expected:?}\n  fluent:  {actual:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}{RESYNC}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_fluent_message_has_a_json_source() {
    let sources = load_sources();
    let orphans: Vec<String> = LOCALES
        .iter()
        .flat_map(|locale| locale.files.iter())
        .flat_map(|(name, source)| {
            message_ids(source)
                .into_iter()
                .map(move |id| format!("{name}.ftl: {id}"))
        })
        .filter(|entry| !sources.contains_key(entry.rsplit(": ").next().unwrap()))
        .collect();
    assert!(
        orphans.is_empty(),
        "Fluent messages with no translation.json key:\n{}{RESYNC}",
        orphans.join("\n")
    );
}

#[test]
fn each_json_section_has_its_own_file() {
    let sources = load_sources();
    let en = &LOCALES[0];
    for source in sources.values() {
        let key = &source.keys[0];
        let section = key.split('.').next().unwrap();
        let file = fluent_id(section);
        let (_, ftl) = en
            .files
            .iter()
            .find(|(name, _)| *name == file)
            .unwrap_or_else(|| panic!("no {file}.ftl for section `{section}`{RESYNC}"));
        let id = fluent_id(key);
        assert!(
            message_ids(ftl).contains(&id),
            "`{id}` should be in {file}.ftl{RESYNC}"
        );
    }
}
