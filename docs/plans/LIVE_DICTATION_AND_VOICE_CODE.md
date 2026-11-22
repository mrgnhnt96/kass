# Streaming dictation and live voice-to-code

Status: phased implementation plan. The initial phase 1 implementation and its
limits are tracked in [streaming dictation phase 1](STREAMING_DICTATION_PHASE_1.md);
phases 2 and 3 remain planned.
Date: 2026-09-17.

## User requirements

1. Transcribe and refine while the user speaks, reducing the work remaining after recording stops.
2. Provide an option to enable or disable live dictation into the focused text element, including modifying text already inserted during the session.
3. Produce literal source code from speech and improve that behavior with training on recordings and intended code output.
4. Show code changing in the target application while the user is speaking. An audio → transcript → code pipeline is acceptable; an end-to-end audio model is not required.
5. Require neither an IDE nor an editor extension. Syntax highlighting is outside scope.

The visible outcome is central: a transcript in Kass's overlay alone does not satisfy live voice-to-code. A supported ordinary text field must show code appearing and being revised before recording stops.

## Current foundation

These are observations of the working tree when this plan was written, including work not yet committed. Agents must inspect current files before editing and preserve unrelated changes.

- `app/src/lib/hooks/useAudioRecording.ts` collects a complete recording, converts it to WAV, and invokes its completion callback. It deliberately avoids MediaRecorder timeslices because of a documented WebKit container-header issue. Simply enabling a timeslice is not an established streaming solution.
- `app/src/lib/hooks/useCaptureRecordingSession.ts` starts capture upload/transcription after recording completes, optionally refines, then delivers final text.
- `backend/backends/__init__.py` defines an STT interface that accepts an audio path and returns a complete string. The MLX implementation in `backend/backends/mlx_backend.py` runs inference on the shared MLX worker.
- `backend/services/captures.py` persists audio and completed transcripts. `backend/services/refinement.py` performs whole-transcript cleanup; deterministic spoken corrections and structural edits also exist.
- `app/src/main.rs` implements `paste_final_text` using focus capture, clipboard staging/restoration, and synthetic paste. This is not yet a verified arbitrary-range editor.
- `backend/services/model_improvement/` contains a local refinement-adapter training and speech-model evaluation workflow. Existing speech evaluation is not speech-recognition weight training.

Related plans: [spoken corrections](SPOKEN_SELF_CORRECTIONS.md), [spoken text edits](DICTATION_TEXT_EDITS.md), [model improvement](MODEL_IMPROVEMENT.md), and [permissions and latency](DICTATION_PERMISSIONS_AND_LATENCY.md). Preserve their existing behavior unless a change is explicitly designed and covered by regressions.

## Architecture and behavioral boundaries

Use independently testable stages:

`microphone frames → streaming recognition → prose refinement OR code conversion → versioned output → native text delivery`

Keep ordinary dictation and code mode distinct. Existing prose cleanup must not silently reformat or paraphrase source code. Live insertion is an independent setting: either mode can process continuously while delivering only once at the end.

Initial implementation targets macOS desktop. Define portable interfaces and document Windows/Linux gaps; do not claim support without native validation. Web-only Kass cannot be assumed to have desktop-wide editing capabilities.

Recognition distinguishes an accepted prefix from a provisional ending. Accepted recognition is not necessarily immutable output: an explicit later correction can request a revision of earlier session text. Ordinary inference updates should revise only the provisional region. Intentional corrections use a separate, explicit revision operation.

The initial editable scope is the current dictation's inserted text, plus a selection captured and verified at session start if selection replacement is supported. Editing unrelated pre-existing document text by spoken navigation is a later feature. Never interpret “modify existing text” as permission for unbounded document rewrites.

## Phase 0 — contracts and feasibility checks

Complete these short investigations before parallel implementation locks in assumptions:

1. Measure the existing warm/cold audio → transcription → refinement → visible-paste path on a named machine and model configuration.
2. Benchmark rolling-window recognition using the installed stack, including overlapping audio and transcript agreement. Compare another streaming-capable backend only if the existing approach misses the budget or quality gate. Record dependency, packaging, memory, and license implications before choosing it.
3. Probe native read/selection/range-replacement capabilities in representative macOS fields: TextEdit plain text, a browser textarea, VS Code's editor, and a terminal input. These are test targets, not promises of equal support.
4. Specify session/event/native-edit contracts and fixtures before downstream agents build against them.

