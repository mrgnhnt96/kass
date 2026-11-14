//! i18next key → Fluent message id.

/// i18next plural suffixes. A key ending in one of these is one form of a
/// plural set, and the whole set is one Fluent message.
pub(crate) const PLURAL_SUFFIXES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// Keys whose scheme id would collide with another key's id, and the id they
/// use instead.
const ID_OVERRIDES: &[(&str, &str)] = &[
    // `settings.features.onboarding.title` already takes
    // `settings-features-onboarding-title`.
    (
        "settings.features.onboardingTitle",
        "settings-features-onboarding-section-title",
    ),
];

/// Maps an i18next key to its Fluent message id.
///
/// Each dot-separated segment goes from camelCase to kebab-case (an
/// underscore also becomes `-`), and the segments are joined with `-`. A
/// plural suffix (`_one`, `_other`, …) is dropped, since the whole plural set
/// is one message. A handful of keys whose ids would collide use a fixed id
/// instead.
///
/// ```
/// use kass_i18n::fluent_id;
///
/// assert_eq!(fluent_id("setup.models.downloadSize"), "setup-models-download-size");
/// assert_eq!(fluent_id("models.problems.count_one"), "models-problems-count");
/// ```
pub fn fluent_id(key: &str) -> String {
    let key = strip_plural_suffix(key).unwrap_or(key);
    if let Some((_, id)) = ID_OVERRIDES.iter().find(|(k, _)| *k == key) {
        return (*id).to_owned();
    }
    let mut id = String::with_capacity(key.len() + 8);
    for (i, segment) in key.split('.').enumerate() {
        if i > 0 {
            id.push('-');
        }
        push_kebab(&mut id, segment);
    }
    id
}

/// The key without its plural suffix, if its last segment has one.
pub(crate) fn strip_plural_suffix(key: &str) -> Option<&str> {
    let last = key.rsplit('.').next().unwrap_or(key);
    let (base, suffix) = last.rsplit_once('_')?;
    if base.is_empty() || !PLURAL_SUFFIXES.contains(&suffix) {
        return None;
    }
    Some(&key[..key.len() - suffix.len() - 1])
}

fn push_kebab(out: &mut String, segment: &str) {
    let start = out.len();
    for ch in segment.chars() {
        if ch == '_' {
            out.push('-');
        } else if ch.is_ascii_uppercase() {
            if out.len() > start && !out.ends_with('-') {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
}
