# Expressive dictation: "!" and stretched words from the voice

Status: phases 1–4 built, 2026-10-05 (see "Phases 2–4 as built"); timing check passed (no added time). Planned 2026-10-01. Prototype and evidence: a scratch prototype run over 312 of the user's saved captures (not in the repo).

## Goal

Text that carries how something was said, not only what was said:

- **"!"** on any sentence, anywhere in the dictation, that was said with clearly more energy than the speaker's norm, when the words fit an exclamation.
- **Stretched words** ("wayyy", "sooo") when the speaker draws a word out.

Not in scope: question marks from intonation (hint only, see below) and sarcasm.

**Nothing may add time after release** (DICTATION_LATENCY_ARCHITECTURE.md). Every piece below either runs while the user speaks or overlaps work that already runs after release.

## Evidence from the prototype

| Signal | Result on the user's recordings |
|---|---|
| Stretched words | Strict rule changed 2 of 312 recordings ("wayyyy more", "fiiiive minutes"); hesitations ("the…", "a…") held back |
| Energy → "!" | Same words, different delivery separate clearly ("Perfect." flat 6th pct vs lively 85th). Emphatic commands ("Commit and push.") also score high, so the text must agree |
| Rising pitch → "?" | Yes/no AUC 0.80, wh 0.48. Auto-applying flags 4% of statements and catches 19% of yes/no questions; mostly wrong flags. Hint only |
| Whisper alone | Wrote "!" in 5 of 1612 transcripts |

## How it works

### While speaking (off the critical path)

1. **Live pitch and loudness.** `StreamingCapture.append` feeds 100 ms chunks (60 ms look-back) to a pitch tracker (Praat autocorrelation via parselmouth, or a native tracker in the Rust capture). Measured: 0.57 ms per chunk, 0.6% of a core; 95.8% of frames within ½ semitone of whole-file analysis; voicing agreement 88.5% (tune look-back).
2. **Speaker baseline.** Running median and spread of pitch level, pitch range and loudness over the user's statements, persisted across sessions. All decisions are relative to it.
3. **Word timings from the decode itself.** `Inference.logits` (mlx-audio `decoding.py:171`) discards the decoder's cross-attention. Keep references to the alignment-head rows (turbo: `[[2,4],[2,11],[3,3],[3,6],[3,11],[3,14]]`) per step. Measured: +3.5 ms vs a −1.9 ms A/A control (noise). Do not use mlx-audio's `word_timestamps=True`: it re-runs the encoder (+280 ms) and is broken in our version (`alignment_heads` vs `_alignment_heads`).
4. Earlier phrases: full DTW word timings (8 ms median, 21 ms p95) and per-sentence energy computed as each phrase is recognized.

### After release

5. **Sentence energy for the last phrase without DTW.** Sentence end time = attention peak of the punctuation token's row. Median 100 ms from DTW; 89% within 300 ms; about 1 ms. Per-sentence energy correlates 0.991 with DTW boundaries; "!" decisions agree 97.9%.
6. **Cleanup gets a per-sentence hint** ("sentence 2 sounded energetic"). The model adds "!" only when the words fit. No hint, no "!" from the audio.
7. **Stretched letters after cleanup.** DTW for the last phrase runs on the CPU while cleanup runs on the GPU. Map refined words back to raw words (difflib, as `sentence_tail.settle`) and stretch the matched word. Rule: ≥3× the speaker's pace, ≥1.5 dB louder than its sentence, a steady vowel of 0.3–0.6 s, not first or last in its sentence, not a closed-class word. Letter count from the stretch (3/4/5). Stretched letter: last vowel, skipping a silent final e.
   - Risk: numba DTW holds the GIL ~8 ms; use `nogil` or move it to Rust, then confirm release-to-paste is unchanged.

### Learning

8. A "!" or stretched word the user removes (or adds) is a correction like any other. Stretched words in particular start strict, and a user who adds them in edits teaches Kass their own stretches (the thresholds move for that user only). This and teaches the style through existing correction learning. No fixed per-style rules.
9. **Settings switch** "Write how it was said" (`capture_settings.expressive`, on by default, in Settings → Transcription → Refinement). Off keeps measuring and learning from edits; it only keeps "!" and stretched words out of the text (phases 2–3 check it before applying).
10. Each capture saves its measurements (sentence energy, stretched words) for the Captures view and for tuning.

## Phases

1. **Measure only.** Steps 1–4, 9–10. No output change. Ship behind the timing check.
2. **"!"**. Steps 5–6.
3. **Stretched words.** Step 7.
4. **Learning.** Step 8.

