//! Calls from the pages: Tauri's plugin commands the screens use (events,
//! dialogs, files, the shell, window dragging) are answered here, and the
//! app's own commands go to [`crate::commands`].

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use base64::Engine as _;
use gpui_kit::PathPromptOptions;
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSWindow};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{async_runtime, emit_value, main_async, open_external, webview, with_app};

/// What a command answers: a value, or an error the page's promise rejects
/// with.
pub type Reply = std::result::Result<Value, Value>;

/// A command's answer, now or once its work is done.
pub enum Answer {
    Now(Reply),
    Later(Pin<Box<dyn Future<Output = Reply> + Send>>),
}

impl Answer {
    pub fn later<F: Future<Output = Reply> + Send + 'static>(future: F) -> Self {
        Self::Later(Box::pin(future))
    }
}

/// The page script for the window `label`.
pub fn bridge_js(label: &str) -> String {
    let label = serde_json::to_string(label).unwrap_or_else(|_| "\"\"".into());
    include_str!("bridge.js").replace("__LABEL__", &label)
}

#[derive(Deserialize)]
struct Message {
    id: u64,
    cmd: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    headers: HashMap<String, String>,
}

/// Answer one call from the page in `label`. Runs on the main thread.
pub fn handle(label: &'static str, body: &str) {
    let message = match serde_json::from_str::<Message>(body) {
        Ok(message) => message,
        Err(e) => {
            eprintln!("[host] unreadable call from {label}: {e}");
            return;
        }
    };
    let id = message.id;
    let answer = match message.cmd.strip_prefix("plugin:") {
        Some(command) => plugin(label, command, message.args, &message.headers),
        None => crate::commands::dispatch(label, &message.cmd, message.args),
    };
    match answer {
        Answer::Now(reply) => main_async(move || reply_to(label, id, reply)),
        Answer::Later(work) => {
            async_runtime::spawn(async move {
                let reply = work.await;
                main_async(move || reply_to(label, id, reply));
            });
        }
    }
}

fn reply_to(label: &str, id: u64, reply: Reply) {
    let (ok, value) = match reply {
        Ok(value) => (true, value),
        Err(value) => (false, value),
    };
    webview::resolve(label, id, ok, &value);
}

fn fail(message: impl ToString) -> Value {
    Value::String(message.to_string())
}

fn plugin(label: &str, command: &str, args: Value, headers: &HashMap<String, String>) -> Answer {
    match command {
        "app|version" => Answer::Now(Ok(json!(env!("CARGO_PKG_VERSION")))),
        "app|name" => Answer::Now(Ok(json!("Kass"))),
        "event|emit" | "event|emit_to" => Answer::Now(emit(&args)),
        "dialog|open" => pick_folder(&args["options"]),
        "dialog|save" => pick_save_path(&args["options"]),
        "fs|write_file" => Answer::Now(write_file(&args, headers)),
        "shell|open" => Answer::Now(match args["path"].as_str() {
            Some(path) => open_external(path).map(|_| Value::Null).map_err(fail),
            None => Err(fail("Nothing to open")),
        }),
        // Now, while the mouse-down that asked is still the current event.
        "window|start_dragging" => {
            webview::with_window(label, start_dragging);
            Answer::Now(Ok(Value::Null))
        }
        "window|internal_toggle_maximize" | "window|toggle_maximize" => {
            webview::with_window(label, |window| window.zoom(None));
            Answer::Now(Ok(Value::Null))
        }
        _ => Answer::Now(Err(fail(format!("plugin:{command} is not available")))),
    }
}

fn start_dragging(window: &NSWindow) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let event: Option<Retained<NSEvent>> = NSApplication::sharedApplication(mtm).currentEvent();
    if let Some(event) = event {
        window.performWindowDragWithEvent(&event);
    }
}

/// `emit` and `emit_to`: the target is a window label, or Tauri's
/// `{ kind, label }`, where `Any` means every window.
fn emit(args: &Value) -> Reply {
    let event = args["event"]
        .as_str()
        .ok_or_else(|| fail("An event needs a name"))?;
    let payload = args.get("payload").cloned().unwrap_or(Value::Null);
    emit_value(event, payload, target_label(&args["target"]));
    Ok(Value::Null)
}

