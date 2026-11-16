//! Where dictionary entries apply, and the lists the page builds from them:
//! the scope list, an entry's places, what a style or app picks up from
//! elsewhere. No UI here, so all of it is tested on its own.

use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
use kass_api::types::{
    DictionaryEntry, DictionaryEntryCreate, DictionaryPlace, DictionaryPlaceInput, DictionaryScope,
    DictionarySource, ResolvedDictionaryEntry, WritingStylesResponse,
};
use std::collections::{HashMap, HashSet};

/// The server's limit on what is said and on a word's spelling.
pub const MAX_LENGTH: usize = 200;
/// The server's limit on what a phrase writes.
pub const MAX_PHRASE_LENGTH: usize = 1000;
/// The server's limit on entries.
pub const MAX_ENTRIES: usize = 1000;

pub const EVERYWHERE_KEY: &str = "global";

/// A place dictionary entries apply: everywhere, one writing style or one app.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Style(String),
    App(String),
}

impl Scope {
    /// The scope as one string: `global`, `style:<id>` or `app:<bundle id>`.
    pub fn key(&self) -> String {
        match self {
            Scope::Global => EVERYWHERE_KEY.to_string(),
            Scope::Style(id) => format!("style:{id}"),
            Scope::App(bundle) => format!("app:{bundle}"),
        }
    }

    /// Reads a [`Scope::key`]; anything else is everywhere.
    pub fn parse(key: &str) -> Scope {
        if let Some(id) = key.strip_prefix("style:").filter(|id| !id.is_empty()) {
            return Scope::Style(id.to_string());
        }
        if let Some(bundle) = key.strip_prefix("app:").filter(|b| !b.is_empty()) {
            return Scope::App(bundle.to_string());
        }
        Scope::Global
    }

    fn of_place(scope: DictionaryScope, scope_id: Option<&str>) -> Scope {
        match (scope, scope_id.filter(|id| !id.is_empty())) {
            (DictionaryScope::Style, Some(id)) => Scope::Style(id.to_string()),
            (DictionaryScope::App, Some(id)) => Scope::App(id.to_string()),
            _ => Scope::Global,
        }
    }
}

pub fn place_scope(place: &DictionaryPlace) -> Scope {
    Scope::of_place(place.scope, place.scope_id.as_deref())
}

/// The scope keys of every place an entry applies, each once.
pub fn entry_place_keys(entry: &DictionaryEntry) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for place in &entry.places {
        let key = place_scope(place).key();
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

pub fn applies_in(entry: &DictionaryEntry, scope: &Scope) -> bool {
    entry_place_keys(entry).contains(&scope.key())
}

/// A server timestamp; one without a zone is UTC.
pub fn parse_server_date(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(date) = DateTime::parse_from_rfc3339(text) {
        return Some(date.with_timezone(&Utc));
    }
    let text = text.trim_end_matches('Z');
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
        .map(|date| date.and_utc())
}

fn created(entry: &DictionaryEntry) -> Option<DateTime<Utc>> {
    entry.created_at.as_deref().and_then(parse_server_date)
}

/// The entries that apply in a scope, newest first.
pub fn entries_in<'a>(entries: &'a [DictionaryEntry], scope: &Scope) -> Vec<&'a DictionaryEntry> {
    let mut found: Vec<&DictionaryEntry> =
        entries.iter().filter(|e| applies_in(e, scope)).collect();
    found.sort_by_key(|e| std::cmp::Reverse(created(e)));
    found
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeOption {
    pub key: String,
    pub scope: Scope,
    /// A style's or app's name; empty for everywhere, which the page labels.
    pub name: String,
    /// Entries whose places include this scope.
    pub count: usize,
    /// For an app, the writing style it uses, when the app is still listed.
    pub style_id: Option<String>,
    /// For a style, how many apps use it.
    pub app_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeOptions {
    pub everywhere: ScopeOption,
    pub styles: Vec<ScopeOption>,
    pub apps: Vec<ScopeOption>,
}

impl ScopeOptions {
    /// Everywhere, then the styles, then the apps.
    pub fn all(&self) -> impl Iterator<Item = &ScopeOption> {
        std::iter::once(&self.everywhere)
            .chain(&self.styles)
            .chain(&self.apps)
    }

    pub fn find(&self, key: &str) -> Option<&ScopeOption> {
        self.all().find(|o| o.key == key)
    }
}

/// The scope list: everywhere, then each style in order, then each app by
/// name. An app with entries but no longer in the styles listing still shows,
/// so its entries stay reachable.
pub fn build_scope_options(
    data: Option<&WritingStylesResponse>,
    entries: &[DictionaryEntry],
) -> ScopeOptions {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        for key in entry_place_keys(entry) {
            *counts.entry(key).or_default() += 1;
        }
    }
    let option = |scope: Scope, name: String| {
        let key = scope.key();
        ScopeOption {
            count: counts.get(&key).copied().unwrap_or(0),
            key,
            scope,
            name,
            style_id: None,
            app_count: 0,
        }
    };

    let listed_apps = data.map(|d| d.apps.as_slice()).unwrap_or_default();
    let mut styles: Vec<_> = data.map(|d| d.styles.iter().collect()).unwrap_or_default();
    styles.sort_by_key(|s| s.position);
    let styles = styles
        .into_iter()
        .map(|style| ScopeOption {
            app_count: listed_apps
                .iter()
                .filter(|app| app.style_id == style.id)
                .count(),
            ..option(Scope::Style(style.id.clone()), style.name.clone())
        })
        .collect();

    let mut apps: Vec<(String, String, Option<String>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for app in listed_apps {
        if seen.insert(app.bundle_id.clone()) {
            let name = app
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| app.bundle_id.clone());
            apps.push((app.bundle_id.clone(), name, Some(app.style_id.clone())));
        }
    }
    for entry in entries {
        for place in &entry.places {
            let Some(bundle) = place.scope_id.as_ref().filter(|id| !id.is_empty()) else {
                continue;
            };
            if place.scope == DictionaryScope::App && seen.insert(bundle.clone()) {
                let name = place
                    .app_name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| bundle.clone());
                apps.push((bundle.clone(), name, None));
            }
        }
    }
    let mut apps: Vec<ScopeOption> = apps
        .into_iter()
        .map(|(bundle, name, style_id)| ScopeOption {
            style_id,
            ..option(Scope::App(bundle), name)
        })
        .collect();
    apps.sort_by_cached_key(|o| o.name.to_lowercase());

    ScopeOptions {
        everywhere: option(Scope::Global, String::new()),
        styles,
        apps,
    }
}

