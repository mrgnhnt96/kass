# Dictation handshake

Kass inserts dictated text into the text field focused when the user presses the dictation keys. Some apps, like terminals, have no real text field until the user starts writing. The dictation handshake tells such an app that a dictation is about to start, so it can show and focus a field (a native `NSTextView` composer, for example) before Kass looks.

Any macOS app can take part. Apps that don't see no change and no added delay.

## Opting in

Do either:

- **Info.plist** (bundled apps): set `KassDictationHandshake` to the boolean `true`. A string such as `"YES"` is not accepted.
- **At runtime** (dev builds that aren't bundles, or any app): post `com.mrgnhnt.kass.handshakeSupported` with `{pid}`. Kass remembers the pid until it quits, so post it at launch and again now and then (every few seconds is fine) to reach a Kass that started after you.

Kass only asks the **frontmost** app (`NSWorkspace.frontmostApplication`), so the pid must be that app's.

## The exchange

All four messages are `NSDistributedNotificationCenter` notifications with `object` nil. `pid` is an `NSNumber`, and the other values are strings. Post with `deliverImmediately: true`, and ignore notifications whose `pid` isn't yours.

| # | Name | From | userInfo |
|---|------|------|----------|
| 1 | `com.mrgnhnt.kass.dictationWillBegin` | Kass | `pid`, `mode`: `"dictate"` or `"command"` |
| 2 | `com.mrgnhnt.kass.dictationReady` | app | `pid` |
| 3 | `com.mrgnhnt.kass.dictationDidEnd` | Kass | `pid`, `outcome`: `"inserted"`, `"cancelled"` or `"failed"` |
| – | `com.mrgnhnt.kass.handshakeSupported` | app | `pid` |

1. The user presses the dictation (or command) keys. Kass opens the microphone, then posts **dictationWillBegin**.
2. The app shows its field, makes it first responder, and posts **dictationReady**.
3. Kass waits up to **150 ms** for the reply, then reads the focused element and the text around the caret as usual. With no reply in time, Kass carries on anyway and logs the timeout. Nothing is lost: the microphone was already recording.
4. When the dictation is over, Kass posts **dictationDidEnd**. `inserted` means the text went into the field. `cancelled` means nothing went in and nothing went wrong (Escape, a dictation too short to keep, silence). `failed` means Kass couldn't insert it, and the text is in Kass's Captures. Hide the field or act on its text as fits your app.

`command` mode means the user is about to speak an instruction for the **selected** text, so keep or restore the selection in the field you focus.

## What the field must support

Kass inserts through Accessibility, so the focused element should be a standard text view: role `AXTextArea` (or `AXTextField`), a settable `AXSelectedText`, and readable `AXValue` and `AXSelectedTextRange` so Kass can see the text before the caret and verify the insert. An `NSTextView` gives you all of this.

## Example (Swift)

```swift
let center = DistributedNotificationCenter.default()
let pid = Int(ProcessInfo.processInfo.processIdentifier)
let mine = { (n: Notification) in (n.userInfo?["pid"] as? NSNumber)?.intValue == pid }

center.addObserver(forName: .init("com.mrgnhnt.kass.dictationWillBegin"), object: nil, queue: .main) { n in
    guard mine(n) else { return }
    composer.show()                      // your field
    window.makeFirstResponder(composer)
    center.postNotificationName(.init("com.mrgnhnt.kass.dictationReady"),
                                object: nil, userInfo: ["pid": pid], deliverImmediately: true)
}
center.addObserver(forName: .init("com.mrgnhnt.kass.dictationDidEnd"), object: nil, queue: .main) { n in
    guard mine(n) else { return }
    composer.dictationEnded(outcome: n.userInfo?["outcome"] as? String)
}
```

## Testing the round trip

`scripts/handshake-responder.swift` acts as an opted-in app, and `examples/handshake_roundtrip.rs` acts as Kass's side using Kass's own notification code:

```sh
swift scripts/handshake-responder.swift &        # add --delay-ms 300 to see the timeout
cd app && cargo run --example handshake_roundtrip -- 30
```

On an M-series Mac, `dictationWillBegin` → `dictationReady` takes about 2 ms (1.2 to 2.2 ms over 30 rounds).

In Kass's log, look for `[handshake]` lines: the ready time per dictation, timeouts, and `dictationDidEnd`.

## Inside Kass

`app/src/dictation_handshake/`. The hotkey dispatcher calls `before_focus` between `dictation::start` (microphone open) and `focus_capture::capture_focus`. The take's task calls `take_ended` once the dictation is settled. Replies are observed on the main run loop, where the distributed center delivers them. For an app that hasn't opted in, the check is a pid and bundle-path lookup plus a cached Info.plist answer: about 0.1 ms, or about 1 ms the first time Kass sees that app.
