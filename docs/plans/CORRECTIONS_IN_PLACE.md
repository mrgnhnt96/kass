# Corrections in place

A correction saved in Captures (or a dictionary word that respells one) also fixes the text where Kass just wrote it, when that text is still exactly as Kass left it. Anything else changes nothing and says nothing. Takes are tracked only while the Voice edits setting is on, since that tracking is shared with voice edits.

## Two paths

| The take went in by | When the fix is made | How | Said by |
| --- | --- | --- | --- |
| Accessibility (TextEdit, Notes, most native fields) | At once, the app behind Kass | `AXSelectedText` over the changed words | A toast in Kass: "Updated in Notes" |
| Keys or ⌘V (Slack and other Electron apps) | When the app is next in front | The changed words selected over Accessibility and typed over | A notification: "Updated in Slack" |

Electron apps accept an `AXSelectedText` write and ignore it, and while behind another app they don't say which field is focused (`AXFocusedUIElement` has no value), so nothing can be done there from Kass. In front, Slack's composer reads back (value, caret) and takes a selection.

## Flow

```
Captures: save a correction ──► POST /captures/{id}/feedback ──► on success
  ──► invoke('apply_correction', {captureId, before: the text as shown, after: the fix})
  ──► last_take::apply_correction: only Kass's last take, only its capture
        ├─ Accessibility take ──► text_insert::correct_focused(…, None) ──► app name ──► toast
        └─ typed take ──► held (last_take::HELD) ──► corrections::watch
              every 250 ms: is its app in front? ──► 300 ms settle ──► correct_focused(…, Some(type_over_selection))
              ──► notification on success; dropped after 30 min, or when another take replaces it
```

## Rules

- **Only the last take.** `dictation/last_take.rs` remembers the take that read back: where Accessibility wrote it, or where keys or ⌘V did, read back on its own thread for up to a second after delivery (`AppEnv::remember_typed`), never delaying the paste. A read that finishes after a newer take went in is dropped (`remember_if` with the take generation).
- **Only as Kass left it.** The field must still show the take with the caret right after it (`intact`). Typing, moving the caret or changing fields since makes it decline. For a typed fix, the selection must read back where it was set before anything is typed, or nothing is typed.
- **Only the changed end.** The capture's text and the field's can differ at the start (a leading space, a capital fitted to the field), so `correct_owned` matches from where `before` and `after` first differ to the end. A fix of the very first letters may decline for that reason.
- **Stacked fixes.** A fix that worked updates the last take to read as corrected, so the next correction of the same capture applies too. Held fixes of the same text join into one edit, from what the field shows to the newest fix.
- **Never from behind.** Nothing is typed into an app that isn't in front: a held fix waits for the user to go back to it.

## Left for later

- Undoing a correction in Captures doesn't revert the field.
- Voice edits in Electron apps: they take the Accessibility path and are ignored; `correct_focused` with `type_in` is the fix.
- Messages and Safari web fields (typed takes there are now tracked, so they take the held path).
