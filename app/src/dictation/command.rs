//! Command Mode (docs/plans/COMMAND_MODE.md): the selection a command
//! rewrites, and commands run from Kass's own window.
//!
//! A command take is a dictation take whose words are an instruction. The
//! selection is read here, on a blocking thread right after key-down, while
//! the user speaks: Accessibility first, then ⌘C where Accessibility can't
//! tell. The rewrite replaces the selection through the insertion chain,
//! which already replaces a selection in every step.

use std::time::{Duration, Instant};

use crate::host::{AppHandle, Emitter, Manager};
use serde_json::Value;

use super::take::PillEvent;
use super::{http, DictationState};
use crate::focus_capture::FocusSnapshot;
use crate::text_insert::SelectionRead;

pub const NO_SELECTION_MESSAGE: &str = "Select text to rewrite first";
pub const NOT_EDITABLE_MESSAGE: &str = "This text can't be rewritten in place";
pub const IN_KASS_MESSAGE: &str = "Command Mode rewrites text in other apps";
pub const NO_TARGET_MESSAGE: &str = "No app to rewrite text in. Select text in an app first.";

/// Settle time after bringing an app forward before ⌘C, as for pastes.
const ACTIVATE_SETTLE: Duration = Duration::from_millis(120);

/// The selection to rewrite, from what Accessibility read and, where it
/// couldn't tell, a copy. `copy` runs only then.
pub fn decide(
    read: SelectionRead,
    copy: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<String, &'static str> {
    let text = match read {
        SelectionRead::Text(text) => text,
        SelectionRead::Empty => return Err(NO_SELECTION_MESSAGE),
        SelectionRead::NotEditable => return Err(NOT_EDITABLE_MESSAGE),
        SelectionRead::Unreadable => match copy() {
            Ok(Some(text)) => text,
            Ok(None) => return Err(NO_SELECTION_MESSAGE),
            Err(e) => {
                eprintln!("[command] copying the selection failed: {e}");
                return Err(NO_SELECTION_MESSAGE);
            }
        },
    };
    if text.trim().is_empty() {
        return Err(NO_SELECTION_MESSAGE);
    }
    Ok(text)
}

/// The selection a Kass window reported, or `None` when it didn't in
/// time. Blank is no selection.
pub fn decide_in_app(reply: Option<String>) -> Result<String, &'static str> {
    match reply {
        Some(text) if !text.trim().is_empty() => Ok(text),
        _ => Err(NO_SELECTION_MESSAGE),
    }
}

/// Read the selection in the app `focus` names. `in_front` is whether that
/// app is frontmost, which ⌘C needs; otherwise it is brought forward first.
/// Blocking.
pub fn read_selection(focus: &FocusSnapshot, in_front: bool) -> Result<String, &'static str> {
    if focus.bundle_id.as_deref() == Some(crate::KASS_BUNDLE_ID) {
        return Err(IN_KASS_MESSAGE);
    }
    let started = Instant::now();
    // An Electron app's tree builds lazily; asking now helps the insertion
    // even when this read comes too early for it.
    crate::text_insert::wake_electron(focus.pid);
    let read = crate::text_insert::selection_in_focused(focus.pid, focus.bundle_id.as_deref());
    let by_ax = !matches!(read, SelectionRead::Unreadable);
    let found = decide(read, || {
        if !in_front {
            crate::focus_capture::activate_pid(focus.pid)?;
            std::thread::sleep(ACTIVATE_SETTLE);
        }
        crate::clipboard::copy_selection()
    });
    eprintln!(
        "[command] selection via {} in {:.0}ms: {}",
        if by_ax { "Accessibility" } else { "⌘C" },
        started.elapsed().as_secs_f64() * 1000.0,
        match &found {
            Ok(text) => format!("{} chars", text.chars().count()),
            Err(message) => message.to_string(),
        }
    );
    found
}