fn target_label(target: &Value) -> Option<String> {
    match target {
        Value::String(label) => Some(label.clone()),
        Value::Object(target) if target.get("kind").and_then(Value::as_str) != Some("Any") => {
            target
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        _ => None,
    }
}

/// `dialog.open`; the screens only ever pick one folder.
fn pick_folder(options: &Value) -> Answer {
    let directory = options["directory"].as_bool().unwrap_or(false);
    let multiple = options["multiple"].as_bool().unwrap_or(false);
    let (tx, rx) = tokio::sync::oneshot::channel();
    with_app(move |cx| {
        let _ = tx.send(cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple,
            prompt: None,
        }));
    });
    Answer::later(async move {
        let picked = rx.await.map_err(fail)?.await.map_err(fail)?.map_err(fail)?;
        let Some(paths) = picked else {
            return Ok(Value::Null);
        };
        let paths: Vec<String> = paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        Ok(if multiple {
            json!(paths)
        } else {
            paths.into_iter().next().map_or(Value::Null, Value::String)
        })
    })
}

/// Where `dialog.save` starts and the name it suggests. A bare file name
/// starts in Downloads.
fn save_start(default_path: Option<&str>) -> (PathBuf, Option<String>) {
    let downloads = || {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Downloads"))
            .unwrap_or_else(|| PathBuf::from("/"))
    };
    let Some(path) = default_path.filter(|p| !p.is_empty()).map(Path::new) else {
        return (downloads(), None);
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    match path.parent().filter(|dir| dir.is_absolute()) {
        Some(dir) => (dir.to_path_buf(), name),
        None => (downloads(), name),
    }
}

fn pick_save_path(options: &Value) -> Answer {
    let (dir, name) = save_start(options["defaultPath"].as_str());
    let (tx, rx) = tokio::sync::oneshot::channel();
    with_app(move |cx| {
        let _ = tx.send(cx.prompt_for_new_path(&dir, name.as_deref()));
    });
    Answer::later(async move {
        let picked = rx.await.map_err(fail)?.await.map_err(fail)?.map_err(fail)?;
        Ok(picked.map_or(Value::Null, |path| {
            Value::String(path.to_string_lossy().into_owned())
        }))
    })
}

/// `fs.writeFile`: the bytes come as base64, the path in a header.
fn write_file(args: &Value, headers: &HashMap<String, String>) -> Reply {
    let path = headers
        .get("path")
        .map(|path| percent_decode(path))
        .ok_or_else(|| fail("No path to write to"))?;
    let bytes = args["__bytes"]
        .as_str()
        .ok_or_else(|| fail("Nothing to write"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(bytes)
        .map_err(fail)?;
    std::fs::write(&path, bytes).map_err(fail)?;
    Ok(Value::Null)
}

/// Undo `encodeURIComponent`.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_encoded_paths() {
        assert_eq!(
            percent_decode("%2FUsers%2Fme%2Fcapture%20corrections.json"),
            "/Users/me/capture corrections.json"
        );
        assert_eq!(percent_decode("caf%C3%A9%"), "café%");
        assert_eq!(percent_decode("100%25"), "100%");
    }

    #[test]
    fn reads_event_targets() {
        assert_eq!(target_label(&json!(null)), None);
        assert_eq!(target_label(&json!("main")), Some("main".into()));
        assert_eq!(target_label(&json!({ "kind": "Any" })), None);
        assert_eq!(
            target_label(&json!({ "kind": "AnyLabel", "label": "dictate" })),
            Some("dictate".into())
        );
    }

    #[test]
    fn save_starts_in_downloads_for_a_bare_name() {
        let (dir, name) = save_start(Some("capture-corrections.json"));
        assert!(dir.ends_with("Downloads"));
        assert_eq!(name.as_deref(), Some("capture-corrections.json"));
        let (dir, name) = save_start(Some("/tmp/out.json"));
        assert_eq!(dir, PathBuf::from("/tmp"));
        assert_eq!(name.as_deref(), Some("out.json"));
    }

    #[test]
    fn bridge_names_its_window() {
        let js = bridge_js("main");
        assert!(js.contains("const LABEL = \"main\";"));
        assert!(!js.contains("__LABEL__"));
    }
}
