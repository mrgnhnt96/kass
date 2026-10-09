//! A correction saved in Captures, made in the field Kass's last take went
//! to, where it is still as Kass left it (docs/plans/CORRECTIONS_IN_PLACE.md).
//! Silent unless it worked: a toast in Kass when it was written at once, a
//! notification when it waited for its app.
//!
//! Where Accessibility wrote the take, the fix is written the same way at
//! once, with the app behind Kass. Where keys or ⌘V put it in (Electron apps
//! such as Slack ignore Accessibility writes, and don't even say which field
//! is focused while behind), the fix is held until the app is in front
//! again, then typed over the words it changes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use super::last_take::{self, Corrected, Correction};

/// How often the frontmost app is checked while a correction is held.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);
/// Time for an app that just came to the front to restore its field.
const SETTLE: Duration = Duration::from_millis(300);
/// A held correction is dropped after this long.
const HOLD_FOR: Duration = Duration::from_secs(30 * 60);

static WATCHING: AtomicBool = AtomicBool::new(false);

/// A correction saved in Captures, from `before` (the text as shown) to
/// `after`. Returns the app's name when it was made at once; held or not
/// made, it returns nothing and says nothing.
#[tauri::command]
pub async fn apply_correction(
    app: AppHandle,
    capture_id: String,
    before: String,
    after: String,
) -> Option<String> {
    let corrected = tauri::async_runtime::spawn_blocking(move || {
        last_take::apply_correction(&capture_id, &before, &after, |pid, owned, b, a| {
            crate::text_insert::correct_focused(pid, owned, b, a, None)
        })
    })
    .await
    .ok()?;
    eprintln!("[kass] correction: {corrected:?}");
    match corrected {
        Corrected::Applied(pid) => Some(
            crate::focus_capture::app_identity(pid)
                .1
                .unwrap_or_default(),
        ),
        Corrected::Held => {
            watch(app);
            None
        }
        Corrected::Nothing => None,
    }
}

/// Until the held correction's app is in front, then make it there.
fn watch(app: AppHandle) {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let until = Instant::now() + HOLD_FOR;
        while let Some(held) = last_take::held() {
            if Instant::now() > until {
                last_take::drop_held();
                break;
            }
            std::thread::sleep(WATCH_INTERVAL);
            let pid = held.take.pid;
            if crate::focus_capture::frontmost_pid() != Some(pid) {
                continue;
            }
            std::thread::sleep(SETTLE);
            if crate::focus_capture::frontmost_pid() != Some(pid) {
                continue;
            }
            let Some(held) = last_take::take_held() else {
                break;
            };
            if type_correction(&held) {
                notify(&app, pid);
            }
        }
        WATCHING.store(false, Ordering::SeqCst);
        // One held while this was stopping.
        if last_take::held().is_some() {
            watch(app);
        }
    });
}

/// Make `held` in its app, now in front, by selecting the words it changes
/// and typing over them. Blocking.
fn type_correction(held: &Correction) -> bool {
    let pid = held.take.pid;
    let (bundle_id, _) = crate::focus_capture::app_identity(pid);
    let type_in = |text: &str| crate::type_over_selection(pid, bundle_id.as_deref(), None, text);
    let field = crate::text_insert::describe_focused(pid);
    let mut outcome = None;
    let applied = last_take::apply_held(held, |pid, owned, b, a| {
        let result = crate::text_insert::correct_focused(pid, owned, b, a, Some(&type_in));
        outcome = Some(result.as_ref().map(|_| ()).map_err(Clone::clone));
        result
    });
    // Positions only: the log never holds what was dictated.
    eprintln!("[kass] held correction in {bundle_id:?}: {outcome:?}; field {field}");
    applied.is_some()
}

fn notify(app: &AppHandle, pid: i32) {
    let body = match crate::focus_capture::app_identity(pid).1 {
        Some(name) => format!("Updated in {name}"),
        None => "Updated where you dictated it".into(),
    };
    let _ = app.notification().builder().title("Kass").body(body).show();
}
