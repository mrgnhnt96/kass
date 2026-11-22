//! `/captures/stream` protocol version 1 (docs/plans/STREAMING_DICTATION_PHASE_1.md).
//!
//! Pure encoding and decoding only, so the wire format is unit-tested without
//! a socket.

use serde_json::Value;

/// Bytes before the PCM payload: little-endian u32 sequence, then
/// little-endian u32 sample offset.
pub const HEADER_BYTES: usize = 8;
/// The whole binary message, header included, must not exceed this.
pub const MAX_MESSAGE_BYTES: usize = 65_536;
/// Largest PCM payload (in samples) that fits in one message.
pub const MAX_SAMPLES_PER_MESSAGE: usize = (MAX_MESSAGE_BYTES - HEADER_BYTES) / 2;

/// Encode one binary audio message.
pub fn encode_frame(sequence: u32, sample_offset: u32, pcm: &[i16]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_BYTES + pcm.len() * 2);
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&sample_offset.to_le_bytes());
    for sample in pcm {
        frame.extend_from_slice(&sample.to_le_bytes());
    }
    frame
}

/// The JSON start object sent right after the socket opens. `provisional`
/// asks for provisional cleaned text after release; `start_cue_ms`, when not
/// zero, says the take's first milliseconds may hold Kass's own start
/// cue, so a voice detected there alone doesn't make the take speech. Older
/// servers ignore both. A `command` take's words are an instruction for
/// selected text (docs/plans/COMMAND_MODE.md).
pub fn start_message(
    sample_rate: u32,
    provisional: bool,
    start_cue_ms: u32,
    command: bool,
) -> String {
    let mut start = serde_json::json!({
        "type": "start",
        "protocol_version": 1,
        "sample_rate": sample_rate,
        "channels": 1,
        "encoding": "pcm_s16le",
        "source": if command { "command" } else { "dictation" },
    });
    if provisional {
        start["provisional"] = Value::Bool(true);
    }
    if start_cue_ms > 0 {
        start["start_cue_ms"] = Value::from(start_cue_ms);
    }
    start.to_string()
}

/// The app a dictation went to, saved with its capture. `category` is its
/// App Store category (`LSApplicationCategoryType`), which the server uses to
/// suggest a writing style for a new app.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetApp {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    pub category: Option<String>,
}

fn app_fields(app: &TargetApp) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    fields.insert("bundle_id".into(), serde_json::json!(app.bundle_id));
    fields.insert("name".into(), serde_json::json!(app.name));
    if let Some(category) = &app.category {
        fields.insert("category".into(), Value::from(category.as_str()));
    }
    fields
}

/// The take's target app, from the focus snapshot taken at key-down. The
/// server picks the app's writing style from it before the first phrase is
/// cleaned (docs/plans/PER_APP_STYLE.md).
pub fn app_message(app: &TargetApp) -> String {
    let mut message = app_fields(app);
    message.insert("type".into(), Value::from("app"));
    Value::Object(message).to_string()
}

/// `app` is the dictation's target app, when known; older servers ignore it.
pub fn finish_message(app: Option<&TargetApp>) -> String {
    let mut finish = serde_json::json!({ "type": "finish" });
    if let Some(app) = app {
        finish["app"] = Value::Object(app_fields(app));
    }
    finish.to_string()
}

/// The field's text before the caret, read just after key-down, so the
/// server can tell whether the take continues its sentence
/// (docs/plans/MID_SENTENCE_DICTATION.md).
pub fn context_message(before: &str) -> String {
    serde_json::json!({ "type": "context", "before": before }).to_string()
}

/// The text before the caret, which a voice edit may change, and how many
/// of its last chars Kass's last take wrote, with the capture that wrote
/// them (docs/plans/VOICE_EDITS.md). Older servers reject unknown messages,
/// so it is only sent while voice edits are on.
pub fn last_take_message(text: &str, capture_id: Option<&str>, own_chars: usize) -> String {
    serde_json::json!({
        "type": "last_take",
        "text": text,
        "capture_id": capture_id,
        "own_chars": own_chars,
    })
    .to_string()
}