/// The body for one place: an app carries its name so the server can show it later.
pub fn place_input(scope: &Scope, app_name: Option<&str>) -> DictionaryPlaceInput {
    match scope {
        Scope::Global => DictionaryPlaceInput::global(),
        Scope::Style(id) => DictionaryPlaceInput::style(id.clone()),
        Scope::App(bundle) => DictionaryPlaceInput::app(bundle.clone(), app_name.map(String::from)),
    }
}

/// The body that adds a word to one scope; a blank "when I say" teaches only the spelling.
pub fn new_entry(
    scope: &Scope,
    written: &str,
    spoken: &str,
    app_name: Option<&str>,
) -> DictionaryEntryCreate {
    let spoken = spoken.trim();
    DictionaryEntryCreate {
        written: written.trim().to_string(),
        spoken: (!spoken.is_empty()).then(|| spoken.to_string()),
        places: vec![place_input(scope, app_name)],
        match_sound: None,
        phrase: None,
    }
}

/// The body that adds a phrase: what is said, and the text it writes with
/// its lines kept. The server tidies each line.
pub fn new_phrase(
    spoken: &str,
    written: &str,
    places: Vec<DictionaryPlaceInput>,
) -> DictionaryEntryCreate {
    DictionaryEntryCreate {
        written: written.trim().to_string(),
        spoken: Some(spoken.trim().to_string()),
        places,
        match_sound: None,
        phrase: Some(true),
    }
}

/// What a row notes beside its word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryNote {
    /// Added when the user spelled it aloud.
    SpokenFix,
    /// Added from a correction in a transcript.
    Correction,
    /// Fixed only where spelled exactly.
    Exact,
}

pub fn entry_note(source: DictionarySource, match_sound: bool) -> Option<EntryNote> {
    match source {
        DictionarySource::SpokenFix => Some(EntryNote::SpokenFix),
        DictionarySource::Correction => Some(EntryNote::Correction),
        _ if !match_sound => Some(EntryNote::Exact),
        _ => None,
    }
}

/// The place bodies for chosen scope keys, naming apps from the scope list.
pub fn places_from_keys(keys: &[String], options: &ScopeOptions) -> Vec<DictionaryPlaceInput> {
    keys.iter()
        .map(|key| {
            let name = options
                .apps
                .iter()
                .find(|o| &o.key == key)
                .map(|o| o.name.as_str());
            place_input(&Scope::parse(key), name)
        })
        .collect()
}