A native capability failure must be surfaced early. If VS Code cannot safely support revisions without an extension, document the limitation and investigate platform mechanisms; do not silently replace the requirement with an overlay or claim append-only text is equivalent.

### Proposed shared contracts

These names describe the intended contract, not existing APIs. The integration owner freezes exact schemas and locations in phase 0.

| Boundary | Required information and rules |
| --- | --- |
| Session start | Session ID, protocol version, prose/code mode, language, live-delivery flag, model/settings snapshot, audio format; native target identity remains in the native delivery layer unless explicitly needed elsewhere. |
| Audio frames | Monotonic sequence and sample offset, sample rate, channels, encoding, bounded frame size. Negotiate format once; a candidate baseline is mono 16 kHz PCM. |
| Recognition update | Session ID, monotonic revision, covered audio offset, accepted prefix, provisional ending, final flag. Full snapshots initially simplify reconciliation. |
| Converted output | Session ID, output revision, source recognition revision, complete desired session text, mutable range, and revision reason: provisional update, phrase acceptance, or explicit correction. |
| Delivery request | Session ID, target token, expected last-applied revision/text, desired replacement text. Convert Unicode offsets at the platform boundary; never mix byte, code-point, UTF-16, and accessibility ranges. |
| Delivery result | Applied, stale, conflict, unsupported, or failed; applied revision and verified text when readable. A sent keystroke alone is not proof the target changed. |
| Session end | Finish, cancel, disconnect, or error, plus last acknowledged audio/output revision. Final delivery is idempotent and must not duplicate already inserted text. |

Empty output is valid: “remove that” can delete the session's entire inserted range. It must not be discarded by a truthiness check. Keep capture persistence separate from per-update inference; do not create a capture row/file for every partial transcript.

Use a session transport suitable for bidirectional audio and events, provisionally a WebSocket. Reuse the application's existing authentication/origin policy; bound session count, input size, queues, and idle lifetime. Do not expose a new unauthenticated network service. Reconnection must not replay text edits blindly.

## Phase 1 — streaming recognition and incremental refinement

Deliver this phase with live insertion disabled. The existing end-of-session paste remains the user-visible delivery behavior.

- Capture continuously decodable audio frames, using an AudioWorklet or another measured native/browser capture path. Preserve microphone selection, warm-mic behavior, hotkeys, cancellation, and archive-quality audio.
- Add a streaming backend capability without breaking the uploaded-file STT API. Bound audio windows and inference backlog; coalesce obsolete partial inference work while preserving audio required for finalization.
- Use overlap/context and agreement to avoid duplicate or missing words at chunk boundaries. Handle silence, short utterances, language hints, and the final audio tail.
- Refine accepted phrases while speech continues. Keep bounded context and avoid restarting whole-session refinement on every word. Preserve existing explicit self-correction behavior across phrase boundaries through deliberate revisions.
- Schedule recognition, refinement, TTS, and background training deliberately. Async tasks do not imply concurrent MLX inference; measure contention and prioritize foreground dictation.
- Persist one coherent capture containing audio, raw transcript, and final output. Keep the existing batch path as a fallback when streaming is unavailable, with an explicit degraded state.

Acceptance:

- A long recording produces recognition and refinement events before stop; finalization drains the last frame exactly once.
- Live-off produces a single final insertion with no missing/duplicated text.
- Pauses, rapid start/stop, cancellation, disconnect, model errors, and consecutive sessions do not mix audio or output.
- Existing dictation/correction regressions pass. Compare accuracy on the same fixed recordings against batch transcription, including identifiers and numbers.
- Report median/P95 first-partial latency, phrase-finalization latency, stop-to-final-output, memory, and inference backlog. Demonstrate lower warm stop-to-output latency than the baseline on longer recordings without hiding a large last-phrase delay.

## Phase 2 — optional native live insertion and revision

Add the setting only after the delivery capability is known. Build a native session abstraction that owns the target and inserted range; do not repeatedly invoke final-paste as though it supports replacement.

Delivery capability levels:

