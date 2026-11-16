//! The menu bar: the same menus Tauri gave Kass on macOS. Each item sends
//! its AppKit action down the responder chain, to the view under the
//! keyboard.

use gpui_kit::{actions, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};
use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_app_kit::NSApplication;

actions!(
    kass,
    [
        About,
        HideKass,
        HideOthers,
        ShowAll,
        Quit,
        CloseWindow,
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        SelectAll,
        FullScreen,
        Minimize,
        Zoom,
        TogglePalette,
    ]
);

/// Send `action` to the first responder that takes it.
fn send(action: Sel) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let _: bool = unsafe {
        msg_send![
            &*app,
            sendAction: action,
            to: std::ptr::null::<AnyObject>(),
            from: std::ptr::null::<AnyObject>()
        ]
    };
}

pub fn install(cx: &mut App) {
    cx.on_action(|_: &About, _| send(sel!(orderFrontStandardAboutPanel:)));
    cx.on_action(|_: &HideKass, _| send(sel!(hide:)));
    cx.on_action(|_: &HideOthers, _| send(sel!(hideOtherApplications:)));
    cx.on_action(|_: &ShowAll, _| send(sel!(unhideAllApplications:)));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CloseWindow, _| send(sel!(performClose:)));
    cx.on_action(|_: &Undo, _| send(sel!(undo:)));
    cx.on_action(|_: &Redo, _| send(sel!(redo:)));
    cx.on_action(|_: &Cut, _| send(sel!(cut:)));
    cx.on_action(|_: &Copy, _| send(sel!(copy:)));
    cx.on_action(|_: &Paste, _| send(sel!(paste:)));
    cx.on_action(|_: &SelectAll, _| send(sel!(selectAll:)));
    cx.on_action(|_: &FullScreen, _| send(sel!(toggleFullScreen:)));
    cx.on_action(|_: &Minimize, _| send(sel!(performMiniaturize:)));
    cx.on_action(|_: &Zoom, _| send(sel!(performZoom:)));
    cx.on_action(|_: &TogglePalette, cx| crate::palette::toggle(cx));

    // Before set_menus: GPUI reads the menus' shortcuts from the keymap.
    cx.bind_keys([
        KeyBinding::new("cmd-h", HideKass, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("ctrl-cmd-f", FullScreen, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-k", TogglePalette, None),
    ]);

    cx.set_menus([
        Menu::new("Kass").items([
            MenuItem::action("About Kass", About),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Kass", HideKass),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Kass", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette", TogglePalette),
            MenuItem::separator(),
            MenuItem::action("Enter Full Screen", FullScreen),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]);
}