/// The text a command take rewrites, read just after key-down.
pub fn selection_message(text: &str) -> String {
    serde_json::json!({ "type": "selection", "text": text }).to_string()
}

pub fn cancel_message() -> String {
    r#"{"type":"cancel"}"#.to_string()
}

/// A server message, reduced to what the client acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    Ready {
        session_id: String,
    },
    /// The raw `final` event; validate it with [`is_valid_final`].
    Final(Value),
    /// Cleaned text the server expects to keep, sent after `finish`.
    Provisional {
        session_id: Option<String>,
        text: String,
    },
    Error(String),
    /// The user asked for a writing style at the start of the take ("use
    /// formal mode"): `to` replaces `from`, which is `None` when the take was
    /// already in that style.
    Style {
        session_id: Option<String>,
        from: Option<String>,
        to: String,
    },
    /// The take opens as a voice edit of the last one ("fix that, ..."),
    /// heard while the user still speaks.
    Edit {
        session_id: Option<String>,
    },
    /// `transcript` / `refined` updates and anything else informational.
    Update,
    /// Not JSON, or not an object.
    Invalid,
}

pub fn parse_server_event(text: &str) -> ServerEvent {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return ServerEvent::Invalid;
    };
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return if value.is_object() {
            ServerEvent::Update
        } else {
            ServerEvent::Invalid
        };
    };
    match kind {
        "ready" => match value.get("session_id").and_then(Value::as_str) {
            Some(id) => ServerEvent::Ready {
                session_id: id.to_string(),
            },
            None => ServerEvent::Invalid,
        },
        "final" => ServerEvent::Final(value),
        "provisional" => match value.get("text").and_then(Value::as_str) {
            Some(text) => ServerEvent::Provisional {
                session_id: value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: text.to_string(),
            },
            None => ServerEvent::Update,
        },
        "style" => match value.get("name").and_then(Value::as_str) {
            Some(name) => ServerEvent::Style {
                session_id: value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                from: value
                    .get("from_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                to: name.to_string(),
            },
            None => ServerEvent::Update,
        },
        "edit" => ServerEvent::Edit {
            session_id: value
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "error" => ServerEvent::Error(
            value
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .unwrap_or("Streaming finalization failed")
                .to_string(),
        ),
        _ => ServerEvent::Update,
    }
}

/// A `final` event is only trusted when it completes refinement for this
/// session and carries a transcript (possibly empty).
pub fn is_valid_final(event: &Value, session_id: &str) -> bool {
    event.get("type").and_then(Value::as_str) == Some("final")
        && event.get("refinement_complete") == Some(&Value::Bool(true))
        && event.pointer("/capture/id").and_then(Value::as_str) == Some(session_id)
        && event
            .pointer("/capture/transcript_raw")
            .map(Value::is_string)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_little_endian_header_then_pcm() {
        let frame = encode_frame(1, 2, &[1, -2]);
        assert_eq!(frame, vec![1, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0xFE, 0xFF]);
    }

    #[test]
    fn largest_payload_fits_the_message_limit() {
        let pcm = vec![0i16; MAX_SAMPLES_PER_MESSAGE];
        assert_eq!(encode_frame(0, 0, &pcm).len(), MAX_MESSAGE_BYTES);
    }

    #[test]
    fn start_message_matches_protocol_v1() {
        let value: Value = serde_json::from_str(&start_message(48_000, false, 0, false)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "start",
                "protocol_version": 1,
                "sample_rate": 48000,
                "channels": 1,
                "encoding": "pcm_s16le",
                "source": "dictation",
            })
        );
    }

    #[test]
    fn a_command_take_says_so_and_sends_its_selection() {
        let value: Value = serde_json::from_str(&start_message(48_000, false, 0, true)).unwrap();
        assert_eq!(value["source"], "command");
        let value: Value = serde_json::from_str(&selection_message("the \"text\"\n")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "type": "selection", "text": "the \"text\"\n" })
        );
    }

    #[test]
    fn app_message_names_the_target_app() {
        let app = TargetApp {
            bundle_id: Some("com.tinyspeck.slackmacgap".into()),
            name: Some("Slack".into()),
            category: Some("public.app-category.business".into()),
        };
        let value: Value = serde_json::from_str(&app_message(&app)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "app",
                "bundle_id": "com.tinyspeck.slackmacgap",
                "name": "Slack",
                "category": "public.app-category.business",
            })
        );
    }

    #[test]
    fn context_message_carries_the_text_before_the_caret() {
        let value: Value = serde_json::from_str(&context_message("we \"should\"\n")).unwrap();
        assert_eq!(value["type"], "context");
        assert_eq!(value["before"], "we \"should\"\n");
    }

    #[test]
    fn start_message_can_ask_for_provisional_text() {
        let value: Value = serde_json::from_str(&start_message(16_000, true, 0, false)).unwrap();
        assert_eq!(value["provisional"], Value::Bool(true));
        assert_eq!(value["protocol_version"], 1);
    }

    #[test]
    fn start_message_marks_the_start_cue_only_when_it_played() {
        let value: Value = serde_json::from_str(&start_message(48_000, false, 300, false)).unwrap();
        assert_eq!(value["start_cue_ms"], 300);
        let value: Value = serde_json::from_str(&start_message(48_000, false, 0, false)).unwrap();
        assert_eq!(value.get("start_cue_ms"), None);
    }

    #[test]
    fn finish_message_carries_the_target_app() {
        let plain: Value = serde_json::from_str(&finish_message(None)).unwrap();
        assert_eq!(plain, serde_json::json!({ "type": "finish" }));
        let app = TargetApp {
            bundle_id: Some("com.apple.mail".into()),
            name: Some("Mail".into()),
            category: None,
        };
        let value: Value = serde_json::from_str(&finish_message(Some(&app))).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "finish",
                "app": { "bundle_id": "com.apple.mail", "name": "Mail" },
            })
        );
    }

    #[test]
    fn parses_provisional_text() {
        assert_eq!(
            parse_server_event(r#"{"type":"provisional","session_id":"s1","text":"Hello there"}"#),
            ServerEvent::Provisional {
                session_id: Some("s1".into()),
                text: "Hello there".into()
            }
        );
        // Malformed provisional text is informational, never fatal.
        assert_eq!(
            parse_server_event(r#"{"type":"provisional"}"#),
            ServerEvent::Update
        );
    }

    #[test]
    fn parses_ready_error_and_updates() {
        assert_eq!(
            parse_server_event(r#"{"type":"ready","session_id":"s1","auto_refine":true}"#),
            ServerEvent::Ready {
                session_id: "s1".into()
            }
        );
        assert_eq!(
            parse_server_event(r#"{"type":"error","message":"boom"}"#),
            ServerEvent::Error("boom".into())
        );
        assert_eq!(
            parse_server_event(r#"{"type":"transcript","text":"hi"}"#),
            ServerEvent::Update
        );
        assert_eq!(parse_server_event("not json"), ServerEvent::Invalid);
        assert_eq!(
            parse_server_event(r#"{"type":"ready"}"#),
            ServerEvent::Invalid
        );
    }

    #[test]
    fn final_must_match_session_and_complete_refinement() {
        let good = serde_json::json!({
            "type": "final",
            "refinement_complete": true,
            "capture": {"id": "s1", "transcript_raw": ""}
        });
        assert!(is_valid_final(&good, "s1"));
        assert!(!is_valid_final(&good, "other"));
        let mut incomplete = good.clone();
        incomplete["refinement_complete"] = Value::Bool(false);
        assert!(!is_valid_final(&incomplete, "s1"));
        let mut no_text = good.clone();
        no_text["capture"]["transcript_raw"] = Value::Null;
        assert!(!is_valid_final(&no_text, "s1"));
    }
}
