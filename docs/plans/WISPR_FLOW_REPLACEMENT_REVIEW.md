# Kass as a Wispr Flow replacement

Reviewed September 16, 2026, against checkout `51f49de`.

## Recommendation

Kass has a credible local desktop dictation foundation. Prioritize delivery reliability, recovery, and personal vocabulary before adding more model choices. Treat it as a candidate for a daily-driver trial, not yet a demonstrated drop-in replacement.

This is a source review and research comparison with current official Wispr documentation. No microphone trial, app compatibility test, accuracy benchmark, or latency measurement was performed. Failure scenarios below follow from code; their frequency is unknown. Feature absences mean no implementation was found in the reviewed paths and repository searches.

## Existing strengths

- Global push-to-talk and toggle-to-talk, configurable left/right modifier chords, a floating recording surface, and native paste integration.
- Local Whisper transcription and Qwen refinement, including an Apple Silicon MLX path. Capture settings default to Whisper Turbo and Qwen 0.6B.
- Audio history, original and refined text, retranscription, and manual refinement.
- Clipboard restoration includes a change-count check to avoid overwriting something the user copied during insertion.
- Local inference offers an offline alternative after models and dependencies are available. Verify cold-start airplane-mode behavior before making an unconditional offline guarantee.

Sources: `docs/content/docs/overview/dictation.mdx`, `backend/database/models.py`, `backend/backends/mlx_backend.py`, `backend/services/captures.py`, `app/src/main.rs`.

## Findings and improvements

### P0: Bind every result to its recording session and destination

`DictateWindow.tsx:42` holds a single mutable `focusRef`. Each new recording overwrites it; the next completed transcript consumes it. `useCaptureRecordingSession.ts:281` prevents starting only while recording, not while transcribing or refining. An earlier result can therefore consume a later recording's destination, leaving the later result without its destination.

Use an immutable session ID containing its focus snapshot, settings, audio, and eventual transcript. Serialize delivery and make it idempotent. Initially block new starts while busy or queue sessions explicitly. Cover A-start, A-stop, B-start, A-completes with a regression test.

### P0: Original application does not mean original field

`focus_capture.rs` stores PID, bundle ID, and role, but no stable window/control identity or selection. Windows reactivation chooses the first matching visible top-level window for the PID. `main.rs:1216` activates the PID and synthesizes paste without validating a text field. Documentation promising the original field is stronger than this implementation.

Store window/control identity where supported, validate it before delivery, and avoid automatic insertion when the destination is ambiguous or has changed. Offer Copy/Insert explicitly in that case. Test two browser windows, multiple fields in one window, app switching, closed destinations, terminals, and custom editors. Update the documentation to describe best-effort behavior until stronger guarantees exist.

### P0: Preserve speech when processing fails

`backend/services/captures.py:122` transcribes before committing a capture row. On failure, the exception path deletes the audio. `useCaptureRecordingSession.ts:200` displays refinement errors without delivering the existing raw transcript. Paste errors are mostly console warnings in `DictateWindow.tsx`, while the session can already show completion.

Create a durable pending capture first, transition through recorded/transcribing/refining/ready/delivered/failed states, and support retry. Preserve audio locally if upload fails. On refinement failure, offer the raw transcript or use an explicit user-configured fallback. Distinguish transcription success from insertion success and provide a visible Copy/Retry action. Respect retention settings for failure recovery too.

### P1: Handle microphone startup and cancellation as explicit states

`useAudioRecording.ts` awaits `getUserMedia` before setting `isRecording`; `DictateWindow.tsx` ignores stop events when `isRecording` is false. Releasing the chord during initialization can lose the stop request and subsequently start a recording. The pill enters recording before microphone readiness. Clips shorter than 0.5 seconds are rejected regardless of whether they contain a useful one-word answer.

