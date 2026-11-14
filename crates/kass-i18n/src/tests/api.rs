//! Key→id mapping, markup runs, lookups and `tr!`.

use crate::{Run, fluent_id, has, locale, parse_markup, set_locale, t, tr};

#[test]
fn key_to_id() {
    assert_eq!(
        fluent_id("setup.models.downloadSize"),
        "setup-models-download-size"
    );
    assert_eq!(
        fluent_id("writingStyle.habits.drop_final_period"),
        "writing-style-habits-drop-final-period"
    );
    assert_eq!(
        fluent_id("settings.captures.storage.retention.30d"),
        "settings-captures-storage-retention-30d"
    );
    assert_eq!(
        fluent_id("models.problems.count_one"),
        "models-problems-count"
    );
    assert_eq!(
        fluent_id("models.problems.count_other"),
        "models-problems-count"
    );
    assert_eq!(
        fluent_id("settings.captures.storage.retention.ask.keep_zero"),
        "settings-captures-storage-retention-ask-keep"
    );
    assert_eq!(fluent_id("common"), "common");
    // `_other` alone is not a plural suffix.
    assert_eq!(fluent_id("a._other"), "a--other");
}

#[test]
fn colliding_key_uses_its_override() {
    assert_eq!(
        fluent_id("settings.features.onboarding.title"),
        "settings-features-onboarding-title"
    );
    assert_eq!(
        fluent_id("settings.features.onboardingTitle"),
        "settings-features-onboarding-section-title"
    );
    assert_eq!(
        t("settings-features-onboarding-section-title"),
        "Onboarding"
    );
}

fn plain(text: &str) -> Run {
    Run {
        text: text.into(),
        tag: None,
    }
}

fn tagged(text: &str, tag: &str) -> Run {
    Run {
        text: text.into(),
        tag: Some(tag.into()),
    }
}

#[test]
fn markup_runs() {
    assert_eq!(
        parse_markup(
            "Kass needs <path>System Settings → Privacy</path> to paste.",
            &["path"]
        ),
        vec![
            plain("Kass needs "),
            tagged("System Settings → Privacy", "path"),
            plain(" to paste.")
        ]
    );
    assert_eq!(
        parse_markup("Hold <keys/> or <keys /> now", &["keys"]),
        vec![
            plain("Hold "),
            tagged("", "keys"),
            plain(" or "),
            tagged("", "keys"),
            plain(" now")
        ]
    );
    assert_eq!(
        parse_markup("<strong>all</strong>", &["strong"]),
        vec![tagged("all", "strong")]
    );
    assert_eq!(parse_markup("", &["strong"]), vec![]);
}

#[test]
fn markup_unknown_and_unclosed_tags_stay_literal() {
    assert_eq!(
        parse_markup("a <em>b</em> c", &["strong"]),
        vec![plain("a <em>b</em> c")]
    );
    assert_eq!(
        parse_markup("a <strong>b c", &["strong"]),
        vec![plain("a <strong>b c")]
    );
    assert_eq!(
        parse_markup("1 < 2 <> <strong", &["strong"]),
        vec![plain("1 < 2 <> <strong")]
    );
    assert_eq!(
        parse_markup("x <em>y</em> <strong>z</strong>", &["strong"]),
        vec![plain("x <em>y</em> "), tagged("z", "strong")]
    );
}

#[test]
fn tr_formats_plurals_with_numbers() {
    assert_eq!(tr!("models-problems-count", count = 1), "1 problem");
    assert_eq!(tr!("models-problems-count", count = 2_u64), "2 problems");
    assert_eq!(tr!("models-problems-count", count = 0), "0 problems");
    // `_zero` became the exact-match variant `[0]`.
    assert_eq!(
        tr!(
            "settings-captures-storage-retention-ask-keep",
            window = "30 days",
            count = 0
        ),
        "Keep 30 days"
    );
    assert_eq!(
        tr!(
            "settings-captures-storage-retention-ask-keep",
            window = "30 days",
            count = 3,
        ),
        "Keep 30 days, delete 3"
    );
    assert_eq!(tr!("common-cancel"), "Cancel");
}

#[test]
fn missing_ids_and_args_do_not_panic() {
    assert_eq!(t("no-such-message"), "no-such-message");
    assert!(!has("no-such-message"));
    assert!(has("common-cancel"));
    // A missing argument shows the placeable's name instead of panicking.
    assert_eq!(
        tr!("models-delete-dialog-body"),
        "Are you sure you want to delete <strong>{$name}</strong>?"
    );
}

#[test]
fn only_shipped_locales_can_be_selected() {
    assert!(!set_locale("xx-unknown"));
    assert!(!set_locale("not a tag!"));
    assert!(set_locale("en"));
    assert_eq!(locale(), "en");
}
