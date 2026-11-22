# Mid-sentence dictation

## Problem

Every take is cleaned up as if it were a whole message. Refinement capitalizes the first word and ends with a period, and insertion puts the text exactly at the caret with no regard for what surrounds it. That is right in an empty field and wrong everywhere else:

| Field before (`|` = caret) | Spoken | Inserted today | Wanted |
| --- | --- | --- | --- |
| `I think we should|` | "move the meeting to Friday" | `I think we shouldMove the meeting to Friday.` | `I think we should move the meeting to Friday.` |
| `I think we should |` | "move it" | `I think we should Move it.` | `I think we should move it.` |
| `Can you | before lunch?` | "send the report" | `Can you Send the report. before lunch?` | `Can you send the report before lunch?` |
| `Done.|` | "next we ship it" | `Done.Next we ship it.` | `Done. Next we ship it.` |
| `Ask |` | "Morgan about it" | `Ask Morgan about it.` | unchanged (a proper noun stays capitalized) |

Nothing in the pipeline knows what is around the caret. `focus_capture::capture_focus` records the app, pid, and element role. The text next to the caret is never read, so neither refinement nor insertion can adjust.

## Approach

Read a few characters on each side of the caret, then use them in two places:

1. **Recognition (text).** Tell the server whether the take continues a sentence, so the first word keeps a capital only where it is a name (section 2).
2. **Insertion (characters at the join).** Fix the spacing and punctuation where the text meets its neighbors with deterministic rules at the moment of insertion. This is exact and free, and it doesn't depend on the model following instructions.

Where context can't be read (no Accessibility text access, as in some Electron and terminal fields), keep today's behavior. A field we can't see is treated like an empty one, which is what we do now.

## 1. Read the caret context

New `InsertContext { before: Option<String>, after: Option<String> }`: up to ~16 UTF-16 units before the selection start and ~4 after the selection end. When text is selected, the take replaces it, so the context is what surrounds the selection.

Read it twice:

- **At key-down**, next to `capture_focus` in `hotkey_monitor.rs::apply_effect`, for refinement. Do it on a blocking task after the mic has opened and focus has been captured, never ahead of either, so it adds nothing to time-to-first-audio. It uses `AXSelectedTextRange` and `AXStringForRange`, with a fallback to slicing `AXValue`, which is `text_in` in `text_insert.rs`. Store it on the take next to `focus`.
- **At insertion**, for the join rules. It is authoritative because the user may have typed or moved the caret while talking. The AX path already calls `observe()` and has `string_for_range`, so this costs one extra read. For keystroke and clipboard insertion, do the same AX read first when the element allows it.

Send the key-down `before` text to the backend with the take, the same way `TargetApp` goes (`dictation/protocol.rs`, then `capture_stream.py` or the `/captures` form). Store it on the capture so re-refining and model improvement see the same input.

## 2. Capitals: continuing the field's sentence

**Built.** The planned prompt hint was dropped. The transcript decides the case instead, so the cleanup prompt is unchanged.

- **Key-down.** `dictation::set_focus` reads up to 600 UTF-16 units before the caret on a blocking task, after the mic and the focus snapshot (`text_insert::sentence_before_focused`). Terminals and secure fields are skipped. The stream client sends it as a `{"type": "context", "before": ...}` message with the first audio after it is known, or ahead of `finish` (`StreamClient::with_field_before`). The text is never saved.
- **Continues or not.** `phrase_seams.continues_sentence(before)` is true when the last character before the caret (ignoring trailing spaces and closing quotes or brackets) is a letter, a digit, a comma, a semicolon or a dash, with no line break after it.
- **Whisper.** When the take continues, the field text is the first phrase's `initial_prompt`, as earlier phrases already are for later ones.
- **The first word's case.** `phrase_seams.continue_phrase` runs on the first phrase, the same rule as after a pause. It lowercases a capital only for a common word: Whisper's BPE vocabulary has the lowercase word (with a leading space) as a more frequent token than the capitalized one ("move" 1286 < "Move" 10475; "Sarah" has no lowercase token). For words with no token either way it falls back to the system word list. "I", acronyms, words like "GitHub", and names the user capitalizes mid-sentence keep their capital. Those names come from the field text, the phrase itself, and `known_names`: 500 recent cleaned dictations and corrections, cached for five minutes. This also stops pause seams from lowercasing names.
- **Cleanup.** Cleanup capitalizes every start, so `match_raw_start` gives the cleaned text's first word the casing of the same word in the transcript (`StreamingCapture.start_like_raw`, applied in `compose` and to whole-dictation cleanups).

