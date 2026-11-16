//! Realistic server bodies (shapes from backend/models.py) parse.

mod common;

use kass_api::types::{
    CaptureListResponse, CaptureSettings, CaptureSource, CorrectionLearningStatus,
    DictionaryListResponse, DictionaryScope, DictionarySource, HistoryRetentionDays,
    LearningOutcome, ModelProgress, ProgressStatus, PunctuationStyle, Qwen3ModelSize,
    ReviewOutcome, ReviewReason, SoundsState, SyncStatus, TeachKind, TeachSession, TeachTrick,
    UsageBucket, UsagePeriod, UsageStatsResponse, WhisperModelSize, WritingStyleHabit,
    WritingStylesResponse,
};

fn parse<T: serde::de::DeserializeOwned>(json: &str) -> T {
    serde_json::from_str(json).unwrap()
}

#[test]
fn a_page_of_captures() {
    let review = r#"{"id":"c-2","audio_path":"/x/c-2.wav","source":"file","transcript_raw":"call 555 0100",
        "transcript_refined":"Call 555-0101.","refinement_review":{"outcome":"review","added":["0101"],"missing":["0100"],"reasons":["number","sarcasm"]},
        "created_at":"2026-11-12T18:00:00"}"#;
    let page: CaptureListResponse = parse(&format!(
        r#"{{"items":[{},{review}],"total":2}}"#,
        common::CAPTURE
    ));
    let [first, second] = &page.items[..] else {
        panic!()
    };
    assert_eq!(first.source, CaptureSource::Dictation);
    assert_eq!(
        first.refinement_flags.as_ref().unwrap().punctuation_style,
        Some(PunctuationStyle::Learned)
    );
    assert_eq!(first.duration_ms, Some(4210));
    assert_eq!(second.language, None);
    assert!(!second.audio_deleted);
    let review = second.refinement_review.as_ref().unwrap();
    assert_eq!(review.outcome, ReviewOutcome::Review);
    assert_eq!(
        review.reasons,
        [ReviewReason::Number, ReviewReason::Unknown]
    );
}

#[test]
fn capture_settings() {
    let settings: CaptureSettings = parse(include_str!("fixtures/capture_settings.json"));
    assert_eq!(settings.stt_model, WhisperModelSize::Turbo);
    assert_eq!(settings.llm_model, Qwen3ModelSize::Size0_6B);
    assert_eq!(
        settings.history_retention_days,
        HistoryRetentionDays::Days30
    );
    assert_eq!(settings.command_transforms[1].id, None);
    assert_eq!(settings.input_device_id, None);
}

#[test]
fn usage_stats() {
    let stats: UsageStatsResponse = parse(
        r#"{
        "period": "7d", "bucket": "day",
        "start": "2026-11-07", "end": "2026-11-13",
        "previous_start": "2026-10-31", "previous_end": "2026-11-06",
        "typing_wpm": 40,
        "current": {"words": 5120, "captures": 88, "speaking_ms": 2160000, "pace_wpm": 142,
                    "time_saved_ms": 5520000, "fixed_captures": 3, "weekdays_dictated": 5,
                    "weekdays_in_period": 5, "weekend_days": [{"date": "2026-11-08", "words": 120}],
                    "hours_dictated": 31},
        "previous": null,
        "all_apps": {"words": 5120, "captures": 88, "speaking_ms": 2160000},
        "series": [{"start": "2026-11-07", "words": 700, "previous_start": "2026-10-31", "previous_words": 512},
                   {"start": "2026-11-13", "words": null}],
        "apps": [{"app_bundle_id": "com.tinyspeck.slackmacgap", "words": 3000, "captures": 50},
                 {"app_bundle_id": null, "words": 20, "captures": 1}],
        "heatmap": [[0, 0, 12], [5, 0, 0]],
        "lengths": {"bin_edges_s": [0, 5, 15, 30, 60], "counts": [10, 40, 30, 8], "p25_ms": 4100, "median_ms": 9000, "p75_ms": null}
    }"#,
    );
    assert_eq!(
        (stats.period, stats.bucket),
        (UsagePeriod::Days7, UsageBucket::Day)
    );
    assert_eq!(stats.current.pace_wpm, Some(142));
    assert_eq!(stats.all_apps.time_saved_ms, 0);
    assert_eq!(stats.series[1].words, None);
    assert_eq!(stats.apps[1].app_bundle_id, None);
    assert_eq!(stats.lengths.median_ms, Some(9000));
}

#[test]
fn writing_styles() {
    let styles: WritingStylesResponse = parse(
        r#"{
        "styles": [
            {"id": "s-1", "name": "Default", "position": 0, "is_default": true, "punctuation_style": "learned", "preserve_technical": true, "description": ""},
            {"id": "s-2", "name": "Work", "position": 1, "is_default": false, "punctuation_style": "formal", "preserve_technical": false}
        ],
        "apps": [{"bundle_id": "com.apple.mail", "name": "Mail", "style_id": "s-2", "confirmed": false, "count": 14, "corrections": 2, "suggested_style_id": "s-2"},
                 {"bundle_id": "com.example.none", "name": null, "style_id": "s-1", "confirmed": true}],
        "max_styles": 5,
        "cache_mb_per_style": 48
    }"#,
    );
    assert_eq!(
        styles.styles[1].punctuation_style,
        PunctuationStyle::Unknown
    );
    assert_eq!(styles.styles[1].description, "");
    assert_eq!(styles.apps[1].count, 0);
    assert_eq!(styles.cache_mb_per_style, Some(48));
    let habits: Vec<WritingStyleHabit> = parse(r#"["lowercase_start","em_dash"]"#);
    assert_eq!(
        habits,
        [
            WritingStyleHabit::LowercaseStart,
            WritingStyleHabit::Unknown
        ]
    );
}

