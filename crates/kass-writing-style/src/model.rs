//! What the page shows, worked out from the server's data: the apps in a
//! style, the app list's filter, recent corrections, dates and the teach
//! conversation's kinds and chips.

use chrono::{DateTime, Duration, Local, NaiveDateTime, TimeZone};
use gpui_kit::assets::IconName;
use kass_api::types::{
    PersonalExample, PersonalExampleSource, StyledApp, TeachChip, TeachKind, WritingStyle,
    WritingStyleHabit, WritingStylesResponse,
};

/// The longest style name.
pub const MAX_NAME: usize = 40;
/// The longest style description.
pub const MAX_DESCRIPTION: usize = 600;
/// The longest teach reply.
pub const MAX_REPLY: usize = 4000;
/// App tiles a style card shows before "+N more".
pub const PREVIEW_APPS: usize = 3;
/// Corrections the page lists; the rest are under Your examples.
pub const RECENT_LIMIT: usize = 5;
/// Chips a conversation shows before "+N".
pub const CHIP_LIMIT: usize = 3;

/// What an app is called: its name, or its bundle id.
pub fn app_label(app: &StyledApp) -> String {
    app.name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| app.bundle_id.clone())
}

fn by_use(a: &&StyledApp, b: &&StyledApp) -> std::cmp::Ordering {
    b.count
        .cmp(&a.count)
        .then_with(|| app_label(a).cmp(&app_label(b)))
}

/// A style's apps, most dictated first.
pub fn apps_in<'a>(data: &'a WritingStylesResponse, style_id: &str) -> Vec<&'a StyledApp> {
    let mut apps: Vec<_> = data
        .apps
        .iter()
        .filter(|app| app.style_id == style_id)
        .collect();
    apps.sort_by(by_use);
    apps
}

/// The style apps use when they aren't in one.
pub fn default_style(data: &WritingStylesResponse) -> Option<&WritingStyle> {
    data.styles
        .iter()
        .find(|style| style.is_default)
        .or(data.styles.first())
}

/// The style shown: the one asked for if it still exists, else the default.
pub fn chosen_style<'a>(
    data: &'a WritingStylesResponse,
    wanted: Option<&str>,
) -> Option<&'a WritingStyle> {
    wanted
        .and_then(|id| data.styles.iter().find(|style| style.id == id))
        .or_else(|| default_style(data))
}

/// What the app list is narrowed to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AppFilter {
    #[default]
    All,
    Style(String),
    New,
}

/// The app list's rows: most dictated first, narrowed by `filter` and by
/// `query` in the app's name.
pub fn filter_apps<'a>(
    data: &'a WritingStylesResponse,
    filter: &AppFilter,
    query: &str,
) -> Vec<&'a StyledApp> {
    let query = query.trim().to_lowercase();
    let mut apps: Vec<_> = data
        .apps
        .iter()
        .filter(|app| match filter {
            AppFilter::Style(id) => &app.style_id == id,
            AppFilter::New => !app.confirmed,
            AppFilter::All => true,
        })
        .filter(|app| query.is_empty() || app_label(app).to_lowercase().contains(&query))
        .collect();
    apps.sort_by(by_use);
    apps
}

/// Whether another style can be made: the styles plus one being named stay
/// under the limit.
pub fn can_add_style(data: &WritingStylesResponse, drafting: bool) -> bool {
    (data.styles.len() + usize::from(drafting)) < data.max_styles as usize
}

/// The newest corrections, at most [`RECENT_LIMIT`].
pub fn recent_corrections(examples: &[PersonalExample]) -> Vec<&PersonalExample> {
    let mut corrections: Vec<_> = examples
        .iter()
        .filter(|example| example.source == PersonalExampleSource::Correction)
        .collect();
    corrections.sort_by(|a, b| {
        b.created_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.created_at.as_deref().unwrap_or(""))
    });
    corrections.truncate(RECENT_LIMIT);
    corrections
}

/// A server time (RFC 3339, or without a zone, read as local time).
pub fn parse_time(text: &str) -> Option<DateTime<Local>> {
    if let Ok(time) = DateTime::parse_from_rfc3339(text) {
        return Some(time.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()?;
    Local.from_local_datetime(&naive).earliest()
}

/// "Nov 16".
pub fn short_date(time: &DateTime<Local>) -> String {
    time.format("%b %-d").to_string()
}

/// When a correction was made, as a recent one shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    On(String),
}

pub fn day_of(time: &DateTime<Local>, now: &DateTime<Local>) -> Day {
    let start_of_today = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
        .unwrap_or(*now);
    if *time >= start_of_today {
        Day::Today
    } else if *time >= start_of_today - Duration::days(1) {
        Day::Yesterday
    } else {
        Day::On(short_date(time))
    }
}

/// The message id a habit's chip and explanation use, after the prefix.
pub fn habit_key(habit: WritingStyleHabit) -> Option<&'static str> {
    Some(match habit {
        WritingStyleHabit::BoundaryPeriod => "boundary-period",
        WritingStyleHabit::BoundaryComma => "boundary-comma",
        WritingStyleHabit::BoundaryNone => "boundary-none",
        WritingStyleHabit::LowercaseStart => "lowercase-start",
        WritingStyleHabit::DropIntroComma => "drop-intro-comma",
        WritingStyleHabit::DropConjunctionComma => "drop-conjunction-comma",
        WritingStyleHabit::DropFinalPeriod => "drop-final-period",
        WritingStyleHabit::Unknown => return None,
    })
}

