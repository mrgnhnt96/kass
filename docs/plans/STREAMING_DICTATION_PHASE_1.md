# Streaming dictation: phase 1 implementation

This implements the first phase of [the live dictation plan](LIVE_DICTATION_AND_VOICE_CODE.md). Recognition and refinement run during recording; delivery remains one final paste after stop. Live editing and voice-to-code are separate, unimplemented phases.

## Flow

The existing microphone stream feeds both an AudioWorklet PCM sidecar and the existing complete MediaRecorder recording. The sidecar sends mono PCM16 at the AudioContext's actual sample rate. Keeping the original recording allows batch fallback if streaming setup, transport, or inference fails before finalization.

The backend processes bounded rolling windows through the installed Whisper backend. It accepts phrases at pauses and refines accepted phrases. All model work still respects the existing single MLX worker. Provisional previews and speculative refinement were removed (2026-09-24): no client displayed them, speculative results were never reused on the benchmark fixtures, and model work can't be interrupted, so a preview still running at release delayed the final result (about 0.3 s on a 2 s take).

Streaming is attempted automatically for dictation. It is independent of a future live-insertion toggle. The recording pill labels batch fallback when the streaming path is unavailable. No partial text is pasted into the target application in this phase.

## Protocol version 1

Connect to `WS /captures/stream` on the configured server. Browser origins must match the HTTP CORS allowlist, including `KASS_CORS_ORIGINS`; originless native clients must connect over loopback. CORS itself does not protect WebSocket handshakes.

Send a JSON start object:

```json
{
  "type": "start",
  "protocol_version": 1,
  "sample_rate": 48000,
  "channels": 1,
  "encoding": "pcm_s16le",
  "source": "dictation"
}
```

The server snapshots capture settings and responds with `ready`, a server-generated `session_id`, `auto_refine`, and `allow_auto_paste`. Optional language/STT overrides are validated by the backend. Supported sample rates are 16–48 kHz.

Each binary message contains an eight-byte header followed by mono signed little-endian 16-bit PCM:

| Bytes | Meaning |
| --- | --- |
| 0–3 | Little-endian unsigned frame sequence, starting at zero. |
| 4–7 | Little-endian unsigned sample offset, starting at zero. |
| 8 onward | PCM samples. |

Offsets and sequence numbers must be contiguous. The entire message must be at most 65,536 bytes. The frontend bounds queued socket data to 1 MiB; the backend bounds pending audio to 60 seconds, active recognition windows to 20 seconds, sessions to one hour, and concurrent sessions to two. An overloaded stream falls back instead of silently dropping audio.

Updates carry `session_id`, a monotonic `revision`, and `covered_samples`:

- `transcript`: complete `text`, `accepted_text`, `provisional_text`, and `final: false`. Sent when a phrase is accepted; `provisional_text` is currently always empty.
- `refined`: the current proposed output in `text`. It may still change.
- `final`: `capture` in the existing capture-create response shape, `refinement_complete: true`, optional `refinement_error`, and optional `degraded_reason`.
- `error`: a failure message; unsuccessful sessions do not persist a partial capture.

After flushing the worklet's final PCM frame, send `{"type":"finish"}` once. It is a commit request: the backend completes finalization and persists one WAV and capture even if the socket disconnects afterward. The client delivers the already-refined result without calling the refinement endpoint again. Empty refined output is meaningful and must not fall back to raw text.

Before finish, cancel/disconnect discards the streaming session. The complete MediaRecorder recording can then use the existing upload path. After finish, do not retry a batch upload: persistence may already have succeeded. Recover through `GET /captures/stream/{session_id}/result`, which returns the final event, HTTP 202 while processing, or HTTP 404 when unavailable. Completed results are cached for ten minutes, capped at 32 entries. The capture itself remains in Captures after cache expiry; a server restart does not preserve the transient recovery cache.

## Accuracy and latency limits

Phrase boundaries currently use a conservative energy/pause heuristic, not a separately trained voice-activity model. There is no partial text before the first pause; the original plan's 0.5–1.5-second first-partial target is not pursued, because live text isn't shown and any preview can delay the final result.

Uninterrupted speech that reaches a forced 20-second window boundary receives a final complete-audio transcription/refinement pass. This avoids trusting text-only overlap matching for final output: repeated words and changes in recognition can otherwise duplicate or delete content. `degraded_reason` makes that final batch pass explicit. Timestamp-based boundary handling is follow-up work, not a claimed capability.

The main latency benefit is expected for speech with phrase pauses, where earlier phrases finish while recording continues. Short uninterrupted dictations may show little benefit. Cold model loading and TTS contention still add delay.