#[test]
fn a_teach_session() {
    let session: TeachSession = parse(
        r#"{
        "session_id": "t-1", "style_id": "s-1", "target": 6, "replies": 1,
        "suggested_kinds": ["team_chat", "email", "voice_memo"],
        "conversations": [{
            "id": "c-1", "kind": "team_chat", "persona": "Sam", "relation": "teammate",
            "title": null,
            "messages": [{"from_you": false, "text": "Did the build go out?"}, {"from_you": true, "text": "yep, just now"}],
            "note": {"facts": "It shipped at 3pm", "answers": [{"ask": "When?", "answer": "3pm"}], "trick": "change_of_mind", "example": null},
            "reply_count": 1,
            "chips": [{"code": "lowercase_start"}, {"code": "length", "value": "short"}],
            "wrapped": false
        }]
    }"#,
    );
    assert_eq!(
        session.suggested_kinds,
        [TeachKind::TeamChat, TeachKind::Email, TeachKind::Unknown]
    );
    let conversation = &session.conversations[0];
    assert!(conversation.messages[1].from_you);
    assert_eq!(
        conversation.note.as_ref().unwrap().trick,
        Some(TeachTrick::ChangeOfMind)
    );
    assert_eq!(conversation.chips[1].value.as_deref(), Some("short"));
}

#[test]
fn the_dictionary() {
    let list: DictionaryListResponse = parse(
        r#"{"entries": [
        {"id": "d-1", "written": "Kass", "spoken": "cass", "places": [{"scope": "global", "scope_id": null, "app_name": null}],
         "created_at": "2026-11-01T10:00:00", "match_sound": false, "source": "voice_edit", "phrase": false},
        {"id": "d-2", "written": "GPUI", "places": [{"scope": "app", "scope_id": "com.microsoft.VSCode", "app_name": "Code"}, {"scope": "style", "scope_id": "s-2"}],
         "source": "teach"}
    ]}"#,
    );
    assert!(!list.entries[0].match_sound);
    assert!(list.entries[1].match_sound);
    assert_eq!(list.entries[1].places[0].scope, DictionaryScope::App);
    assert_eq!(list.entries[1].source, DictionarySource::Unknown);
}

#[test]
fn learning_status_from_a_dict() {
    let status: CorrectionLearningStatus = parse(
        r#"{
        "model": {"phase": "idle", "revision": 3, "running": false, "active_adapter": "a-3", "can_rollback": true,
                  "counts": {"pending": 2, "used": 40}, "last_run": "2026-11-13T02:00:00", "error": null,
                  "metrics": {"adapter": {"passed": true, "reasons": [], "baseline_errors": 0.12, "candidate_errors": 0.09}},
                  "voice": {"active": null, "can_undo": false, "min_train": 40, "min_test": 10,
                            "bank": {"takes": 12, "train": 9, "test": 3, "minutes": 4.5, "room_minutes": 1.0},
                            "sounds": {"state": "downloading", "fraction": 0.4, "error": null},
                            "metrics": null, "training": {"updates": 120, "seconds": 64.2, "train_takes": 9},
                            "something_new": 1}},
        "evaluated_report_ids": ["r-1"],
        "revision": 7, "active_rules": 4, "last_run": null, "outcome": "no_change", "can_rollback": false,
        "metrics": {"examples": 20, "candidates": 6, "accepted": 4, "withdrawn": 0, "median_rule_ms": 0.3, "latency_passed": true}
    }"#,
    );
    assert_eq!(status.outcome, LearningOutcome::NoChange);
    let model = status.model.unwrap();
    assert_eq!(model.counts["used"], 40);
    let voice = model.voice.unwrap();
    assert_eq!(voice.sounds.state, SoundsState::Downloading);
    assert_eq!(voice.bank.takes, 12);
    assert_eq!(
        parse::<CorrectionLearningStatus>("{}").outcome,
        LearningOutcome::Waiting
    );
}

#[test]
fn progress_and_sync() {
    let progress: ModelProgress = parse(
        r#"{"model_name":"whisper-turbo","current":1048576,"total":1610612736,"progress":0.07,"filename":"model.safetensors","status":"downloading","timestamp":"2026-11-13T10:00:00"}"#,
    );
    assert_eq!(progress.status, ProgressStatus::Downloading);
    assert!(!progress.is_finished());
    let done: ModelProgress =
        parse(r#"{"model_name":"m","current":1,"total":1,"progress":1.0,"status":"complete"}"#);
    assert!(done.is_finished());

    assert_eq!(
        parse::<SyncStatus>(r#"{"standing_in":false}"#),
        SyncStatus::default()
    );
    let standing_in: SyncStatus =
        parse(r#"{"standing_in":true,"pending":3,"downloading":["whisper-turbo"]}"#);
    assert_eq!(standing_in.pending, Some(3));
}