| Capability | Behavior |
| --- | --- |
| Verified range editing | Insert provisional output and safely revise the session-owned range after checking target/text identity. |
| Verified append only | Insert accepted phrases; display provisional output in the overlay. Explain that in-place revisions are unavailable. |
| Final paste only | Continue streaming internally and deliver at stop through the existing path. |
| Unavailable | Keep output in Captures and show the delivery failure. |

Native requirements:

- Track application, actual focused element, selection, session-owned text, and last applied revision. Process identity alone is insufficient.
- Serialize edits and reject out-of-order updates. Verify the expected target/range before replacement and acknowledge only what was applied or observably confirmed.
- On manual typing, cursor movement, target closure, or focus change, pause live mutation and reconcile or end delivery explicitly. Do not repeatedly steal focus to keep inserting.
- Preserve pre-existing text and clipboard contents. Do not use blind backspace counts to undo prior output.
- Define start-with-selection, multiline/indentation, Unicode, and empty-output behavior. Probe undo behavior in real apps; do not promise cross-app atomic undo before verifying it.
- Stop finalization reconciles the latest output against the applied revision, rather than appending the full transcript again.
- Cancel stops further processing/delivery. Removing already inserted text is allowed only while ownership and unchanged target content remain verifiable; otherwise retain it and explain the result.
- Exclude secure fields. Treat terminal/console controls as restricted until multiline delivery is shown not to execute commands inadvertently; writing code is not authorization to run it.

Acceptance:

- A supported ordinary text field shows changing text during speech with live mode on; live mode off retains one final insertion.
- Demonstrate partial replacement, spoken correction of an earlier session phrase, complete deletion, selection replacement where supported, multiline code-shaped text, and final reconciliation.
- Concurrent manual edits, clipboard changes, and focus switches cannot overwrite unrelated text.
- Publish an app/control capability matrix backed by installed-build checks, including VS Code and at least one non-IDE text field. Unsupported cases must visibly degrade as documented.

## Phase 3 — live code conversion and model training

### 3A. Working code mode and data collection

First ship a measurable audio → transcript → code baseline through phase 2 delivery. Default the initial language to TypeScript for this repository, with an explicit language selector; add other languages after validation. This is an implementation starting assumption, not a user restriction.

- Define literal punctuation, casing, indentation, newline, block, and correction commands. Define how users dictate the command words literally.
- Maintain evolving code state across phrases. Incomplete code while speaking is expected; do not require every partial output to parse.
- Render converted code while speaking, including revisions. A converter that waits until stop fails this phase's central requirement.
- Provide bounded context from the current session and explicit settings. No extension means project symbols/imports/full-document context are not automatically available. Use readable selected context only if separately designed and exposed to the user.
- Distinguish literal code dictation from optional instruction-to-code generation. Implement literal dictation and corrections first; broader requests such as “write a function that…” are a separately evaluated mode.
- Collect explicitly accepted or corrected examples locally under the existing recording/data controls. Do not label arbitrary subsequent user edits as ground truth automatically.

Each example should retain recording reference/hash, transcript, intended code, language, relevant context/settings, model versions, command interpretation, and user-confirmed correction. Where available, retain timing-aligned phrase/code revisions to evaluate live behavior, not just the final string.

### 3B. Targeted training

- Train transcript-to-code conversion adapters using intended code output; evaluate them through the complete recorded-audio pipeline.
- Keep speech-recognition errors separate. Fine-tuning recognition requires audio paired with faithful spoken transcripts, which may differ substantially from intended code. Audio/code pairs alone are not interchangeable with ASR supervision.
- Reuse the existing local trainer's isolation, foreground preemption, split discipline, evaluation, promotion, and rollback mechanisms where suitable. Audit assumptions such as prose-specific checks and input-length limits before reuse.
- Keep code adapters separately selectable/versioned so code training does not silently change normal prose dictation.
- Group duplicate/related recordings and code templates before train/validation/test splitting. Keep an independent fixed acceptance set; record provenance and exclude sensitive content from datasets according to user settings.
- Do not promise gains from a specific small dataset size. Establish eligibility thresholds and learning curves from actual results.

Acceptance:

