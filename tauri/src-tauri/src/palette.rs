//! The ⌘K palette's window: a borderless child of the main window over its
//! content area, over the screen showing there.
//! It closes on Escape, a click outside the panel, ⌘K again, or when
//! another window takes the keyboard.

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::CaptureAppFilter;
use kass_i18n::tr;
use kass_palette::{CommandPalette, PaletteCommand, PaletteHost};
use kass_ui::{show_toast, Toast};
use objc2::msg_send;
use objc2_app_kit::NSWindow;

use crate::host::webview::{self, OnClose, Spec};
use crate::host::{self, AppHandle};
use crate::shell::{self, data};
use crate::MAIN_WINDOW_LABEL;

const LABEL: &str = "palette";

/// The captures searched: the newest, as the page's palette did.
const SEARCHED: u32 = 200;

/// Open the palette over the main window, or close it when it's open.
pub fn toggle(cx: &mut App) {
    if let Some(window) = webview::get(LABEL) {
        close(&window);
        return;
    }
    // Only over the main window, while the user is in it.
    if webview::with_window(MAIN_WINDOW_LABEL, |main| main.isKeyWindow()) != Some(true) {
        return;
    }
    match webview::open(cx, spec()) {
        Ok(window) => {
            let _ = window.with_ns_window(|palette| {
                webview::with_window(MAIN_WINDOW_LABEL, |main| cover(main, palette));
                palette.makeKeyAndOrderFront(None);
            });
        }
        Err(e) => eprintln!("[palette] {e}"),
    }
}