/// Run `instruction` (or the transform it names) on the selection in the app
/// the user was in before Kass's window, for the ⌘K palette. Progress
/// and errors show in the pill.
pub async fn run_from_kass(app: &AppHandle, instruction: String) -> Result<(), String> {
    let state = app.state::<DictationState>();
    let take_id = state.next_take_id();
    let (server_url, http_client) = state.server();
    let emit = |event: PillEvent| emit_pill(app, take_id, event);
    let focus = crate::focus_capture::app_behind_kass().ok_or(NO_TARGET_MESSAGE)?;
    super::show_hud(app);
    emit(PillEvent::Refining);

    let reading = focus.clone();
    let selection =
        crate::host::async_runtime::spawn_blocking(move || read_selection(&reading, false))
            .await
            .map_err(|e| e.to_string())?;
    let selection = match selection {
        Ok(selection) => selection,
        Err(message) => {
            emit(PillEvent::notice(message));
            return Ok(());
        }
    };
    let capture = match http::run_command(
        &http_client,
        &server_url,
        &selection,
        http::CommandInput::Instruction {
            instruction: &instruction,
            bundle_id: focus.bundle_id.as_deref(),
            app_name: focus.app_name.as_deref(),
        },
    )
    .await
    {
        Ok(capture) => capture,
        Err(message) => {
            emit(PillEvent::error(message));
            return Ok(());
        }
    };
    let _ = app.emit("capture:created", serde_json::json!({ "capture": capture }));
    let text = capture
        .get("transcript_refined")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let pasted = crate::paste_final_text_with(text, focus, None).await;
    match super::delivery::paste_failure(pasted) {
        None => emit(PillEvent::Done),
        Some((message, accessibility)) => {
            if accessibility {
                let _ = app.emit("system:accessibility-missing", ());
            }
            emit(PillEvent::error(message));
        }
    }
    Ok(())
}

fn emit_pill(app: &AppHandle, take_id: u64, event: PillEvent) {
    let mut payload = serde_json::to_value(&event).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = payload {
        map.insert("take".into(), Value::from(take_id));
    }
    let _ = app.emit("dictation:state", payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_copy() -> Result<Option<String>, String> {
        panic!("copied although Accessibility could tell")
    }

    #[test]
    fn a_selection_accessibility_reads_is_used_as_is() {
        assert_eq!(
            decide(SelectionRead::Text(" two words ".into()), no_copy),
            Ok(" two words ".into())
        );
    }

    #[test]
    fn a_bare_caret_or_a_terminal_declines_without_copying() {
        assert_eq!(
            decide(SelectionRead::Empty, no_copy),
            Err(NO_SELECTION_MESSAGE)
        );
        assert_eq!(
            decide(SelectionRead::NotEditable, no_copy),
            Err(NOT_EDITABLE_MESSAGE)
        );
    }

    #[test]
    fn unreadable_selections_are_copied() {
        assert_eq!(
            decide(SelectionRead::Unreadable, || Ok(Some("copied".into()))),
            Ok("copied".into())
        );
        assert_eq!(
            decide(SelectionRead::Unreadable, || Ok(None)),
            Err(NO_SELECTION_MESSAGE)
        );
        assert_eq!(
            decide(SelectionRead::Unreadable, || Err("no pasteboard".into())),
            Err(NO_SELECTION_MESSAGE)
        );
    }

    #[test]
    fn a_kass_window_selection_is_used_as_is() {
        assert_eq!(
            decide_in_app(Some(" two words ".into())),
            Ok(" two words ".into())
        );
    }

    #[test]
    fn no_reply_or_a_blank_one_from_a_kass_window_is_no_selection() {
        assert_eq!(decide_in_app(None), Err(NO_SELECTION_MESSAGE));
        assert_eq!(decide_in_app(Some(" \n".into())), Err(NO_SELECTION_MESSAGE));
    }

    #[test]
    fn whitespace_is_no_selection() {
        assert_eq!(
            decide(SelectionRead::Text("\n  ".into()), no_copy),
            Err(NO_SELECTION_MESSAGE)
        );
    }
}
