//! The microphone outside a take: its macOS permission, and a level preview
//! for onboarding's microphone step (docs/plans/ONBOARDING.md).
//!
//! The preview opens the device the way a take does ([`capture::spawn`]) but
//! throws the audio away and only sends `mic:level` events. A take starting
//! stops it, and it can't start while a take records.

use std::time::Instant;

use crate::host::{AppHandle, Emitter, Manager};

use super::capture::{self, CaptureHooks};
use super::stream::AudioMsg;
use super::DictationState;

/// The microphone permission: `granted`, `denied`, `restricted` or
/// `undetermined` (macOS hasn't asked yet).
pub fn microphone_permission() -> &'static str {
    permission_name(authorization_status())
}

/// `AVAuthorizationStatus` by name.
fn permission_name(status: i64) -> &'static str {
    match status {
        1 => "restricted",
        2 => "denied",
        3 => "granted",
        _ => "undetermined",
    }
}

#[cfg(target_os = "macos")]
fn authorization_status() -> i64 {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *mut Object;
    }

    let Some(class) = Class::get("AVCaptureDevice") else {
        return 0;
    };
    // SAFETY: a class method taking the framework's own media type constant.
    unsafe { msg_send![class, authorizationStatusForMediaType: AVMediaTypeAudio] }
}

#[cfg(not(target_os = "macos"))]
fn authorization_status() -> i64 {
    3
}

/// Open the microphone and send its loudness as `mic:level { db }` about
/// every 50 ms, or `mic:error { message }` when it can't be opened. On
/// macOS this is what asks for the permission when it is undetermined.
/// Replaces a preview already running; refused while a take records.
pub fn mic_preview_start(app: AppHandle, device_id: Option<String>) -> Result<(), String> {
    let state = app.state::<DictationState>();
    let active = state.active.lock().map_err(|e| e.to_string())?;
    if active.is_recording() {
        return Err("The microphone is recording a dictation".into());
    }
    let mut preview = state.preview.lock().map_err(|e| e.to_string())?;
    if let Some(stop) = preview.take() {
        let _ = stop.send(());
    }
    let level_app = app.clone();
    let error_app = app.clone();
    let hooks = CaptureHooks {
        on_heard: Box::new(|| {}),
        on_level: Box::new(move |db| {
            let _ = level_app.emit("mic:level", serde_json::json!({ "db": db }));
        }),
        on_stopped: Box::new(|_| {}),
        on_error: Box::new(move |message| {
            let _ = error_app.emit("mic:error", serde_json::json!({ "message": message }));
        }),
    };
    // Nothing reads the audio: sending to a closed channel is a no-op.
    let (audio_tx, _) = tokio::sync::mpsc::unbounded_channel::<AudioMsg>();
    let device_id = device_id.filter(|id| !id.is_empty());
    let handle = capture::spawn(device_id, Instant::now(), audio_tx, hooks);
    *preview = Some(handle.stopper());
    Ok(())
}

/// Close the microphone the preview opened. Nothing to do when none runs.
pub fn mic_preview_stop(app: AppHandle) {
    stop_preview(&app.state::<DictationState>());
}

/// Stop the preview, if one runs.
pub fn stop_preview(state: &DictationState) {
    let stop = state.preview.lock().ok().and_then(|mut p| p.take());
    if let Some(stop) = stop {
        let _ = stop.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_statuses_have_the_webview_names() {
        assert_eq!(permission_name(0), "undetermined");
        assert_eq!(permission_name(1), "restricted");
        assert_eq!(permission_name(2), "denied");
        assert_eq!(permission_name(3), "granted");
    }
}