## Phase 1 as built

- `backend/backends/word_timing.py`: keeps the alignment heads' attention as Whisper decodes (no second pass), word times by numpy DTW (same paths as mlx-audio's `dtw_cpu`). 95.7% of word edges within 50 ms of mlx-audio's own word timestamps.
- `backend/services/prosody.py`: a numpy YIN pitch tracker instead of parselmouth (no new dependency): 95.8% of frames within ½ semitone of Praat, 0.6 ms per 100 ms chunk. `measure` gives per-sentence level, span, loudness, final slope and rise, energy against the speaker's baseline (from the last 200 captures' statements, at least 20), and stretched words with how they'd be written.
- Phrases recognized while speaking get their word times on a worker thread at once; the last phrase's, and the measurement itself (2–7 ms), run after the final event is sent. Saved as JSON in `captures.prosody`. Commands aren't measured.
- Timing check, 8 saved dictations × 2 rounds, real-time replay, on/off alternated: release to final text median 429 ms on vs 434 ms off; paired difference median −5 ms (−61 to +23). No added time.

## Phases 2–4 as built

Built 2026-10-05, while the user was away, from their go-ahead to continue.

- **"!" is decided in code, not by the cleanup model** (a change from step 6). `prosody.exclaim` splits the *cleaned* text into sentences and finds each in the recording by its words (difflib against Whisper's timed words). A sentence ending in a period (after a letter, not an abbreviation like "Dr.", not a period the speaker said aloud) gets "!" when its energy against the speaker's baseline is at least `EXCLAIM_ENERGY` (2.5 when built, 2.9 since 2026-10-14; see below). Why not the hint: no prompt change (the cached prefill stays valid, no added tokens), the small model can't misplace or drop it, and a strict fixed cutoff is what the user asked for. Because sentences come from the cleaned text, the mixed take now works: only its middle sentence got "!".
- **Stretched words** (`prosody.stretch`): the strict rule (`StretchRule`) picks words in the recording, and the written word that matches the heard word is drawn out ("faaar"). A word the cleanup changed is left alone.
- **No added time.** `StreamingCapture.express` uses only what is already worked out and never waits. With the switch on, the last phrase's word times start on a worker thread as soon as it is recognized (`Expression(eager=True)`), so they are ready while the cleanup still runs. If they aren't, the text goes out as written and the log says so. Settled sentences get their "!" when they settle, so provisional text (live text, off by default) already has it.
- **Switch.** Only `capture_settings.expressive` (with cleanup on, not commands) applies anything. Measuring and learning run either way.
- **Saved for learning** (`captures.prosody`, after the final event): `written` (each sentence of the delivered text with its energy) and `shapes` (each word's stretch, loudness and steady vowel), with or without the switch.
- **Learning** (`expression_learning.py`), read again when each dictation starts, from saved measurements and refined-text reports (Captures edits, voice fixes and redictations; newest report per capture). A redictation only makes the rules stricter: its new "!" or drawn-out words teach nothing.
  - "." changed to "!" on a sentence said with energy ≥ 1.5 lowers the cutoff to 0.25 below it. This needs 2 such edits. It is never lower than 98% of the speaker's own plain sentences (with 50+ of them), and never below 1.5. A "!" typed on a calm sentence teaches nothing.
  - "!" changed back to "." raises the cutoff 0.25 above that sentence, from one edit.
  - Words written drawn out ("way" to "wayyy") set the speaker's own stretch rule. This needs 2 such edits; the rule goes 10% under their slowest, 0.5 dB under their quietest, and widens the steady vowel. Closed-class words they stretched ("sooo") are allowed for them only. The rule is slowed until at most 0.5% of their plain words would pass.
  - A word drawn out that they wrote plainly again raises the pace above it.

### Phase 2 checks (2026-10-05)

Full pipeline, real-time replay through `StreamingCapture` (4B cleanup, baseline from the 80 earlier dictations):

- **Labelled set:**
  - All 5 excited takes got "!" ("Perfect!", "That's great news!", "We finally shipped it!", "I can't believe that actually worked!", "Okay, sounds good!").
  - The mixed take gave "!" to its middle sentence only ("…budget. That is amazing! Let's do it…").
  - Nothing else changed: flat takes, stretched takes, the hesitation, the commands and the question kept their marks.
  - The voice was ready in time on all 20.
- **False alarms, 150 ordinary dictations** (2026-10-04 to 10-05): no "!" added, and the voice was ready in time on all 150.
- **Work on the critical path:** applying "!" and stretches to the final text takes 0.2 ms for a short dictation, and 1.4 ms for a 60 s one with 12 sentences. The voice itself (27 ms for 60 s) is worked out on a thread while cleanup runs.
- **Release-time A/B (blocks shipping): passed.**
  - The rerun was after the other job finished: 8 dictations × 2 rounds, alternating on and off, 0.6B cleanup.
  - Paired difference (on − off): median −1 ms, mean +1 ms (−176 to +190). No added time.
  - Absolute release-to-text was ~1.1 s in both arms, against 0.43 s in phase 1's check. The extra time is in Whisper after release, not in this feature: the same audio took 0.34 s on fresh runs and 0.7 s on later ones, with or without expression. The first run also overlapped another session's shared-adapter evaluation on the GPU (~1.2 s in both arms) and doesn't count.

### Thinking drawls (2026-10-14)

Real dictations from 2026-10-05 to 10-14 wrote stretched words the user never meant: "naaame", "cooooode", "leaaarn", "forwaaard", "seeiiing the goaaals". These were words drawn out while finding the next words. The default rule fired on 23 content words, all of them drawls. It never caught a deliberate stretch.

- **No acoustic test separates them.** Drawls are not reliably followed by a pause: "forward" had 0.14 s after it, while the deliberate "fiiive" had 0.32 s. Deliberate "fiiive" and "sooo" are as flat in pitch (0.6–0.8 semitones) as "naaame" (0.5).
- **The default pace went from 3× to 7×.** The drawls reached 6.4×; at 7×, none of the 11,007 saved word shapes passes. The default now practically never stretches. Speakers who write their own stretches still teach a closer rule, as before.
- **The "!" cutoff went from 2.5 to 2.9.** Plain sentences reached 2.51 ("Okay, I still don't love the UI for taskboard") and 2.59. The labelled excited takes start at 2.96.
- **Redictations teach expression learning, stricter only.** A report from dictating again (in terminals, where edits can't be read) can take back a stretch or a "!", never add one.

## Labelled set (2026-10-05)

The user's 20 test dictations, captures created 2026-10-05 19:35:09–19:36:30 in their local database, in this order: Perfect (flat, excited), That's great news (flat, excited), We finally shipped it (flat, excited), This is way better than before (normal, "waaay"), It took five minutes (normal, "fiiive"), That was so good (normal, "sooo"), hesitant "The… uh… meeting moved to Friday", Commit and push, Delete that file, Can you send it today?, I can't believe that actually worked (excited), Okay, sounds good (flat, happy), and three sentences with only the middle one excited. Replay: `scripts/replay-expression.py --since "2026-10-05 19:35"` (baseline from the 80 dictations before).

Results:

- **Energy works for "!"**: the 5 excited takes scored 2.96–4.07. Everything else scored 2.03 or below: flat, normal, the question and the hesitation, the commands (0.88, 1.25) and the stretched takes. The flat "Perfect." was too short to measure (under 10 voiced frames). A threshold near 2.5 separates them.
- **Mixed take**: replayed whole, Whisper wrote it as one unpunctuated sentence, so there was no per-sentence split. In the app, earlier phrases' context made it three sentences. Phase 2 should place sentences from the cleaned text, not from Whisper's punctuation alone.
- **Stretched words: 0 of 3 caught, by design.** The user decided (2026-10-05) that the rule stays strict: not everyone wants stretched words. It adapts from edits instead. Someone who types "wayyy" over their own text teaches Kass their stretches (phase 4); everyone else never gets them. What held these 3 back is the evidence for that learning:
  - The pace was taken from the recording itself, and a short take has few words.
  - "so" is on the closed-class list.
  - "five" was 1.4 dB louder, under the 1.5 dB the rule needs.
  - Attention gave part of "waaay" to "better".
  Don't loosen the defaults to catch these.
- **False alarms, 150 ordinary dictations** (2026-10-04 to 10-05):
  - No sentence reached the "!" cutoff: 0 of 198 scored 2.5 or higher.
  - The stretched-word rule fired once, wrongly: "UI" as "UIIII". Initialisms (all capitals) are now never stretched, which leaves 0.

## Checks

- `after_release_*` timings must not grow (A/B on saved dictations). Blocks shipping.
- A labelled set of ~20 user recordings (the user agreed 2026-10-05 to record them) (stretched / excited / flat / emphatic command) must pass.
- Saved-capture replay: report every output change for review before each phase ships.