- Dictate and correct small TypeScript blocks in a supported non-IDE text field, watching code update before stop, without an extension.
- Evaluate exact intended output, identifiers/literals/operators, indentation, parse validity of completed examples, spoken-command accuracy, correction success, output revision churn, and end-to-end latency.
- Evaluate prose regressions separately. A syntactically valid but wrong program does not count as correct; do not execute arbitrary dictated code for validation.
- A trained candidate is promoted only after held-out improvement and defined latency/memory/regression gates; failure retains the previous model and a readable report. Verify rollback.

## Multi-agent execution plan

This section authorizes a future implementation coordinator to delegate bounded work. Writing this plan does not start implementation. Run at most three implementation agents concurrently plus a coordinator where the environment has four slots.

The coordinator must tell every worker: you are not alone in the codebase; preserve other agents' and the user's edits, and accommodate changes already present. Inspect `git status` first. Do not reset or broadly reformat the working tree.

| Owner | Responsibility and file ownership | Dependencies |
| --- | --- | --- |
| Coordinator / integration | Freeze contracts and acceptance fixtures; own cross-layer schemas/settings, application route registration, and edits to shared Rust command registration in `main.rs`. Integrate and resolve interface changes. | Phase 0 first. |
| Audio/frontend agent | `useAudioRecording.ts`, `useCaptureRecordingSession.ts`, new streaming client/audio modules, associated frontend tests, and live-mode/pill UI after settings contract freezes. | Contract fixtures; can use fake streaming server. |
| Recognition/backend agent | New streaming route/service/backend modules, STT backend interfaces/implementations, streaming refinement scheduling, capture persistence integration, backend tests. | Contracts; coordinate changes to shared refinement code. |
| Native delivery agent | New native delivery module, `focus_capture.rs`, `clipboard.rs`, `synthetic_keys.rs`, native platform bridge and delivery tests. Send minimal registration changes to coordinator. | Native feasibility probe; fake versioned output supports independent work. |
| Code/training agent, later wave | New code-conversion modules and code-specific fixtures/dataset/adapters; `backend/services/model_improvement/` changes. Shared refinement integration remains coordinator-owned once backend work lands. | Phase 1 stable; baseline converter can proceed alongside phase 2 after a slot opens. |
| Verification agent, milestone wave | Read-only review and dedicated integration/benchmark tests with explicit ownership assigned by coordinator. Run installed-app checks and report evidence/gaps. | Replaces a completed worker; must not compete for the same microphone/GUI with native agent. |

Execution order:

1. Coordinator freezes contracts while audio/backend worker benchmarks recognition and native worker probes compatibility. Keep shared-file ownership explicit.
2. Implement phase 1 across frontend/backend using fixtures. Native work can continue independently against fake output.
3. Integrate and validate live-off streaming, then phase 2 live delivery. Do not postpone real native-app testing until all features are built.
4. Begin code baseline/data collection once session/output contracts stabilize. Validate real-time code delivery before committing to training architecture.
5. Add training, evaluate held-out recordings, and release only capabilities actually verified.

Each agent handoff must include changed files, contract changes, checks run/results, reproducible manual steps, observed timings/hardware/models, known limitations, and remaining work. No agent should mark a phase complete based on mocked delivery alone.

## Measurement and release gates

Provisional UX goals, to be accepted or revised from phase 0 measurements:

- Warm first provisional transcript: roughly 0.5–1.5 seconds after sufficient speech arrives.
- Target mutation: within 250 ms of an eligible converted-output update under normal load.
- Warm stop-to-final-output: target median under 1 second and P95 under 2 seconds on the named reference setup.

These are targets, not current performance claims. Measure audio onset → first visible code separately; fast transcript events can conceal slow code conversion. Report cold loading separately and test under representative TTS/model contention. Record revision frequency, maximum rollback length, and text lost/duplicated, not latency alone.

Required scenario set: silence, short taps, long speech, mid-word pauses, technical identifiers, non-ASCII text, phrase-boundary corrections, empty final output, fast successive sessions, model failure, disconnect, slow consumers, target changes, manual edits, clipboard contention, unsupported controls, and packaged desktop operation.

Completion means documented behavior and passing evidence for each phase, an honest native compatibility matrix, and a visible live-code demo in a non-IDE field. Remaining platform/model limitations belong in the release documentation rather than being hidden behind the live-dictation toggle.