/// Checking or unchecking one place in the "Applies in" menu. Everywhere
/// clears the rest; anything else clears everywhere. Unchecking the last
/// place leaves none, which the caller refuses to save.
pub fn toggle_place(selected: &[String], key: &str) -> Vec<String> {
    if selected.iter().any(|k| k == key) {
        return selected.iter().filter(|k| *k != key).cloned().collect();
    }
    if key == EVERYWHERE_KEY {
        return vec![EVERYWHERE_KEY.to_string()];
    }
    selected
        .iter()
        .filter(|k| *k != EVERYWHERE_KEY)
        .cloned()
        .chain(std::iter::once(key.to_string()))
        .collect()
}

pub fn same_places(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && b.iter().all(|key| a.contains(key))
}

/// Whether written text reads as code (an identifier, path or domain), to show in monospace.
pub fn looks_like_code(text: &str) -> bool {
    text.contains(['_', '.', '/']) && !text.chars().any(char::is_whitespace)
}

/// The first few written words, then "…" when there are more.
pub fn preview_written(entries: &[InheritedEntry], count: usize) -> String {
    let words: Vec<&str> = entries
        .iter()
        .take(count)
        .map(|e| e.written.as_str())
        .collect();
    if entries.len() > count {
        format!("{}, …", words.join(", "))
    } else {
        words.join(", ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InheritedEntry {
    pub id: String,
    pub written: String,
    pub spoken: Option<String>,
    pub phrase: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InheritedSource {
    Style,
    Global,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InheritedGroup {
    pub source: InheritedSource,
    /// The style's id, for a style group.
    pub scope_id: Option<String>,
    pub entries: Vec<InheritedEntry>,
    pub preview: String,
}

impl InheritedGroup {
    fn new(
        source: InheritedSource,
        scope_id: Option<String>,
        entries: Vec<InheritedEntry>,
    ) -> Self {
        Self {
            preview: preview_written(&entries, 2),
            source,
            scope_id,
            entries,
        }
    }

    /// Stable across renders, for remembering which groups are open.
    pub fn key(&self) -> String {
        match self.source {
            InheritedSource::Style => format!("style:{}", self.scope_id.as_deref().unwrap_or("")),
            InheritedSource::Global => EVERYWHERE_KEY.to_string(),
        }
    }
}

/// What an app picks up from elsewhere: its style's entries, then
/// everywhere's. An entry that is also the app's own is left out, and so is
/// one a more specific entry replaces there: it doesn't apply in the app.
/// Empty groups are dropped.
pub fn inherited_for_app(resolved: &[ResolvedDictionaryEntry]) -> Vec<InheritedGroup> {
    let own: HashSet<&str> = resolved
        .iter()
        .filter(|row| row.scope == DictionaryScope::App)
        .map(|row| row.id.as_str())
        .collect();
    let pick = |scope: DictionaryScope| {
        let mut seen: HashSet<&str> = HashSet::new();
        resolved
            .iter()
            .filter(|row| {
                row.scope == scope
                    && !row.overridden
                    && !own.contains(row.id.as_str())
                    && seen.insert(row.id.as_str())
            })
            .collect::<Vec<_>>()
    };
    let resolved_entry = |row: &&ResolvedDictionaryEntry| InheritedEntry {
        id: row.id.clone(),
        written: row.written.clone(),
        spoken: row.spoken.clone(),
        phrase: row.phrase,
    };
    let mut groups = Vec::new();
    let style = pick(DictionaryScope::Style);
    if let Some(first) = style.first() {
        groups.push(InheritedGroup::new(
            InheritedSource::Style,
            first.scope_id.clone(),
            style.iter().map(resolved_entry).collect(),
        ));
    }
    let global = pick(DictionaryScope::Global);
    if !global.is_empty() {
        groups.push(InheritedGroup::new(
            InheritedSource::Global,
            None,
            global.iter().map(resolved_entry).collect(),
        ));
    }
    groups
}

/// What a style picks up: everywhere's entries, newest first, less those its own replace.
pub fn inherited_for_style(entries: &[DictionaryEntry], style: &Scope) -> Vec<InheritedGroup> {
    let own: HashSet<String> = entries_in(entries, style)
        .into_iter()
        .map(|e| said_key(&e.written, e.spoken.as_deref()))
        .collect();
    let global: Vec<InheritedEntry> = entries_in(entries, &Scope::Global)
        .into_iter()
        .filter(|e| !own.contains(&said_key(&e.written, e.spoken.as_deref())))
        .map(inherited_entry)
        .collect();
    if global.is_empty() {
        Vec::new()
    } else {
        vec![InheritedGroup::new(InheritedSource::Global, None, global)]
    }
}

fn inherited_entry(entry: &DictionaryEntry) -> InheritedEntry {
    InheritedEntry {
        id: entry.id.clone(),
        written: entry.written.clone(),
        spoken: entry.spoken.clone(),
        phrase: entry.phrase,
    }
}

/// Words or phrases: the page shows one kind at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EntryKind {
    #[default]
    Words,
    Phrases,
}

impl EntryKind {
    pub fn of(phrase: bool) -> EntryKind {
        if phrase {
            EntryKind::Phrases
        } else {
            EntryKind::Words
        }
    }
}

/// Inherited groups with only `kind`'s entries; groups left empty are dropped.
pub fn inherited_of_kind(groups: Vec<InheritedGroup>, kind: EntryKind) -> Vec<InheritedGroup> {
    groups
        .into_iter()
        .filter_map(|g| {
            let entries: Vec<_> = g
                .entries
                .into_iter()
                .filter(|e| EntryKind::of(e.phrase) == kind)
                .collect();
            (!entries.is_empty()).then(|| InheritedGroup::new(g.source, g.scope_id, entries))
        })
        .collect()
}

/// How long ago an entry was added: minutes or hours within a day, then its date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryAge {
    Now,
    Minutes(i64),
    Hours(i64),
    /// The local date, and whether it is this year.
    Date(chrono::NaiveDate, bool),
}

pub fn entry_age(created_at: &str, now: DateTime<Utc>) -> Option<EntryAge> {
    let date = parse_server_date(created_at)?;
    let minutes = (now - date).num_minutes();
    Some(if minutes < 1 {
        EntryAge::Now
    } else if minutes < 60 {
        EntryAge::Minutes(minutes)
    } else if minutes < 24 * 60 {
        EntryAge::Hours(minutes / 60)
    } else {
        let local = date.with_timezone(&chrono::Local).date_naive();
        let this_year = now.with_timezone(&chrono::Local).year();
        EntryAge::Date(local, local.year() == this_year)
    })
}

/// The words an entry matches on, as the server compares them: what's said, else what's written.
fn said_key(written: &str, spoken: Option<&str>) -> String {
    spoken
        .unwrap_or(written)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// What was said as a phrase to match: one line, lowercase, without the
/// punctuation around it.
pub fn spoken_phrase(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    line.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
        .chars()
        .take(MAX_LENGTH)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kass_api::types::{PunctuationStyle, StyledApp, WritingStyle};

    fn place(scope: DictionaryScope, id: Option<&str>, app: Option<&str>) -> DictionaryPlace {
        DictionaryPlace {
            scope,
            scope_id: id.map(String::from),
            app_name: app.map(String::from),
        }
    }

    fn entry(id: &str, written: &str, places: Vec<DictionaryPlace>, at: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: id.into(),
            written: written.into(),
            spoken: None,
            places,
            created_at: Some(at.into()),
            match_sound: true,
            source: DictionarySource::User,
            phrase: false,
        }
    }

    fn global() -> DictionaryPlace {
        place(DictionaryScope::Global, None, None)
    }

    fn styles() -> WritingStylesResponse {
        let style = |id: &str, name: &str, position| WritingStyle {
            id: id.into(),
            name: name.into(),
            position,
            is_default: false,
            punctuation_style: PunctuationStyle::Standard,
            preserve_technical: false,
            description: String::new(),
        };
        let app = |bundle: &str, name: Option<&str>, style: &str| StyledApp {
            bundle_id: bundle.into(),
            name: name.map(String::from),
            style_id: style.into(),
            confirmed: true,
            count: 0,
            corrections: 0,
            suggested_style_id: None,
        };
        WritingStylesResponse {
            styles: vec![style("b", "Work", 1), style("a", "Casual", 0)],
            apps: vec![
                app("com.tinyspeck.slackmacgap", Some("Slack"), "a"),
                app("com.apple.mail", None, "b"),
                app("com.apple.notes", Some("notes"), "a"),
            ],
            max_styles: 5,
            cache_mb_per_style: None,
        }
    }

    #[test]
    fn scope_keys_round_trip() {
        for scope in [
            Scope::Global,
            Scope::Style("s1".into()),
            Scope::App("com.x".into()),
        ] {
            assert_eq!(Scope::parse(&scope.key()), scope);
        }
        assert_eq!(Scope::parse("style:"), Scope::Global);
        assert_eq!(Scope::parse("nonsense"), Scope::Global);
    }

    #[test]
    fn place_keys_are_deduped() {
        let e = entry(
            "1",
            "Kass",
            vec![
                global(),
                place(DictionaryScope::Style, Some("a"), None),
                place(DictionaryScope::Style, Some("a"), None),
                place(DictionaryScope::App, None, None),
            ],
            "2026-01-01 00:00:00",
        );
        assert_eq!(entry_place_keys(&e), vec!["global", "style:a"]);
    }

    #[test]
    fn entries_in_a_scope_are_newest_first() {
        let entries = vec![
            entry("old", "a", vec![global()], "2026-01-01 00:00:00"),
            entry("new", "b", vec![global()], "2026-03-01T00:00:00Z"),
            entry(
                "other",
                "c",
                vec![place(DictionaryScope::Style, Some("a"), None)],
                "2026-02-01 00:00:00",
            ),
        ];
        let ids: Vec<_> = entries_in(&entries, &Scope::Global)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, vec!["new", "old"]);
    }

    #[test]
    fn scope_options_list_styles_in_order_and_apps_by_name() {
        let entries = vec![
            entry("1", "a", vec![global()], "2026-01-01 00:00:00"),
            entry(
                "2",
                "b",
                vec![
                    place(DictionaryScope::Style, Some("a"), None),
                    place(DictionaryScope::App, Some("com.gone"), Some("Zed")),
                ],
                "2026-01-01 00:00:00",
            ),
        ];
        let data = styles();
        let options = build_scope_options(Some(&data), &entries);
        assert_eq!(options.everywhere.count, 1);
        assert_eq!(options.everywhere.name, "");
        let styles: Vec<_> = options
            .styles
            .iter()
            .map(|o| (o.name.as_str(), o.count, o.app_count))
            .collect();
        assert_eq!(styles, vec![("Casual", 1, 2), ("Work", 0, 1)]);
        let apps: Vec<_> = options
            .apps
            .iter()
            .map(|o| (o.name.as_str(), o.style_id.as_deref(), o.count))
            .collect();
        assert_eq!(
            apps,
            vec![
                ("com.apple.mail", Some("b"), 0),
                ("notes", Some("a"), 0),
                ("Slack", Some("a"), 0),
                ("Zed", None, 1),
            ]
        );
        assert_eq!(options.find("app:com.gone").map(|o| o.count), Some(1));
    }

    #[test]
    fn new_entries_trim_and_drop_a_blank_spoken() {
        let body = new_entry(&Scope::App("com.x".into()), " Kass ", "  ", Some("X"));
        assert_eq!(body.written, "Kass");
        assert_eq!(body.spoken, None);
        assert_eq!(
            body.places,
            vec![DictionaryPlaceInput::app("com.x", Some("X".into()))]
        );
        let phrase = new_phrase(
            " my email ",
            "a@b.c\n",
            vec![DictionaryPlaceInput::global()],
        );
        assert_eq!(phrase.spoken.as_deref(), Some("my email"));
        assert_eq!(phrase.written, "a@b.c");
        assert_eq!(phrase.phrase, Some(true));
    }

    #[test]
    fn notes_follow_the_source_then_exactness() {
        assert_eq!(
            entry_note(DictionarySource::SpokenFix, false),
            Some(EntryNote::SpokenFix)
        );
        assert_eq!(
            entry_note(DictionarySource::Correction, true),
            Some(EntryNote::Correction)
        );
        assert_eq!(
            entry_note(DictionarySource::User, false),
            Some(EntryNote::Exact)
        );
        assert_eq!(entry_note(DictionarySource::User, true), None);
    }

    #[test]
    fn toggling_places() {
        let keys = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            toggle_place(&keys(&["global"]), "style:a"),
            keys(&["style:a"])
        );
        assert_eq!(
            toggle_place(&keys(&["style:a", "app:x"]), "global"),
            keys(&["global"])
        );
        assert_eq!(toggle_place(&keys(&["style:a"]), "style:a"), keys(&[]));
        assert!(same_places(&keys(&["a", "b"]), &keys(&["b", "a"])));
        assert!(!same_places(&keys(&["a"]), &keys(&["a", "b"])));
    }

    #[test]
    fn places_from_keys_name_apps() {
        let data = styles();
        let options = build_scope_options(Some(&data), &[]);
        let places = places_from_keys(
            &["global".into(), "app:com.tinyspeck.slackmacgap".into()],
            &options,
        );
        assert_eq!(
            places,
            vec![
                DictionaryPlaceInput::global(),
                DictionaryPlaceInput::app("com.tinyspeck.slackmacgap", Some("Slack".into())),
            ]
        );
    }

    #[test]
    fn code_looks_like_code() {
        assert!(looks_like_code("kass.mrgnhnt.com"));
        assert!(looks_like_code("snake_case"));
        assert!(!looks_like_code("Kass"));
        assert!(!looks_like_code("a.b c"));
    }

    fn resolved(
        id: &str,
        scope: DictionaryScope,
        scope_id: Option<&str>,
    ) -> ResolvedDictionaryEntry {
        ResolvedDictionaryEntry {
            id: id.into(),
            written: id.to_uppercase(),
            spoken: None,
            scope,
            scope_id: scope_id.map(String::from),
            app_name: None,
            created_at: None,
            match_sound: true,
            phrase: false,
            overridden: false,
        }
    }

    #[test]
    fn an_app_inherits_its_style_then_everywhere() {
        let mut overridden = resolved("o", DictionaryScope::Global, None);
        overridden.overridden = true;
        let rows = vec![
            resolved("own", DictionaryScope::App, Some("com.x")),
            resolved("s1", DictionaryScope::Style, Some("a")),
            resolved("own", DictionaryScope::Global, None),
            resolved("g1", DictionaryScope::Global, None),
            resolved("g1", DictionaryScope::Global, None),
            resolved("g2", DictionaryScope::Global, None),
            resolved("g3", DictionaryScope::Global, None),
            overridden,
        ];
        let groups = inherited_for_app(&rows);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].source, InheritedSource::Style);
        assert_eq!(groups[0].scope_id.as_deref(), Some("a"));
        assert_eq!(groups[0].preview, "S1");
        let ids: Vec<_> = groups[1].entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["g1", "g2", "g3"]);
        assert_eq!(groups[1].preview, "G1, G2, …");
    }

    #[test]
    fn a_style_inherits_everywhere_less_what_it_replaces() {
        let style = Scope::Style("a".into());
        let mut own = entry(
            "own",
            "Kass",
            vec![place(DictionaryScope::Style, Some("a"), None)],
            "2026-01-02 00:00:00",
        );
        own.spoken = Some("cass".into());
        let mut replaced = entry("r", "Cass", vec![global()], "2026-01-01 00:00:00");
        replaced.spoken = Some("  CASS ".into());
        let mut phrase = entry("p", "hi@x.y", vec![global()], "2026-01-03 00:00:00");
        phrase.phrase = true;
        let kept = entry("k", "GPUI", vec![global()], "2026-01-01 00:00:00");
        let groups = inherited_for_style(&[own, replaced, phrase, kept], &style);
        let ids: Vec<_> = groups[0].entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["p", "k"]);
        let words = inherited_of_kind(groups.clone(), EntryKind::Words);
        assert_eq!(words[0].entries.len(), 1);
        assert_eq!(words[0].preview, "GPUI");
        let only_phrases = inherited_of_kind(
            inherited_of_kind(groups, EntryKind::Words),
            EntryKind::Phrases,
        );
        assert!(only_phrases.is_empty());
    }

    #[test]
    fn ages() {
        let now = parse_server_date("2026-06-15T12:00:00Z").unwrap();
        assert_eq!(entry_age("2026-06-15 11:59:30", now), Some(EntryAge::Now));
        assert_eq!(
            entry_age("2026-06-15 11:15:00", now),
            Some(EntryAge::Minutes(45))
        );
        assert_eq!(
            entry_age("2026-06-15T02:00:00+00:00", now),
            Some(EntryAge::Hours(10))
        );
        assert!(matches!(
            entry_age("2025-01-10 12:00:00", now),
            Some(EntryAge::Date(_, false))
        ));
        assert!(matches!(
            entry_age("2026-03-10 12:00:00", now),
            Some(EntryAge::Date(_, true))
        ));
        assert_eq!(entry_age("not a date", now), None);
    }

    #[test]
    fn spoken_phrases_are_one_tidy_line() {
        assert_eq!(spoken_phrase("  Insert my\n email.  "), "insert my email");
        assert_eq!(spoken_phrase("“Sign off!”"), "sign off");
        assert_eq!(spoken_phrase("...?"), "");
        assert_eq!(spoken_phrase(&"a".repeat(300)).len(), MAX_LENGTH);
    }
}
