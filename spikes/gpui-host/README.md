# GPUI host spike (T561)

Throwaway prototype, not shipped. It proves GPUI can own the app loop and host the existing React screens in wry web views while they are ported.

Build the React bundle first (`tauri/dist`), then:

```sh
cargo build
./target/debug/gpui-host-spike            # runs the app
./target/debug/gpui-host-spike status     # from another shell: talk to it over its control socket
```

Control commands: `status`, `ipclog`, `pill show|hide`, `show`, `emit <event> <json>`, `focus <label>`, `eval <label> <js>`, `paste`, `menu`, `send <selector>`, `quit`. Set `SPIKE_DIST` to serve a different bundle.

What it covers:

- Main window: GPUI header plus a wry child web view serving `tauri/dist` at `tauri://localhost` (same origin as Tauri, so localStorage and the server's CORS list carry over).
- A JS bridge that stands in for `window.__TAURI_INTERNALS__`, so `@tauri-apps/api` `invoke`/`listen`/`emit` work unchanged.
- Edit menu and ⌘A/⌘C/⌘V/⌘Z reaching the web view (bind keys before `set_menus`, forward the actions to the responder chain).
- The capture pill: a GPUI PopUp panel, transparent, made never-key by a class swap done before the web view attaches. `pill-over-finder.png` shows it over another app with Kass in the background.
- A menu bar item via `tray-icon`.
- Clean quit: web views are leaked at exit because GPUI frees the windows first.