These were measured with `say`-generated phrases, whisper-large-v3-turbo and the 4B cleanup model through a real `StreamingCapture`:

| Field | Without context | With context |
| --- | --- | --- |
| `I think we should ` | Move the meeting to Friday. | move the meeting to Friday. |
| `Please ask ` | Morgan about the budget. | Morgan about the budget. |
| `The problem is ` | That nobody tested it. | that nobody tested it. |
| `I talked to ` | Sarah and she agreed. | Sarah and she agreed. |
| `It depends on ` | Whether GitHub is down. | whether GitHub is down. |

Rejected along the way:
- Whisper's casing alone lowercased only 3 of 6 ordinary words.
- The old `continue_phrase` lowercased the names.
- Whisper's decoder `prefix` broke recognition: most phrases came back as `!`.
- The word list alone has noisy capitalized entries ("For", "Part") and no plurals.

Still ambiguous: words that are both common and names ("Grace", "Mark", "Bill"). As first words they are lowercased unless the user has written them capitalized mid-sentence. The batch upload fallback, used when the stream fails before `finish`, does not get the context yet.

## 3. Insertion: fitting the join

A pure function in a new `app/src/join.rs`: `fit(text, ctx) -> String`, used by every insertion method.

Leading edge:
- `before` ends in a word character or closing punctuation, and `text` starts with a letter or digit: add one space.
- `before` ends in whitespace and `text` starts with whitespace: remove the text's leading whitespace.
- `text` starts with `,.;:!?)` or a closing quote: add no space.
- `before` ends in an opening bracket or quote: add no space.

Trailing edge:
- The first character of `after` that isn't a space is a lowercase letter or a digit, so the sentence keeps going: drop a final `.` from `text`. Only a lone period is dropped, not `?` or `!`, since a question in mid-sentence is rare but real. Then add one space if neither `after` nor the text already has whitespace at that point.
- `after` starts with `.`, `,`, `;`, `:`, `!`, or `?`: drop the text's own final `.` so the result doesn't get a doubled `..` or `.,`.
- `after` is empty, starts a new line, or continues with a capital letter (a new sentence): leave the text's ending as it is, and only fix the spacing.

This runs on the text we *insert*. The capture keeps the refined text, so the Captures tab and the learning code see what the model produced.

### Live insertion

`begin_live`, `extend_live`, and `finish_live` write provisional text as it streams in, and `rewrite` keeps the shared prefix. `fit` is prefix-stable at the leading edge: the same first character always gets the same space. The trailing rules only run in `finish_live`. So the start of each provisional chunk goes through `fit`, the full `fit` runs on the final text, and the `Owned` bookkeeping tracks the fitted text. Read the context once, in `begin_live`, and reuse it for the whole take.

## 4. Corrections and learning

If the user fixes a join that came out wrong, the difference between refined and inserted text must not count as a correction to learn from.

- Store `insert_context` and the actually inserted text on the capture. `capture_feedback.py` and `TeachCorrection.tsx` compare the user's edit against the **inserted** text, and train on the pair with the hint included.
- Personal examples (`personal_examples.py`) should carry `continues_sentence`. Otherwise a lowercase continuation example teaches the model to lowercase whole messages.

## 5. Re-dictation cleanup

**Built.** People put the caret mid-sentence and say the new words, running on into the words already after the caret: `Let's meet | at noon tomorrow.` + "on Friday at noon tomorrow". `overlap::without_repeat` drops the repeated words from the *dictated* text and leaves the field alone, so the field keeps its own spelling, case and punctuation. It runs before `fit`, so it works for Accessibility, keystrokes and paste alike.

