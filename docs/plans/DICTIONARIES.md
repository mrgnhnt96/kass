# Dictionaries

## Problem

Kass learns vocabulary only on its own: `known_names.py` guesses names from past dictations, and `correction_rules.py` learns a fix after the same correction shows up in at least two captures. The user can't just tell it "I say Kubernetes", "write my handle as mrgnhnt96", or "in Zed, 'voice box' means Kass". Whisper hears such words wrong every time until corrections pile up, and some never qualify: `candidate()` refuses digits, single-word context and dissimilar spellings.

## Entries

An entry is one row: what to **write**, and optionally what is **said**.

| Kind | Example | Effect |
| --- | --- | --- |
| Term (`spoken` empty) | `Kubernetes`, `mrgnhnt96`, `Zed` | Whisper is prompted with it, so it is heard right. A term that isn't a common word also has its capitals fixed after cleanup ("kubernetes" → "Kubernetes"). A capitalized term counts as a known name. |
| Replacement | said `voice box` → written `Kass` | After cleanup, the spoken phrase is swapped for the written text, matched case-insensitively on word boundaries. `written` is also a term. |
| Phrase (`phrase` on) | said `insert my email` → `you@example.com`; said `sign off` → `Thanks,⏎Morgan` | A replacement whose `written` is text to insert exactly. It keeps its line breaks (each line's spaces tidied, blank lines at either end dropped) and may be up to 1,000 characters. It is never a term: not prompted to Whisper, never recased, never matched by sound. `spoken` is required. |

Whatever a replacement or phrase writes is left as written: terms recase and sound-match only the text around it.

Capitals are fixed only for terms that aren't common words, using `phrase_seams._common_word`. That way the term `Mark` never capitalizes the verb "mark", and `mrgnhnt96` always gets fixed. This is one general rule, with no per-term exceptions.

**Sound matching** is a property of each entry (`match_sound`, on by default). Off, the word is still prompted to Whisper, recased where it is spelled exactly (any case, same letters) and counted as a known name, but never swapped in for a word that only sounds like it: a `Meghan` term with it off leaves `Megan`, `Meagan` and `Meghann` alone. The word also stays protected from other terms' sound matching, as every term is. Where two active entries write the same word, the most specific decides.

**Spoken fixes.** When the user fixes a word by spelling it aloud ("fix that, Meghan, M-E-G-H-A-N"), Voice Edits calls `dictionary.add_spelled_word(letters, bundle_id, heard)`, which adds it with no confirmation: everywhere (a name is the same in every app), `match_sound` off, `source = "spoken_fix"`. `spelled_word` writes letters in one case as a name ("MEGHAN" → "Meghan"), keeps the heard word's own capitals when it is the same letters ("NASA", "iPhone"), and keeps mixed case or digits as spelled. Nothing is added when the app's dictionary already writes the word, and an existing entry is never changed; a word said the same way that writes something else stays too. Editing a spoken-fix entry makes it the user's (`source` cleared).

**Corrected words.** When the user corrects a transcript (a manual report or a voice fix, never a redictation), `capture_feedback.save_feedback` calls `dictionary.add_corrected_words`. `corrected_words` finds each one- or two-word term written in place of up to three heard words that isn't all common words, digits or a drawn-out spelling ("wayyy"). A term is kept only when it fixes that very mistake: as a term matched by sound ("Pikto" → "Picto"), or, when the heard form isn't a common word, as a replacement of it ("Clard" → "Claude"). A common word heard ("cloud", "clod", "cash") is never replaced, since the user says it too, but the term is still prompted. Entries go everywhere with `source = "correction"` and `added_by` the report's id: withdrawing, amending or deleting the report removes them, and a word or misheard form an entry already has is left to that entry. Of the user's first 133 reports, this would have added Picto, Saggar, Zonai, taskboard and Claude (from "Clard").

## Scopes

An entry applies in one or more places, each a scope:

- **Everywhere**: every dictation.
- **A writing style**: every app assigned to that style (`app_styles`, or the default style for a new app).
- **An app**: one bundle id.

Everywhere excludes the others: choosing it clears the rest. A dictation merges the three scopes for its app. For terms, the lists are combined and duplicates dropped. For replacements with the same spoken phrase, the most specific scope wins: app, then style, then everywhere. Deleting a style moves its entries to the default style, the same way it moves the style's apps. An app entry keeps its bundle id even while the app has no captures.

## Storage

The entries live in a new table, `dictionary_entries`: `id`, `scope` (`global` / `style` / `app`), `scope_id` (null for global, else a style id or bundle id), `written`, `spoken` (nullable), `app_name` (for display, app scope only), `group_id`, `match_sound` (default true), `source` (null for the user, `spoken_fix`, `correction`), `created_at`. There is one row per place; the rows of an entry that applies in several places share `group_id` (null: the row is its own entry, as rows from before groups are). `migrations.py` adds `group_id`, `match_sound`, `source`, `added_by` and `phrase` (default false) to an existing table. There's a unique constraint on (`scope`, `scope_id`, `casefold(spoken or written)`), so a place never holds the same word said twice; the API names the place in its 409. Matching, prompting and dictation read rows and never see groups.

`services/dictionary.py` keeps an in-memory snapshot, like `styles.snapshot()`. Writes rebuild it. `for_app(bundle_id)` returns a resolved `Dictionary`: prompt terms in priority order, compiled replacements, and the recase set. That result is cached per (bundle id, style) until the next write. Dictation itself never reads from disk or the database.

## Where it applies

**Whisper prompt.** `transcribe` / `transcribe_array` take a new `vocabulary: Sequence[str]` argument, the terms in priority order. `_transcribe_sync` builds `initial_prompt` as the terms that fit, then `previous_text` (`mlx_backend.phrase_prompt`). Whisper keeps only the last ~223 prompt tokens and cuts from the front, so the terms, which come first, would be cut first. To prevent that, `previous_text` is trimmed (by tokens, not the 600-char `PHRASE_CONTEXT_CHARS`) so terms plus context fit. Terms get at most 64 tokens. They fill in priority order (app, then style, then everywhere, newest first within a scope; plain terms before what replacements write, since a replacement fixes its word whatever Whisper hears). The first term that doesn't fit ends the list, and whatever doesn't fit is left out of the prompt but still applies after cleanup. The prompt reads as a plain list: `"Kubernetes, mrgnhnt96, Zed."`. The earlier sentence still comes last, so continuing a phrase works the same as it does today.

Every path that runs Whisper passes it: phrase recognition (`capture_stream.recognize`), full-audio reconcile, upload (`captures.py:197`) and retranscribe (`captures.py:398`). The stream resolves the global and default-style dictionary when the session starts, and the app's at `set_app`, usually before the first phrase.

**After cleanup.** `Dictionary.apply` runs directly after `apply_learned_corrections` everywhere that function is called (`StreamingCapture.corrected` in the stream, and `captures.py` refine). That way the user's explicit entries win over learned rules. It applies replacements (overlapping matches: the longest spoken phrase wins), then fixes term capitals, then terms Whisper heard a little wrong. Those are matched by sound (`_sound`: no spaces or marks, one of each doubled letter, `c`/`k`/`q`, `z`/`s`, `ph`/`f`, `y`/`i`). A match is either the term split into up to one word more than it has ("cuber netes", "tail scale"), or one uncommon word that sounds the same ("Sagar" for "Saggar") or, for terms of five or more sounds, is at least 0.82 similar ("Kubernetis"). A common word is never taken for a term, words across punctuation or a possessive never join, and a word that is itself a term stays as it is. Without this, a term only helped where Whisper already heard it, and a misheard term stayed wrong until a learned correction qualified. With cleanup off, only the Whisper prompt applies: the raw transcript is saved as heard. Provisional text holds back the longest match plus one word, so a replacement never lands after its words were shown.

**Names.** Capitalized terms join `known_names()` for that dictation, so `continue_phrase` and `style_first_word` keep their capitals.

**Not in the cleanup prompt.** The LLM's system prompt is cached per style (`refinement._cache_key`), and switching apps would re-prefill it (1.6–3.2 s, measured in PER_APP_STYLE.md). Terms reach the text through Whisper, and the fixes run deterministically afterward, so the LLM doesn't need them.

**Command mode.** Terms go into Whisper's prompt so the instruction is heard right. Replacements don't touch command output, which may be a translation or a rewrite.

## Speed and quality gates

- Replacement and recase run with the same hard 5 ms limit that `correction_rules.evaluate` uses, tested at `MAX_TEXT` with 500 entries.
- There are at most 1,000 entries across all scopes.
- Whisper prompt: measure the phrase decode time with 0 and with 64 term tokens on the M2 Max. Expect under 15 ms. If it's slower, lower the token cap.
- Quality: replay the corrected captures in `capture_feedback` with the global terms taken from their expected text. Word errors must not rise on any capture that has no dictionary words. If Whisper starts hallucinating the list into silent or short phrases, `speech_detect` already skips silent audio; add a test that a phrase never outputs only the term list.

## API

- `GET /dictionary`: every entry, newest first, each with its `places` (`{scope, scope_id, app_name}`).
- `POST /dictionary`: `{written, spoken?, places, match_sound?, phrase?}`. Entries come back with `match_sound`, `source` (`user` / `spoken_fix` / `correction`) and `phrase`. An entry stays a word or a phrase; `PATCH` doesn't change it, and refuses to clear a phrase's `spoken`.
- `PATCH /dictionary/{id}`: `{written?, spoken?, places?, match_sound?}`; changing `places` adds and removes rows, keeping the entry's date. `DELETE /dictionary/{id}` removes it everywhere.
- `GET /dictionary/resolved?bundle_id=`: the rows one app uses, most specific first, each with its entry's id, and which terms fit the Whisper prompt.

## UI

Settings gets a **Dictionary** page, next to Writing style (the canvas "Dictionary Page Concepts", concept D).

- A scope list on the left: Everywhere, then writing styles, then apps, each with how many entries apply there.
- The selected scope on the right. At the top, an add form reads "When I say" → "Write", then Add; an empty "When I say" only teaches the spelling. New entries go in the selected scope.
- Its entries, newest first: said → written, a date, edit and delete. A spelling-only entry shows "spelling only" where the said words go.
- Editing opens a strip under the row with an "Applies in" dropdown: a checkbox list of Everywhere, the styles and the apps. Checking Everywhere clears the rest. Below it, a switch: "Also fix words that sound like it" (`match_sound`).
- Beside the written word, a row notes "spelled aloud" for a spoken-fix entry, "from a correction" for a corrected word, or "exact spelling" for one with sound matching off.
- For an app or style, "Also applies in <scope>" rows show what it inherits from its style and from everywhere: the first word or two and a count, opening to the list. There's also a note when some terms don't fit in the Whisper prompt.

This covers only the Settings page. The capture pill doesn't change. An "Add to dictionary" action from a correction in the Captures tab is a possible follow-up, not part of this work.

### Phrases

The canvas "Kass Replacements", flow page.

- The scope pane has a **Words | Phrases** switch (`?kind=phrases`), each with its count. Words is the page as above. Phrases has its own add form: "When I say" with a mic button → "Write exactly", a text box where ⏎ starts a new line and ⌘⏎ adds. Rows show the text with its lines.
- **Saying the phrase.** The mic button, or the push-to-talk shortcut, fills "When I say" with the take's raw transcript (lowercase, no punctuation around it), not the cleaned text: cleanup may reword it, or swap in a phrase already in the dictionary. A shortcut take goes to the phrase box while it is focused or no field is; with another field focused ("Write exactly", say) it types there as usual. The form claims the take through `claimInAppDictation` (`useInAppDictationInsert.ts`) before the text would be typed, and answers Rust that it was inserted, so the pill shows no error. The raw transcript is the newest capture's, when that capture is the one that delivered the text; otherwise the delivered text stands in. While a take is heard the box shows that it is listening, and the hint reads "Let go of ⌘ ⌥ to fill in the phrase" with the user's own keys.
- **Captures.** Text a phrase wrote is underlined in the transcript card, and opens what wrote it with **Edit phrase**. "What changed" counts it as a phrase, not a rewording. A phrase fired when its `spoken` is in the raw transcript and its `written` is in the delivered text.
- **From a correction.** When a correction replaces words with other text, its change offers **Make a phrase** next to **Add to dictionary**. The dialog fills "When I say" with the words heard and "Write exactly" with the new text, notes when another phrase already writes that text, and adding it saves the correction too.

## Tests

- `test_dictionary.py`: scope merge and precedence, deleting a style moves its entries, replacement boundaries and case, longest-match overlap, common-word terms left uncapitalized, 5 ms limit.
- `test_mlx_whisper_transcribe.py`: prompt layout, and the token budget keeping terms when previous text is long.
- `test_capture_stream_*`: a dictionary set at `set_app` reaches `recognize` and the finished text. A late app falls back to the global terms.
- `app/tests`: the Dictionary page's add, edit and delete per scope; phrases told apart from words, phrases made from a correction, and which phrases a capture used.

## Measured

Whisper large-v3-turbo on 42 of the user's corrected dictations (1.5–25 s), with a full 64-token term list (18 terms):

| | Word errors | Punctuation marks | Decode time |
| --- | --- | --- | --- |
| No dictionary | 82 | 122 | median 479 ms |
| Comma list (shipped) | 83 | 119 | +0 ms median, +34 ms p90 (noise) |
| "Words I use: …" | 85 | 110 | |
| "Glossary: …" with sample sentences | 98 | 128 | |

Recordings containing a dictionary word improved (13 → 11 word errors). One without any went from "the recording" to "that recording". Whisper's raw punctuation drops slightly, and cleanup repunctuates.