/// Every kind of conversation, in the order the server lists them.
pub const TEACH_KINDS: [TeachKind; 8] = [
    TeachKind::CodingAgent,
    TeachKind::DesignFeedback,
    TeachKind::Notes,
    TeachKind::Writeup,
    TeachKind::TeamChat,
    TeachKind::IssueComment,
    TeachKind::Email,
    TeachKind::TextMessage,
];

/// A kind's message id, after `writing-style-teach-kinds-`.
pub fn kind_key(kind: TeachKind) -> &'static str {
    match kind {
        TeachKind::CodingAgent => "coding-agent",
        TeachKind::DesignFeedback => "design-feedback",
        TeachKind::Notes => "notes",
        TeachKind::Writeup => "writeup",
        TeachKind::TeamChat => "team-chat",
        TeachKind::IssueComment => "issue-comment",
        TeachKind::Email => "email",
        TeachKind::TextMessage | TeachKind::Unknown => "text-message",
    }
}

pub fn kind_icon(kind: TeachKind) -> IconName {
    match kind {
        TeachKind::CodingAgent => IconName::SquareTerminal,
        TeachKind::DesignFeedback => IconName::PenTool,
        TeachKind::Notes => IconName::NotebookPen,
        TeachKind::Writeup => IconName::FileText,
        TeachKind::TeamChat => IconName::Hash,
        TeachKind::IssueComment => IconName::CircleDot,
        TeachKind::Email => IconName::Mail,
        TeachKind::TextMessage | TeachKind::Unknown => IconName::MessageCircle,
    }
}

/// The kinds for the style's apps first, in catalog order, then the rest.
pub fn group_kinds(suggested: &[TeachKind]) -> (Vec<TeachKind>, Vec<TeachKind>) {
    TEACH_KINDS
        .iter()
        .copied()
        .partition(|kind| suggested.contains(kind))
}

/// The chips a conversation shows: all when expanded or few, else the
/// first `limit` and how many more there are.
pub fn visible_chips(chips: &[TeachChip], limit: usize, expanded: bool) -> (&[TeachChip], usize) {
    if expanded || chips.len() <= limit {
        (chips, 0)
    } else {
        (&chips[..limit], chips.len() - limit)
    }
}

/// A chip's code as a message id suffix: `kept_exact` → `kept-exact`.
pub fn chip_key(code: &str) -> String {
    code.replace('_', "-")
}

/// The first letter of a name, as an avatar shows it.
pub fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

/// What moving an app to a style takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovePlan {
    /// Already there and confirmed: nothing to do.
    Stay,
    /// Assign it, bringing any corrections along.
    Assign,
    /// It has corrections teaching another style: ask whether they come too.
    Ask,
}

pub fn plan_move(app: &StyledApp, to: &str) -> MovePlan {
    if app.style_id == to {
        if app.confirmed {
            MovePlan::Stay
        } else {
            MovePlan::Assign
        }
    } else if app.corrections > 0 {
        MovePlan::Ask
    } else {
        MovePlan::Assign
    }
}

/// The styles as they'll be once `bundle` is in `to`, shown before the server answers.
pub fn moved(data: &mut WritingStylesResponse, bundle: &str, to: &str) {
    if let Some(app) = data.apps.iter_mut().find(|app| app.bundle_id == bundle) {
        app.style_id = to.to_string();
        app.confirmed = true;
        app.suggested_style_id = None;
    }
}

/// Marks every new app confirmed in its current style.
pub fn confirmed(data: &mut WritingStylesResponse) {
    for app in data.apps.iter_mut().filter(|app| !app.confirmed) {
        app.confirmed = true;
    }
}

/// The style a new app is shown in until it's confirmed.
pub fn suggested_style<'a>(
    data: &'a WritingStylesResponse,
    app: &StyledApp,
) -> Option<&'a WritingStyle> {
    let find = |id: &str| data.styles.iter().find(|style| style.id == id);
    app.suggested_style_id
        .as_deref()
        .and_then(find)
        .or_else(|| find(&app.style_id))
        .or_else(|| default_style(data))
}