The rules are general, with no word lists (the module doc has the details):

- The caret continues a sentence (`continues_sentence`, as in section 2) and is not inside a word. A side that can't be read trims nothing.
- Only the rest of the caret's sentence counts: up to `.?!…` before a capital or the end, or a line break. It must start at the caret, after nothing but whitespace.
- The overlap is the longest end of the dictation equal to a start of that text. Words compare ignoring case and punctuation, split at whitespace, hyphens, slashes and digit/letter changes (`3pm` = `3 pm` = `3 p.m.`).
- At least two field words overlap, and at least one dictated word stays. One word is enough, and the caret may follow a sentence end or start the field, when the overlap is the whole rest of the field's sentence and the dictation ends its sentence on it too: `Thanks. | Morgan.` + "See you on Tuesday, Morgan." (seen in Messages, 2026-10-01).
- One edit per word is allowed when both words have five or more letters, start the same and hold no digits: at most one such word in three, never all. Emoji match exactly. No overlap crosses a sentence end in the dictation.

Where it runs:

- **Paste path.** `text_insert::fit_at_caret` (through `fit_to_focused` in `run_insert_chain`) reads the text after the caret too: up to the dictation's length plus 32 UTF-16 units, clamped to the character count (past the end TextEdit and Notes read nothing, Safari and Gecko a shortened string), dropping a partial last word when cut short. It costs about 0.1 ms. Terminals and secure fields are still not read, and Kass's own window, which isn't fitted either, isn't trimmed.
- **Live text.** `finish_live` reads the same text from the end of the owned text and trims the final text, so `rewrite` shrinks the owned text in one verified write. Drafts are not held back, so they may show the repeat until the final text arrives.

The capture keeps the refined text as the model wrote it. Corrections are compared with that text (section 4), and the inserted text isn't stored yet, so a dropped repeat is never read as a correction. When section 4 stores the inserted text, it has to be the trimmed text: `finish_live` and `fit_at_caret` would need to return it.

Known limits, kept in the tests: emphasis said twice ("no way, no way") is taken as a repeat, and an abbreviation's period (`Dr. Smith`) ends the field's sentence early.

## Tests

- `join.rs`: table tests for every rule above, including emoji and other multi-unit UTF-16 characters in `before` and `after`, a replaced selection, an empty field, a caret right after a newline, and text that is only whitespace.
- `text_insert.rs`: extend the fake `AxTextTarget` so the tests check that the context comes from the right ranges, and that live insertion keeps its prefix stable once fitted.
- Backend: `continues_sentence` detection. The prompt stays byte-identical when the flag is off. The deterministic lowercase leaves "I", "I'm", "NASA", and "GitHub" alone.
- Refinement samples: add mid-sentence cases with proper nouns ("ask Morgan", "on Friday") to the model-improvement evaluation set, and check how often the local Qwen model gets the case right with the hint.
- Real-app check, using `insert_bench.rs`: TextEdit, Notes, Slack, Chrome textarea, and Messages. Measure the added AX read at insertion, which should be about a millisecond.

## Order

1. `join.rs` plus reading the context at insertion. This fixes all the spacing and doubled punctuation, needs no backend change, and ships by itself. **Done.** `text_insert::fit_to_focused` runs once before the insertion chain in `run_insert_chain`, and live insertion reads the context in `begin_live` and keeps it on `Owned`. Insertion into Kass's own window, which goes through the DOM, is not fitted yet. `insert_bench::caret_context_bench` measures the extra AX read in TextEdit.
2. Reading the context at key-down, and the first word's case. This fixes capitals. **Done** (section 2).
3. Capture fields and the correction and learning changes.
4. Re-dictation cleanup. **Done** (section 5).

## Open questions

- How far back should the model see? A few words helps it choose case, but the first sentence of an email pasted as context is also a privacy question and makes the prompt longer. Start with at most the last ~8 words of the current sentence.
- Should mid-sentence `?` and `!` also be dropped when `after` is a word? Proposed: no.
- Where no context can be read, should the app's past takes decide? For example, Terminal fields are nearly always mid-line. Proposed: no, until real data shows a need.