Existing deterministic spoken corrections are applied against session text so later phrases can revise earlier output. Phrase-wise generative refinement is not guaranteed identical to whole-recording refinement; benchmark both final text and timing before broadening quality claims.

Isolated phrase refinement additionally checks ordered content tokens. It accepts punctuation/case changes and configured disfluency cleanup, but preserves the prepared raw phrase if the model drops, adds, or replaces content. For example, the small model removed “add a reminder to” from one test phrase; that result is now rejected. This conservative policy may retain wording a whole-session cleanup would improve. Deterministically resolved edits remain supported; ambiguous cross-phrase corrections can require a final whole-session refinement pass without repeating transcription.

## Verification

Automated checks cover origin policy, PCM framing/bounds, session lifecycle, intermediate processing, cancellation, finalization/recovery, and the existing dictation/correction behavior. Model replay evidence is recorded separately by the benchmark harness; synthetic recordings avoid exposing personal captures.

A real macOS WKWebView smoke test used the production `streamingAudio.ts` module with synthetic oscillator audio. It produced 57,600 nonzero PCM samples in 29 frames at 48 kHz over approximately 1.2 seconds, flushed successfully, and emitted no further samples after stop. This verifies AudioWorklet operation in WebKit on localhost; it does not by itself verify microphone permissions or an installed Tauri hotkey-to-paste session.

Do not conflate source tests, a frozen backend socket check, and an installed desktop microphone test. Report each separately with the build/model configuration and any remaining gaps.

### Source and transport checks (2026-09-17)

- 114 focused backend tests cover streaming, origins, and existing correction/refinement regressions. The streaming set includes real database initialization during application lifespan; it catches capturing an uninitialized session factory at route import time.
- 25 frontend tests pass, including final-result recovery, cancellation, empty output without native paste, delayed WAV conversion, and overlapping takes. App/web typechecks and both web/desktop Vite builds pass. The separate Tauri TypeScript project still reports existing window-global, virtual-changelog, and dependency declarations outside this feature.
- A running source server with offline cached Whisper turbo/Qwen3 0.6B accepted an 11.167-second synthetic recording over a real WebSocket. It emitted five transcript updates and one refinement before stop, produced the expected text, saved one capture, and returned that same capture through final-result recovery. Stop-to-response was approximately 0.84 seconds in this smoke test.
- The transport rejected an untrusted browser origin. Canceling a second stream during inference left no extra capture or orphan WAV.
- Detailed model comparisons and limitations are in [the benchmark report](STREAMING_BENCHMARK.md).

### Frozen server and desktop build

The rebuilt PyInstaller server passed the same real WebSocket recording check with offline cached models: five transcript updates and one refinement before stop, expected final text, one capture, origin rejection, and recovery all passed. The measured final wait was approximately 0.81 seconds for this synthetic short recording. A separate connection closed immediately after sending finish; polling recovered exactly one additional persisted capture, demonstrating recovery without a duplicate upload.

The updated macOS bundle was built at `app/target/release/bundle/macos/Kass.app`, signed with the available local development identity, and passed deep/strict code-signature verification. That initial validation did not replace the installed app. The user subsequently requested installation, and `/Applications/Kass.app` was replaced and relaunched with a healthy backend. An actual installed-app microphone/hotkey-to-paste session remains a separate hands-on check; synthetic WebKit capture and packaged server inference do not establish that entire interaction.


## Startup and empty-result fixes

The first implementation awaited AudioWorklet setup before starting MediaRecorder.
Recording now starts immediately after microphone acquisition. The dictation
session prepares and reuses the worklet module and AudioContext ahead of time,
without opening the microphone. If the processor is cold or suspended at capture
start, that take uses the complete archival recording instead of finalizing PCM
that could omit its first words. A sidecar that resolves after stop or unmount is
canceled without delaying completion.

An empty streaming result could finish within one React render batch. Completion
previously checked a pill-state ref that still said “recording”, then left the
newly queued “transcribing” state visible indefinitely. Completion now checks the
recording identity; empty output clears the pill without pasting and old results
cannot hide a newer recording.

Both failures were reproduced in regression tests before fixing them, including
React concurrent rendering for the empty-result race. The focused frontend suite
now passes 28 tests, with typecheck and scoped lint passing. A production-module
WKWebView smoke test also verifies PCM capture, flush, and reuse of a prepared
context across takes. Physical microphone startup still depends on device access;
the existing opt-in “keep microphone ready” setting avoids reopening the device.