Add starting/recording/stopping/canceled states, latch stop requests during initialization, and cancel stale asynchronous starts. Signal when the microphone is actually ready. Evaluate a native capture path or reusable microphone stream only after measuring startup delay and privacy/battery tradeoffs. Use speech detection to distinguish empty taps from brief speech.

### P1: Make cleanup controls truthful and preserve meaning

`backend/services/refinement.py:140` always instructs filler removal and punctuation. Disabling smart cleanup removes only an additional prompt section. Disabling all flags adds a contradictory pass-through instruction while still running the LLM with cleanup examples. The output budget is fixed at 2,048 tokens, creating a truncation risk for long recordings. No output completeness validation was found.

Implement true verbatim bypass, condition-specific prompts/examples, and safe long-text chunking. Detect incomplete output and expose the raw version rather than silently accepting it. Test negation, numbers, dates, quoted instructions, meaningful filler-like words, technical identifiers, and explicit self-corrections. Measure accuracy separately from refinement quality.

### P1: Add vocabulary and destination-aware formatting

No dictation dictionary, spoken snippet expansion, correction-learning workflow, or surrounding-text formatting pipeline was found. Preserving technical terms in a prompt cannot recover an unknown project name that transcription already misrecognized.

Add a local editable dictionary with spoken aliases and preferred spelling; use ASR hints where the backend supports them and carefully scoped substitutions afterward. Let users save a correction explicitly. Add deterministic snippets and modes such as Verbatim, Clean prose, Chat, and Technical. Later, add opt-in local context from nearby text for spacing, capitalization, recipient names, and identifiers. Exclude sensitive fields and keep context transient.

### P1: Measure and shorten release-to-insertion delay

The current flow finishes recording, converts audio, uploads the whole clip, transcribes, refines, and then pastes. There is no incremental dictation result in this path. Model readiness checks disk cache rather than warm inference readiness. Do not confuse documentation latency anecdotes with measurements on this machine.

Instrument microphone-ready time, conversion, transcription, refinement, and insertion separately. Warm selected models when appropriate; skip optional refinement when disabled. Benchmark local backends before replacing Whisper or enlarging the refinement model. Use voice activity detection and incremental processing if measurements justify them.

`useDictationReadiness.ts:88` also requires the LLM to be downloaded even when auto-refine is disabled. Make that dependency conditional.

### P2: Add retention controls and keep the product focused

Successful captures persist audio and transcripts; manual deletion exists, but no automatic retention or no-history mode was found. Add configurable expiration, audio-only deletion, and an ephemeral mode. A local archive still needs user control.

Offer a dictation-focused onboarding path with a microphone test, permission checks, one recommended model preset, and a sample insertion. Mobile keyboards and Linux global insertion are separate substantial projects; prioritize them only if required for the replacement workflow.

## What Wispr Flow supplies today

