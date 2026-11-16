//! The app's commands, by the names the screens `invoke`. Arguments come
//! in camelCase, as Tauri passed them. Commands that were `async` answer
//! once their work is done; the rest answer on the main thread, as Tauri
//! ran them.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::host::ipc::{Answer, Reply};
use crate::host::{AppHandle, Manager};
use crate::{
    deep_link, dictation, hotkey_monitor, login_item, report, sound_cues, this_mac, updater,
    HotkeyState, ServerState,
};

/// The argument `key`; a missing one reads as `null`, as Tauri's did.
fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, Value> {
    let value = args.get(key).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value)
        .map_err(|e| Value::String(format!("invalid args `{key}` for command: {e}")))
}

fn json<T: Serialize>(value: T) -> Reply {
    serde_json::to_value(value).map_err(|e| Value::String(e.to_string()))
}

/// A command's `Result`, as the page's promise sees it.
fn result<T: Serialize>(value: Result<T, String>) -> Reply {
    value.map_err(Value::String).and_then(json)
}

/// Answer now with what `f` returns, or with a bad argument.
fn now(f: impl FnOnce() -> Reply) -> Answer {
    Answer::Now(f())
}

pub fn dispatch(_label: &str, cmd: &str, args: Value) -> Answer {
    let app = AppHandle;
    let a = &args;
    match cmd {
        "dictate_chip_space" => now(|| {
            crate::dictate_chip_space(app, arg(a, "show")?);
            Ok(Value::Null)
        }),
        "shell_location" => now(|| {
            crate::shell::set_location(crate::shell::bridge::Location {
                path: arg(a, "path")?,
                search: arg::<Option<String>>(a, "search")?.unwrap_or_default(),
            });
            Ok(Value::Null)
        }),
        "prefs_adopt" => now(|| Ok(crate::prefs::adopt(arg(a, "prefs")?))),
        "start_server" | "restart_server" => {
            let restart = cmd == "restart_server";
            match arg(a, "modelsDir") {
                Err(e) => Answer::Now(Err(e)),
                Ok(models_dir) => Answer::later(async move {
                    let state = app.state::<ServerState>();
                    result(if restart {
                        crate::restart_server(app, state, models_dir).await
                    } else {
                        crate::start_server(app, state, models_dir).await
                    })
                }),
            }
        }
        "stop_server" => {
            Answer::later(
                async move { result(crate::stop_server(app.state::<ServerState>()).await) },
            )
        }
        "network_address" => now(|| result(crate::network_address())),
        "this_mac" => now(|| json(this_mac::this_mac())),
        "restart_app" => {
            Answer::later(
                async move { result(crate::restart_app(app.state::<ServerState>()).await) },
            )
        }
        "app_location" => now(|| json(crate::app_location())),
        "move_to_applications" => Answer::later(async move {
            result(crate::move_to_applications(app.state::<ServerState>()).await)
        }),
        "debug_clipboard_roundtrip" => match (arg::<String>(a, "text"), arg::<u64>(a, "holdMs")) {
            (Ok(text), Ok(hold_ms)) => Answer::later(async move {
                result(crate::debug_clipboard_roundtrip(text, hold_ms).await)
            }),
            (Err(e), _) | (_, Err(e)) => Answer::Now(Err(e)),
        },
        "debug_paste_text" => {
            let parsed = (|| {
                Ok((
                    arg(a, "text")?,
                    arg(a, "prePasteDelayMs")?,
                    arg(a, "postPasteDelayMs")?,
                ))
            })();
            match parsed {
                Err(e) => Answer::Now(Err(e)),
                Ok((text, pre, post)) => {
                    Answer::later(
                        async move { result(crate::debug_paste_text(text, pre, post).await) },
                    )
                }
            }
        }
        "debug_capture_focus" => now(|| result(crate::debug_capture_focus())),
        "debug_focus_roundtrip" => {
            let parsed = (|| {
                Ok((
                    arg(a, "text")?,
                    arg(a, "driftMs")?,
                    arg(a, "postPasteDelayMs")?,
                ))
            })();
            match parsed {
                Err(e) => Answer::Now(Err(e)),
                Ok((text, drift, post)) => Answer::later(async move {
                    result(crate::debug_focus_roundtrip(text, drift, post).await)
                }),
            }
        }
        "app_icon" => now(|| json(crate::app_icon(arg(a, "bundleId")?))),
        "check_accessibility_permission" => now(|| json(crate::check_accessibility_permission())),
        "check_input_monitoring_permission" => {
            now(|| json(crate::check_input_monitoring_permission()))
        }
        "open_accessibility_settings" => now(|| result(crate::open_accessibility_settings(app))),
        "open_input_monitoring_settings" => now(|| result(crate::open_input_monitoring_settings())),
        "paste_final_text" => match (arg::<String>(a, "text"), arg(a, "focus")) {
            (Ok(text), Ok(focus)) => {
                Answer::later(async move { result(crate::paste_final_text(text, focus).await) })
            }
            (Err(e), _) | (_, Err(e)) => Answer::Now(Err(e)),
        },
        "enable_hotkey" => now(|| {
            result(crate::enable_hotkey(
                AppHandle,
                app.state::<HotkeyState>(),
                arg(a, "pushToTalk")?,
                arg(a, "toggleToTalk")?,
                arg(a, "command")?,
            ))
        }),
        "disable_hotkey" => now(|| result(crate::disable_hotkey(app.state::<HotkeyState>()))),
        "update_chord_bindings" => now(|| {
            result(crate::update_chord_bindings(
                app.state::<HotkeyState>(),
                arg(a, "pushToTalk")?,
                arg(a, "toggleToTalk")?,
                arg(a, "command")?,
            ))
        }),
        "set_chord_practice" => now(|| {
            hotkey_monitor::set_chord_practice(
                app.state::<hotkey_monitor::ChordMode>(),
                arg(a, "enabled")?,
            );
            Ok(Value::Null)
        }),
        "set_dictation_gate" => now(|| {
            hotkey_monitor::set_dictation_gate(
                app.state::<hotkey_monitor::ChordMode>(),
                arg(a, "blocked")?,
            );
            Ok(Value::Null)
        }),
        "open_onboarding" => now(|| {
            let resume: Option<bool> = arg(a, "resume")?;
            result(crate::open_onboarding(app, resume.unwrap_or(false)))
        }),
        "finish_onboarding" => now(|| {
            crate::finish_onboarding(app, arg(a, "show")?);
            Ok(Value::Null)
        }),
        "onboarding_window_open" => now(|| json(crate::onboarding_window_open(app))),
        "microphone_permission" => now(|| json(dictation::mic::microphone_permission())),
        "mic_preview_start" => {
            now(|| result(dictation::mic::mic_preview_start(app, arg(a, "deviceId")?)))
        }
        "mic_preview_stop" => now(|| {
            dictation::mic::mic_preview_stop(app);
            Ok(Value::Null)
        }),
        "dictation_configure" => now(|| {
            result(dictation::dictation_configure(
                app.state::<dictation::DictationState>(),
                arg(a, "serverUrl")?,
                arg(a, "fallbackUrl")?,
                arg(a, "origin")?,
                arg(a, "inputDeviceId")?,
                arg(a, "deviceKnown")?,
                arg(a, "liveText")?,
                arg(a, "voiceEdits")?,
            ))
        }),
        "dictation_set_away" => now(|| {
            dictation::dictation_set_away(
                app.state::<dictation::DictationState>(),
                arg(a, "away")?,
            );
            Ok(Value::Null)
        }),
        "dictation_start" => now(|| json(dictation::dictation_start(app))),
        "dictation_stop" => now(|| {
            dictation::dictation_stop(app);
            Ok(Value::Null)
        }),
        "command_run" => match arg::<String>(a, "instruction") {
            Ok(instruction) => {
                Answer::later(async move { result(dictation::command_run(app, instruction).await) })
            }
            Err(e) => Answer::Now(Err(e)),
        },
        "apply_correction" => {
            let parsed = (|| Ok((arg(a, "captureId")?, arg(a, "before")?, arg(a, "after")?)))();
            match parsed {
                Err(e) => Answer::Now(Err(e)),
                Ok((capture_id, before, after)) => Answer::later(async move {
                    json(
                        dictation::corrections::apply_correction(app, capture_id, before, after)
                            .await,
                    )
                }),
            }
        }
        "list_input_devices" => {
            Answer::later(async move { result(dictation::list_input_devices().await) })
        }
        "configure_sound_cues" => now(|| {
            sound_cues::configure_sound_cues(arg(a, "enabled")?, arg(a, "volume")?);
            Ok(Value::Null)
        }),
        "preview_sound_cue" => now(|| {
            sound_cues::preview_sound_cue(arg(a, "volume")?);
            Ok(Value::Null)
        }),
        "launch_at_login_status" => now(|| json(login_item::launch_at_login_status())),
        "set_launch_at_login" => {
            now(|| result(login_item::set_launch_at_login(arg(a, "enabled")?)))
        }
        "open_login_items_settings" => now(|| result(login_item::open_login_items_settings())),
        "create_report" => Answer::later(async move { result(report::create_report(app).await) }),
        "reveal_report" => now(|| result(report::reveal_report(arg(a, "path")?))),
        "take_deep_link" => now(|| {
            json(deep_link::take_deep_link(
                app.state::<deep_link::DeepLinkState>(),
            ))
        }),
        "update_status" => {
            now(|| json(updater::update_status(app.state::<updater::UpdaterState>())))
        }
        "check_for_updates" => {
            Answer::later(async move { result(updater::check_for_updates(app).await) })
        }
        "update_channel" => now(|| json(updater::update_channel(app))),
        "set_update_channel" => {
            now(|| result(updater::set_update_channel(app, arg(a, "channel")?)))
        }
        "restart_to_update" => Answer::later(async move {
            result(updater::restart_to_update(app.state::<ServerState>()).await)
        }),
        _ => Answer::Now(Err(Value::String(format!("Command {cmd} not found")))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_arguments_like_tauri() {
        let args = json!({ "pushToTalk": ["fn"], "volume": 0.5 });
        assert_eq!(arg::<Vec<String>>(&args, "pushToTalk").unwrap(), vec!["fn"]);
        assert_eq!(arg::<Option<f32>>(&args, "volume").unwrap(), Some(0.5));
        assert_eq!(arg::<Option<String>>(&args, "missing").unwrap(), None);
        assert!(arg::<bool>(&args, "missing").is_err());
    }

    #[test]
    fn unknown_commands_fail() {
        let Answer::Now(Err(error)) = dispatch("main", "no_such_command", json!({})) else {
            panic!("expected an error");
        };
        assert_eq!(error, json!("Command no_such_command not found"));
    }
}
