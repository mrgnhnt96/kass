# Spoken self-corrections

The local 0.6B refinement model can retain both versions of a statement or even reverse the correction. The existing list/newline parser does not handle ordinary sentence corrections, so those cases previously depended entirely on generative interpretation.

The conservative correction pass handles:

- Explicit “no actually”, “no wait”, “wait no”, “I mean”, “I meant”, “or rather”, and repeated “no, no, no” cues.
- Replacement clauses that repeat at least two starting words of the immediately preceding clause.
- A pronoun restating one local copular value: “my favorite candy is A; no, no, it's B”. The subject stays intact, the final value wins, and the correction cue disappears.
- Repeated corrections, with earlier/later unrelated sentences retained.

It does not search backward past an intervening clause or reinterpret quoted/reported speech. Pronoun-based edits reject competing coordinated antecedents, negations, and tense/number mismatches. Unsupported cases retain the existing LLM path. Accepted edits skip generative rewriting so the small model cannot restore the rejected text. Other filler/grammar cleanup is not additionally applied to those takes.

## Corrections after a pause

A correction said after a pause arrives as its own phrase, often after the text it corrects has already settled during streaming cleanup. `retracted_by` decides whether that phrase takes earlier words back:

- A retraction cue (“no wait”, “scratch that”, “make that”, “I meant”, “or rather”) always does.
- An opening cue that can also start an answer or a new thought (“no”, “wait”, “sorry”, “actually”) does only when a short replacement value follows (“Sorry, Wednesday.”) or the rest restates earlier words with a change. “No, unless you replace that key…” continues the thought and takes nothing back.
- With no cue, the phrase must repeat three earlier words, one of them a content word, then say something else in their place (“The meeting is on Wednesday.” after “…on Tuesday.”).

When it does, streaming reopens the last settled chunk so the correction is cleaned together with it. If settled text older than that chunk remains, the whole take is cleaned again at release. The content check lets these cleanups drop the words the correction took back. If it rejects the whole-take cleanup anyway, the text cleaned while speaking is kept.

The candy regression must return exactly:

`Alright, my favorite candy is Reese's Pieces.`

## Speech recognition is a separate problem

The earlier personal phrase-replacement feature was the wrong solution for a misheard name. It has been removed from runtime code, settings, API schemas, and UI, and the running app's saved map was cleared. Existing databases may retain an unused additive column, but no code reads or applies it. There are no hardcoded personal-name substitutions.

The original name recording is evaluated against speech-recognition models directly. Refinement must not manufacture “Morgan Hunt” from “going to hunt” without audio evidence. Model comparison findings and installed validation are recorded below when completed.

Audio investigation (2026-09-16): replayed the saved four-second name clip through Whisper Turbo with default decoding, an English hint, temperature zero, previous-utterance context, a name prompt, and added leading silence. None recovered the full name. Full Whisper Large v3 also failed on that clip, while correctly transcribing the separate “This is Morgan Hunt” clip. The Transformers reference Turbo decoder with one and five beams likewise retained “going to hunt”. These results do not justify changing the active recognition model or claiming the name issue is solved. Turbo remains selected; no text substitutions are used.

Installed validation for the candy fix: 79 backend tests and 11 frontend tests passed; TypeScript checks and the signed desktop build passed. The packaged HTTP endpoint returned the expected candy sentence, preserved surrounding sentences, and left a misheard name unchanged instead of substituting it. The live settings API no longer exposes text replacements. The user's reported candy capture was re-refined and its corrected result was verified in the Captures UI. Current selections (Whisper Turbo and Qwen3 1.7B) were preserved.
