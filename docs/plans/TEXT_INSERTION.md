# Text insertion without the clipboard (work item 5)

Code: `app/src/text_insert.rs`, hooked at the top of
`paste_final_text` in `main.rs`.

## What happens now

1. After the final text arrives, `paste_final_text` asks the target app
   (the PID from the chord-start focus snapshot) for its
   `AXFocusedUIElement`. It asks the app, not the system-wide element, so
   the pill holding key focus cannot redirect the text.
2. It reads the role, subrole, whether `AXSelectedText` is settable, the
   selected range and the character count. Direct insertion is used only
   when all of these hold:
   - the role is `AXTextField`, `AXTextArea` or `AXComboBox`;
   - neither role nor subrole is `AXSecureTextField`;
   - the app is not a terminal (Terminal, iTerm2, Warp, kitty, WezTerm,
     Alacritty, Ghostty);
   - `AXSelectedText` is settable;
   - the selected range and the character count can both be read.
3. It sets `AXSelectedText`. This replaces the selection or inserts at the
   caret. Then it reads the element again:
   - The caret is at start + length and the count grew as predicted:
     **done.** No clipboard, no pill hide, no 80 ms sleep, no ⌘V, no
     400 ms wait.
   - The element changed, but not exactly as predicted (autocorrect, a
     length limit): **done.** Nothing is pasted, so there is no second
     copy.
   - The element is exactly as before: if the app rejected the set, fall
     back to clipboard + ⌘V at once. If it accepted the set, re-read up to
     3 times, 15 ms apart (45 ms at most), then fall back. This covers
     apps that apply the change late.
   - The element can't be read afterwards: **no paste.** Kass shows
     an error, and the text is still in Captures.
4. Every AX call has a 250 ms timeout, so a hung app can't stall the
   paste for the default 6 s.
5. If the target wasn't frontmost, it is activated after the insertion
   (no settle sleep), as the ⌘V path did before.

Each attempt writes one log line to stderr:
`[kass] text insert into <bundle id>: <outcome> in <n> ms`. The
outcome is `Inserted { exact: true }`, `Inserted { exact: false }`,
`UseClipboard(<reason>)` or `Uncertain(..)`. Run a dev build from a
terminal to see these lines.

A quick signal without logs: on the direct path, the clipboard (or a
clipboard manager's history) never contains the dictated text.

## Manual checklist

For each app, place the caret mid-sentence and dictate once. Then select
a word and dictate again (it should replace the selection). Then press ⌘Z
once.

| Target | Expected path | Check |
| --- | --- | --- |
| TextEdit (rich and plain) | Direct (`Inserted`) | Text appears instantly at the caret or over the selection. Clipboard unchanged. ⌘Z removes the dictated text in one step. |
| Safari `<textarea>` / `<input>` | Direct, most likely | Text appears, and the page sees it: a React-style field keeps it after the next keystroke, and a Send button enables. If the text shows but the page ignores it, that is the "succeeds but doesn't register" risk (below). |
| Safari/Chrome `contenteditable` (Gmail compose, Notion) | Direct or clipboard | Same as above. If an editor rejects it, you should see exactly one copy from the ⌘V fallback. |
| Chrome `<textarea>` | Clipboard on first use, maybe direct after that | Chrome only builds its AX tree when a client asks, so the first attempt may log `NoFocusedElement` / `NotATextRole`. Either way, exactly one copy. |
| VS Code editor (Monaco) | Either | Exactly one copy appears in the editor, the cursor ends after it, and ⌘Z undoes it. Watch for text that appears in a hidden textarea but not in the editor. |
| Terminal / iTerm2 / Ghostty | Clipboard (`ClipboardOnlyApp`) | Same as before this change: pasted at the prompt. |
| Slack (Electron) message box | Either | One copy in the composer, and Send posts it. |
| Password field (Safari login, System Settings) | Clipboard (`SecureField`) | Never inserted directly. Same behavior as before. |
| Kass's own windows | Skipped | No change: pasting into Kass stays disabled. |

Report any app where the text appears twice, doesn't appear, or appears
but the app ignores it. The fix is usually to add its bundle ID to
`CLIPBOARD_ONLY_BUNDLES` in `text_insert.rs`.

## Known risks

- **Succeeds but doesn't register.** An app can accept `AXSelectedText`
  and report the new text through AX, but its own model never sees an
  input event. The text then looks inserted, but it isn't sent or saved,
  or it vanishes on the next keystroke. Verification can't detect this,
  because AX reports success. Web editors built on custom models are
  most at risk. The workaround is the per-app denylist.
- **Late application.** If an app applies the change more than about
  45 ms after accepting it, it looks unchanged, the fallback pastes, and
  the text appears twice. No app is known to do this yet. Raise
  `VERIFY_POLLS` if one does.
- **Undo.** Cocoa text views usually record the AX change as one undo
  step, but some apps don't record it at all. ⌘Z then can't remove the
  dictated text. The ⌘V path always made an undoable paste.
- **Rich text.** Direct insertion uses the style at the caret, as typing
  does. The old ⌘V path pasted plain text, so the result should look the
  same.
- **Fallback cost.** When a field isn't eligible, the decision takes a few
  AX reads (usually a few ms) before the unchanged ⌘V path runs. The
  45 ms re-read only happens when an app accepts the set and then shows
  no change.
- **Windows/Linux.** No change; they always use the clipboard path.