/// `text` cut to `max` characters, or `None` when it already fits.
pub fn clipped(text: &str, max: usize) -> Option<String> {
    text.char_indices()
        .nth(max)
        .map(|(at, _)| text[..at].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kass_api::types::PunctuationStyle;

    fn style(id: &str, is_default: bool) -> WritingStyle {
        WritingStyle {
            id: id.into(),
            name: id.to_uppercase(),
            position: 0,
            is_default,
            punctuation_style: PunctuationStyle::Standard,
            preserve_technical: true,
            description: String::new(),
        }
    }

    fn app(
        bundle: &str,
        name: Option<&str>,
        style: &str,
        count: u64,
        confirmed: bool,
    ) -> StyledApp {
        StyledApp {
            bundle_id: bundle.into(),
            name: name.map(Into::into),
            style_id: style.into(),
            confirmed,
            count,
            corrections: 0,
            suggested_style_id: None,
        }
    }

    fn data() -> WritingStylesResponse {
        WritingStylesResponse {
            styles: vec![style("work", false), style("chat", true)],
            apps: vec![
                app("com.slack", Some("Slack"), "chat", 4, true),
                app("com.mail", Some("Mail"), "work", 9, true),
                app("com.notes", None, "work", 9, false),
                app("com.zed", Some("Zed"), "work", 1, false),
            ],
            max_styles: 3,
            cache_mb_per_style: None,
        }
    }

    #[test]
    fn labels_fall_back_to_the_bundle_id() {
        let data = data();
        assert_eq!(app_label(&data.apps[0]), "Slack");
        assert_eq!(app_label(&data.apps[2]), "com.notes");
    }

    #[test]
    fn a_styles_apps_are_most_used_first_then_by_name() {
        let data = data();
        let labels: Vec<_> = apps_in(&data, "work").into_iter().map(app_label).collect();
        assert_eq!(labels, ["Mail", "com.notes", "Zed"]);
    }

    #[test]
    fn the_shown_style_falls_back_to_the_default() {
        let data = data();
        assert_eq!(chosen_style(&data, Some("work")).unwrap().id, "work");
        assert_eq!(chosen_style(&data, Some("gone")).unwrap().id, "chat");
        assert_eq!(chosen_style(&data, None).unwrap().id, "chat");
    }

    #[test]
    fn the_app_list_filters_by_style_new_and_name() {
        let data = data();
        let labels = |filter: AppFilter, query: &str| -> Vec<String> {
            filter_apps(&data, &filter, query)
                .into_iter()
                .map(app_label)
                .collect()
        };
        assert_eq!(
            labels(AppFilter::All, ""),
            ["Mail", "com.notes", "Slack", "Zed"]
        );
        assert_eq!(labels(AppFilter::Style("chat".into()), ""), ["Slack"]);
        assert_eq!(labels(AppFilter::New, ""), ["com.notes", "Zed"]);
        assert_eq!(labels(AppFilter::All, " ZE "), ["Zed"]);
        assert!(labels(AppFilter::New, "slack").is_empty());
    }

    #[test]
    fn styles_stop_at_the_limit_counting_a_draft() {
        let mut data = data();
        assert!(can_add_style(&data, false));
        assert!(!can_add_style(&data, true));
        data.max_styles = 2;
        assert!(!can_add_style(&data, false));
    }

    fn example(id: &str, source: PersonalExampleSource, at: Option<&str>) -> PersonalExample {
        PersonalExample {
            id: id.into(),
            source,
            said: String::new(),
            meant: String::new(),
            created_at: at.map(Into::into),
            app_bundle_id: None,
            app_name: None,
        }
    }

    #[test]
    fn recent_corrections_are_the_newest_five_corrections() {
        let mut examples: Vec<_> = (1..=7)
            .map(|day| {
                example(
                    &day.to_string(),
                    PersonalExampleSource::Correction,
                    Some(&format!("2026-11-0{day}T10:00:00Z")),
                )
            })
            .collect();
        examples.push(example(
            "cal",
            PersonalExampleSource::Calibration,
            Some("2026-11-09T10:00:00Z"),
        ));
        let ids: Vec<_> = recent_corrections(&examples)
            .into_iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, ["7", "6", "5", "4", "3"]);
    }

    #[test]
    fn days_read_today_yesterday_or_a_date() {
        let now = Local.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        let at = |d, h| Local.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        assert_eq!(day_of(&at(16, 1), &now), Day::Today);
        assert_eq!(day_of(&at(15, 23), &now), Day::Yesterday);
        assert_eq!(day_of(&at(15, 0), &now), Day::Yesterday);
        assert_eq!(day_of(&at(14, 23), &now), Day::On("Nov 14".into()));
    }

    #[test]
    fn server_times_parse_with_or_without_a_zone() {
        assert!(parse_time("2026-11-16T10:00:00Z").is_some());
        assert!(parse_time("2026-11-16T10:00:00.123+02:00").is_some());
        let local = parse_time("2026-11-16T10:00:00").unwrap();
        assert_eq!(short_date(&local), "Nov 16");
        assert!(parse_time("yesterday").is_none());
    }

    #[test]
    fn kinds_group_the_suggested_first_in_catalog_order() {
        let (own, other) = group_kinds(&[TeachKind::Email, TeachKind::CodingAgent]);
        assert_eq!(own, [TeachKind::CodingAgent, TeachKind::Email]);
        assert_eq!(other.len(), 6);
        assert!(!other.contains(&TeachKind::Email));
        let (own, other) = group_kinds(&[]);
        assert!(own.is_empty());
        assert_eq!(other, TEACH_KINDS);
    }

    #[test]
    fn chips_collapse_past_the_limit() {
        let chips: Vec<_> = (0..5)
            .map(|i| TeachChip {
                code: format!("c{i}"),
                value: None,
            })
            .collect();
        assert_eq!(visible_chips(&chips, 3, false), (&chips[..3], 2));
        assert_eq!(visible_chips(&chips, 3, true), (&chips[..], 0));
        assert_eq!(visible_chips(&chips[..3], 3, false), (&chips[..3], 0));
        assert_eq!(chip_key("line_per_thought"), "line-per-thought");
    }

    #[test]
    fn every_kind_and_habit_has_a_message() {
        for kind in TEACH_KINDS {
            let key = kind_key(kind);
            assert!(kass_i18n::has(&format!(
                "writing-style-teach-kinds-{key}-label"
            )));
            assert!(kass_i18n::has(&format!(
                "writing-style-teach-kinds-{key}-short"
            )));
        }
        for habit in [
            WritingStyleHabit::BoundaryPeriod,
            WritingStyleHabit::BoundaryComma,
            WritingStyleHabit::BoundaryNone,
            WritingStyleHabit::LowercaseStart,
            WritingStyleHabit::DropIntroComma,
            WritingStyleHabit::DropConjunctionComma,
            WritingStyleHabit::DropFinalPeriod,
        ] {
            let key = habit_key(habit).unwrap();
            assert!(kass_i18n::has(&format!("writing-style-habits-{key}")));
            assert!(kass_i18n::has(&format!("writing-style-habit-chips-{key}")));
        }
        assert_eq!(habit_key(WritingStyleHabit::Unknown), None);
    }
    #[test]
    fn moving_an_app_asks_only_when_corrections_would_move() {
        let mut slack = app("com.slack", Some("Slack"), "chat", 4, true);
        assert_eq!(plan_move(&slack, "chat"), MovePlan::Stay);
        assert_eq!(plan_move(&slack, "work"), MovePlan::Assign);
        slack.corrections = 2;
        assert_eq!(plan_move(&slack, "work"), MovePlan::Ask);
        assert_eq!(plan_move(&slack, "chat"), MovePlan::Stay);
        slack.confirmed = false;
        assert_eq!(plan_move(&slack, "chat"), MovePlan::Assign);
    }

    #[test]
    fn moves_and_confirms_show_before_the_server_answers() {
        let mut data = data();
        moved(&mut data, "com.zed", "chat");
        let zed = data.apps.iter().find(|a| a.bundle_id == "com.zed").unwrap();
        assert_eq!((zed.style_id.as_str(), zed.confirmed), ("chat", true));
        confirmed(&mut data);
        assert!(data.apps.iter().all(|a| a.confirmed));
    }

    #[test]
    fn a_new_app_shows_its_suggested_style() {
        let data = data();
        let mut zed = app("com.zed", Some("Zed"), "work", 1, false);
        assert_eq!(
            suggested_style(&data, &zed).map(|s| s.id.as_str()),
            Some("work")
        );
        zed.suggested_style_id = Some("chat".into());
        assert_eq!(
            suggested_style(&data, &zed).map(|s| s.id.as_str()),
            Some("chat")
        );
        zed.suggested_style_id = Some("gone".into());
        zed.style_id = "gone".into();
        assert_eq!(
            suggested_style(&data, &zed).map(|s| s.id.as_str()),
            Some("chat")
        );
    }

    #[test]
    fn text_is_clipped_by_characters() {
        assert_eq!(clipped("héllo", 5), None);
        assert_eq!(clipped("héllo", 2).as_deref(), Some("hé"));
        assert_eq!(clipped("", 0), None);
    }
}