fn spec() -> Spec {
    Spec {
        label: LABEL,
        options: WindowOptions {
            titlebar: None,
            // A panel that can take the keyboard; `cover` drops it to the
            // main window's level.
            kind: WindowKind::Floating,
            focus: false,
            show: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        prepare: |_| {},
        on_close: OnClose::Close(|| {}),
        root,
    }
}

/// Place `palette` over `main`'s content area (right of the rail, between
/// the title strip and the status line) and tie it to `main`.
fn cover(main: &NSWindow, palette: &NSWindow) {
    let mut frame = main.frame();
    let top = f64::from(shell::TITLEBAR) + 1.;
    let left = f64::from(shell::RAIL);
    let bottom = f64::from(shell::status::HEIGHT);
    frame.origin.x += left;
    frame.origin.y += bottom;
    frame.size.width = (frame.size.width - left).max(0.);
    frame.size.height = (frame.size.height - top - bottom).max(0.);
    palette.setFrame_display(frame, true);
    palette.setLevel(main.level());
    unsafe {
        // NSWindowAbove: it moves with the main window and stays over it.
        let _: () = msg_send![main, addChildWindow: palette, ordered: 1isize];
    }
}

fn close(window: &webview::WebviewWindow) {
    let _ = window.with_ns_window(|palette| {
        webview::with_window(MAIN_WINDOW_LABEL, |main| unsafe {
            let _: () = msg_send![main, removeChildWindow: palette];
            main.makeKeyWindow();
        });
    });
    let _ = window.destroy();
}

struct Host;

impl PaletteHost for Host {
    fn close(&self, _: &mut App) {
        if let Some(window) = webview::get(LABEL) {
            close(&window);
        }
    }

    fn open_capture(&self, id: &str, cx: &mut App) {
        shell::navigate("/captures", &format!("?capture={id}"), cx);
    }

    fn copy_capture(&self, text: String, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        webview::update_window(MAIN_WINDOW_LABEL, cx, |window, cx| {
            show_toast(
                window,
                cx,
                Toast::new(kass_i18n::t("captures-toast-transcript-copied")),
            );
        });
    }
}

fn root(window: &mut Window, cx: &mut App) -> AnyView {
    let commands = commands(cx);
    let palette = cx.new(|cx| {
        // Another window took the keyboard: put the palette away.
        let mut was_active = false;
        cx.observe_window_activation(window, move |_, window, cx| {
            if window.is_window_active() {
                was_active = true;
            } else if was_active {
                Host.close(cx);
            }
        })
        .detach();
        CommandPalette::new(Rc::new(Host), commands, window, cx)
    });
    load_captures(&palette, cx);
    palette.into()
}

fn load_captures(palette: &Entity<CommandPalette>, cx: &mut App) {
    let api = shell::shell_data(cx).read(cx).api.clone();
    let load = data::background(cx, move || {
        api.list_captures(SEARCHED, 0, &CaptureAppFilter::All)
    });
    let palette = palette.downgrade();
    cx.spawn(async move |cx| match load.await {
        Ok(list) => {
            let _ = palette.update(cx, |palette, cx| palette.set_captures(list.items, cx));
        }
        Err(e) => eprintln!("[palette] captures: {e}"),
    })
    .detach();
}

fn command(
    id: impl Into<SharedString>,
    label: String,
    hint: &str,
    run: impl Fn(&mut App) + 'static,
) -> PaletteCommand {
    PaletteCommand {
        id: id.into(),
        label: label.into(),
        hint: kass_i18n::t(hint).into(),
        run: Rc::new(run),
    }
}

fn go(id: &'static str, path: &'static str, hint: &str) -> PaletteCommand {
    command(
        id,
        kass_i18n::t(&format!("palette-cmd-{id}")),
        hint,
        move |cx| shell::navigate(path, "", cx),
    )
}

/// Everything the palette offers besides captures, in the page's order.
fn commands(cx: &App) -> Vec<PaletteCommand> {
    const GO: &str = "palette-keys-go";
    const SETTINGS: &str = "palette-keys-settings";
    let mut commands = vec![
        go("captures", "/captures", GO),
        go("models", "/models", GO),
        go("settings-general", "/settings", SETTINGS),
        go("settings-dictation", "/settings/dictation", SETTINGS),
        go("settings-command-mode", "/settings/command-mode", SETTINGS),
        go(
            "settings-transcription",
            "/settings/transcription",
            SETTINGS,
        ),
        go(
            "settings-writing-style",
            "/settings/writing-style",
            SETTINGS,
        ),
        go("settings-logs", "/settings/logs", SETTINGS),
        go("reports", "/reports", GO),
        go("settings-features", "/settings/features", SETTINGS),
        go("setup", "/setup", GO),
        // Teaching starts from the writing style page.
        go("calibrate", "/settings/writing-style", "palette-keys-style"),
    ];
    let data = shell::shell_data(cx).read(cx);
    // Transforms rewrite the text selected in the app the user came from
    // (docs/plans/COMMAND_MODE.md).
    let transforms = data.settings.as_ref().map(|s| s.command_transforms.clone());
    for transform in transforms.unwrap_or_default() {
        let id = format!(
            "transform-{}",
            transform.id.as_deref().unwrap_or(&transform.name)
        );
        let label = tr!("palette-cmd-transform", name = transform.name.clone());
        let instruction = transform.name;
        commands.push(command(id, label, "palette-keys-transform", move |_| {
            let instruction = instruction.clone();
            host::async_runtime::spawn(async move {
                if let Err(e) = crate::dictation::command_run(AppHandle, instruction).await {
                    eprintln!("[palette] command_run failed: {e}");
                }
            });
        }));
    }
    // Only the themes the user isn't already on.
    let current = data.prefs.theme.clone();
    for theme in ["light", "dark", "system"] {
        if current != theme {
            commands.push(command(
                format!("theme-{theme}"),
                kass_i18n::t(&format!("palette-cmd-theme-{theme}")),
                "palette-keys-theme",
                move |cx| set_theme(theme, cx),
            ));
        }
    }
    commands
}

/// Show it now; every window follows once it's saved.
fn set_theme(theme: &'static str, cx: &mut App) {
    kass_ui::set_theme_choice(data::theme_choice(theme), cx);
    crate::prefs::set_theme(theme);
}
