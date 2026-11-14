//! The embedded locales and the global bundle lookups.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use unic_langid::LanguageIdentifier;

/// One locale's `.ftl` files, embedded at compile time.
pub(crate) struct Locale {
    /// BCP 47 tag, also the folder name under `locales/`.
    pub(crate) tag: &'static str,
    /// `(file stem, source)` for every section.
    pub(crate) files: &'static [(&'static str, &'static str)],
}

/// Embeds `locales/<tag>/<section>.ftl` for every section. A new section is
/// a new line here (and a file in every locale folder).
macro_rules! locale {
    ($tag:literal) => {
        Locale {
            tag: $tag,
            files: &[
                section!($tag, "app-location"),
                section!($tag, "captures"),
                section!($tag, "common"),
                section!($tag, "dictation"),
                section!($tag, "dictionary"),
                section!($tag, "insights"),
                section!($tag, "models"),
                section!($tag, "nav"),
                section!($tag, "onboarding"),
                section!($tag, "palette"),
                section!($tag, "reports"),
                section!($tag, "settings"),
                section!($tag, "setup"),
                section!($tag, "writing-style"),
            ],
        }
    };
}

macro_rules! section {
    ($tag:literal, $name:literal) => {
        (
            $name,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/locales/",
                $tag,
                "/",
                $name,
                ".ftl"
            )),
        )
    };
}

/// Every shipped locale. The first one is the default and the fallback for
/// messages a locale lacks. Adding a locale is adding its folder and a line.
pub(crate) const LOCALES: &[Locale] = &[locale!("en")];

static BUNDLES: LazyLock<Vec<FluentBundle<FluentResource>>> =
    LazyLock::new(|| LOCALES.iter().map(build_bundle).collect());

/// Index into [`LOCALES`] of the current locale.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn build_bundle(locale: &Locale) -> FluentBundle<FluentResource> {
    let langid: LanguageIdentifier = locale.tag.parse().expect("locale tags are valid BCP 47");
    let mut bundle = FluentBundle::new_concurrent(vec![langid]);
    // No Unicode isolation marks around placeables: they would show up in UI
    // text and break string comparisons.
    bundle.set_use_isolating(false);
    for (name, source) in locale.files {
        // Parse errors and duplicate ids are caught by the tests; at runtime
        // keep whatever did parse.
        let resource =
            FluentResource::try_new((*source).to_owned()).unwrap_or_else(|(res, errors)| {
                warn_once(format!("parse:{}/{name}", locale.tag), || {
                    format!(
                        "kass-i18n: {}/{name}.ftl has syntax errors: {errors:?}",
                        locale.tag
                    )
                });
                res
            });
        if let Err(errors) = bundle.add_resource(resource) {
            warn_once(format!("add:{}/{name}", locale.tag), || {
                format!("kass-i18n: {}/{name}.ftl: {errors:?}", locale.tag)
            });
        }
    }
    bundle
}

/// Switches the current locale. Returns `false` (and changes nothing) if
/// `tag` is not a shipped locale.
pub fn set_locale(tag: &str) -> bool {
    let Ok(wanted) = tag.parse::<LanguageIdentifier>() else {
        return false;
    };
    let found = LOCALES.iter().position(|l| {
        l.tag
            .parse::<LanguageIdentifier>()
            .is_ok_and(|id| id == wanted)
    });
    if let Some(index) = found {
        CURRENT.store(index, Ordering::Relaxed);
    }
    found.is_some()
}

/// The current locale's tag, such as `"en"`.
pub fn locale() -> &'static str {
    LOCALES[CURRENT.load(Ordering::Relaxed)].tag
}

/// Whether a message with this id exists (in the current locale or the
/// fallback). Use it where React passed `defaultValue` for a dynamic key.
pub fn has(id: &str) -> bool {
    lookup(id).is_some()
}

/// Formats a message that takes no arguments.
///
/// A missing id returns the id itself (and, in debug builds, logs it once).
pub fn t(id: &str) -> String {
    format(id, None)
}

/// Formats a message with arguments. Pass numbers as numbers (not strings)
/// so `$count` plural selection works; see also [`tr!`](crate::tr).
///
/// A missing id returns the id itself (and, in debug builds, logs it once).
/// Formatting errors, such as a missing argument, never panic: Fluent
/// writes the placeable's name (`{$name}`) in its place.
pub fn t_args(id: &str, args: &FluentArgs) -> String {
    format(id, Some(args))
}

fn lookup(id: &str) -> Option<&'static FluentBundle<FluentResource>> {
    let current = &BUNDLES[CURRENT.load(Ordering::Relaxed)];
    let fallback = &BUNDLES[0];
    [current, fallback]
        .into_iter()
        .find(|b| b.get_message(id).is_some_and(|m| m.value().is_some()))
}

fn format(id: &str, args: Option<&FluentArgs>) -> String {
    let Some(bundle) = lookup(id) else {
        warn_once(format!("missing:{id}"), || {
            format!("kass-i18n: missing message `{id}`")
        });
        return id.to_owned();
    };
    let Some(pattern) = bundle.get_message(id).and_then(|m| m.value()) else {
        return id.to_owned();
    };
    let mut errors = Vec::new();
    let text = bundle
        .format_pattern(pattern, args, &mut errors)
        .into_owned();
    if !errors.is_empty() {
        warn_once(format!("format:{id}"), || {
            format!("kass-i18n: formatting `{id}`: {errors:?}")
        });
    }
    text
}

/// In debug builds, prints `message()` the first time `key` is seen.
#[cfg(debug_assertions)]
fn warn_once(key: String, message: impl FnOnce() -> String) {
    use std::collections::HashSet;
    use std::sync::Mutex;

    static SEEN: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
    let first = SEEN
        .lock()
        .map(|mut seen| seen.insert(key))
        .unwrap_or(false);
    if first {
        eprintln!("{}", message());
    }
}

#[cfg(not(debug_assertions))]
fn warn_once(_key: String, _message: impl FnOnce() -> String) {}