- Personalization: [Context Awareness](https://docs.wisprflow.ai/articles/4678293671-Context-Awareness) uses nearby text and app/site identity for names, spacing, capitalization, and style. Platform support differs.
- Productivity: [snippets](https://docs.wisprflow.ai/articles/5784437944-create-and-use-snippets), dictionaries, [smart formatting and backtracking](https://docs.wisprflow.ai/articles/5373093536-how-do-i-use-smart-formatting-and-backtrack), and [custom transforms](https://docs.wisprflow.ai/articles/2719941210-how-to-configure-polish-shortcuts-and-custom-prompts).
- Editing: paid [Command Mode](https://docs.wisprflow.ai/articles/4816967992-how-to-use-command-mode) supports desktop editing; it remains experimental and its documentation acknowledges silent failed edits. Do not treat Flow as a flawless baseline.
- Tradeoffs: [Flow requires internet for transcription](https://docs.wisprflow.ai/articles/4048537120-what-to-expect-from-flow-accuracy-and-known-limitations?lang=nl). Its [data controls](https://wisprflow.ai/data-controls) distinguish cloud storage and model-improvement choices from processing. Privacy settings do not make it a local inference product.
- Cost: [pricing](https://wisprflow.ai/pricing) lists Pro at $15/user/month monthly or $12/user/month billed annually. Kass removes that subscription dependency but consumes local compute and requires model setup and maintenance.

## Implementation order and replacement acceptance test

### Existing upstream work discovered after the initial review

Checked 173 open upstream PRs and the fork's empty open PR queue on September 16, 2026. These are integration candidates, not tested merge approvals.

- [#929](https://github.com/jamiepine/voicebox/pull/929), the draft v0.6 release PR, directly addresses several findings: per-capture delivery context replaces shared focus state, early stop requests are forwarded and latched, microphone acquisition is coalesced, and microphone warming is opt-in. Its focused commits include `0070c04bcfd4` (serialized MLX lifecycle), `9b1beba3e159` (microphone lifecycle), and `7e424a20a372` (focus/fullscreen injection). Verified in the diff. Extract and review this work rather than integrating the whole conflicting release PR. It still does not establish exact-field identity or recoverable transcription failure storage.
- [#1013](https://github.com/jamiepine/voicebox/pull/1013): directly fixes conditional LLM readiness. Three files, +22/-10; patch applies cleanly to this checkout.
- [#1014](https://github.com/jamiepine/voicebox/pull/1014): persistent microphone selection with default-device fallback. Patch applies cleanly individually; reconcile with #929's recorder rewrite.
- [#848](https://github.com/jamiepine/voicebox/pull/848): macOS fullscreen/focus and MLX thread fixes. Overlaps #929; its patch does not apply cleanly to this checkout despite GitHub reporting mergeability against upstream main.
- [#616](https://github.com/jamiepine/voicebox/pull/616): small PyTorch long-form Whisper fix. Patch applies cleanly; does not change the Apple Silicon MLX path. #832 and #1033 offer overlapping chunking work, so choose one approach.
- [#921](https://github.com/jamiepine/voicebox/pull/921): custom refinement system prompt. Patch applies cleanly, but existing few-shot examples remain, so this is not a complete writing-mode or verbatim implementation.
- [#1037](https://github.com/jamiepine/voicebox/pull/1037): draft single-instance/shutdown fix for duplicate global hotkeys.
- [#943](https://github.com/jamiepine/voicebox/pull/943): draft, conflicting multilingual refinement work; supersedes #629 according to its description.
- [#660](https://github.com/jamiepine/voicebox/pull/660): conflicting recorder/sample-ordering bundle. Its stale-completion suppression can discard a previous recording once a newer session begins; prefer #929's per-take context approach for dictation.

No dedicated open PR was found for dictation vocabulary/snippets, automatic capture retention, durable failed-transcription recovery, or exact-field insertion validation. #1025 is a TTS pronunciation dictionary, not an STT correction dictionary. Most shortlisted PRs expose only a CodeRabbit status, not substantive build/test CI; clean patch application does not establish runtime correctness. No PRs were merged or applied.

1. Session ownership, destination validation, durable retry, and honest delivery status.
2. Microphone lifecycle, real verbatim mode, conditional LLM readiness, retention controls.
3. Personal dictionary, explicit correction saving, snippets, and writing modes.
4. Measured latency improvements, opt-in context, then selected-text transforms.

Evaluate 100 representative utterances in the user's actual apps: short replies, long prompts, project names, code paths, dates/numbers, corrections, pauses, and background noise. Compare both tools on identical audio where feasible; evaluate hotkey and insertion behavior separately with live use. Include restart/offline, fast release, rapid consecutive dictations, focus changes, model failures, and clipboard changes.

Suggested release gates, not current measurements: zero wrong-target or duplicate insertions in the scripted suite; all failed sessions recoverable subject to retention choice; no silent long-text truncation; at least 95% of routine utterances usable without correction; warm release-to-insertion p95 under two seconds for 5–15-second utterances on the target machine. Record hardware, models, language, cold-start behavior, memory, and battery impact. Adjust the latency target from actual baseline measurements.
