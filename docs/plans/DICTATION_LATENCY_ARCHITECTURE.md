# Dictation latency: architecture review and work plan

Date: 2026-09-24. Status: approved for implementation. Machine for the
evidence below: Apple M2 Max, macOS 26.6, Whisper large-v3-turbo on MLX, Qwen3
4B (4-bit MLX) cleanup, "learned" punctuation style.

## Goal

Text should be ready to paste within about one second of releasing the
hotkey, and every word spoken after the key goes down must be captured.
**Nothing may add time after release.** Speed is the product. Any change that
makes the post-release wait longer is rejected, even if it fixes something
else. This includes fixes such as "re-transcribe the full recording at the
end".

## How dictation works today

```
key down (Rust, hotkey_monitor.rs)
  → Tauri event "dictate:start" → webview (DictateWindow.tsx)
  → WebKit getUserMedia opens the mic
  → two copies of the audio:
      MediaRecorder (complete webm, the batch fallback)
      AudioWorklet PCM sidecar → WebSocket /captures/stream → Python server
  → server: rolling Whisper windows + per-phrase LLM cleanup on one MLX worker
key up → "dictate:stop" → webview flushes the worklet → sends "finish"
  → server finishes the last phrase → final capture JSON → webview
  → invoke paste_final_text (Rust) → clipboard + hide pill + 80 ms + ⌘V
```

Tauri's shell is native Rust, and it is as close to macOS APIs as Swift
would be. The mistake was putting the time-critical capture path in the
webview. A Swift rewrite is not needed for speed. The webview stays for the
UI only (settings, captures, the pill).

## Evidence

- **Browser capture loses the start of speech.** Most streamed recordings
  (the 48 kHz archives written from the AudioWorklet) begin with
  0.54–0.60 s of digital zeros, and the user's speech starts right where the
  zeros end. Recordings that took the MediaRecorder path (16 kHz, the same
  Bluetooth headset, 2026-09-24 10:15) have no gap. The loss is in the WebKit
  Web Audio path, not the device.
- **Browser capture bursts on first use.** The first dictation after launch
  delivered 649.8 s of audio in 13.4 s (and 229.4 s in 5.1 s on another
  launch). This caused "Recognition cannot keep up" and a Whisper
  "Thank you." hallucination. It is currently worked around by a guard that
  falls back to the MediaRecorder batch path (captureStream.ts).
- **Startup is about 50 s.** Logged: Whisper turbo loads in 23.8–25.0 s,
  refinement 4B in 0.5–0.8 s, and the prompt warm-up takes 2.0 s. The cached
  Whisper is `openai/whisper-large-v3-turbo` (PyTorch-format safetensors,
  1.5 GB), which is likely converted to MLX on every load. This is not yet
  verified.
- **Previews can delay the final result.** Preview recognition re-runs
  Whisper on the whole growing phrase every 2 s on the single MLX worker. A
  release that lands while a preview runs waits for it and then recognizes
  again (+~0.3 s, seen on 2.0 s recordings).
- **Recognition goes through temporary files.** Every window is written to a
  temporary WAV so the file-based STT API can read it back.
- **Paste.** Clipboard swap, hiding the pill, then an 80 ms sleep before ⌘V,
  and a 400 ms wait before the clipboard is restored.
- **Warm path today.** Release to final text is 0.43–0.94 s server-side on
  real recordings (after prompt caching). Whisper takes 0.30–0.38 s and the
  cached 4B cleanup 0.14–0.59 s.

## Work items

Each item says which files it owns, so the items can proceed in parallel. An
item touching a file it doesn't own should make the smallest possible change
and say so in its hand-off.

### 1. Native microphone capture and streaming (highest impact)

The desktop app captures the microphone itself and streams it to the
server. The webview no longer touches audio for dictation.

- On chord Start in `hotkey_monitor.rs`, open the selected input device with
  `cpal` (already a dependency). Downmix to mono s16le, and send frames of
  about 100 ms from a lock-free handoff (never block the audio callback).
- A Rust WebSocket client speaks the existing `/captures/stream` protocol
  version 1 unchanged (see STREAMING_DICTATION_PHASE_1.md), so the server
  needs no protocol change. Connect on key-down and buffer frames until
  `ready`, so no audio is lost while connecting. Chord End flushes, sends
  `finish` and awaits `final`, with the existing result-recovery endpoint as
  a fallback.
- Events to the pill webview: recording / transcribing / done / error, plus
  the final capture. Paste delivery keeps using `paste_final_text` (or item 4
  when it lands). Focus is still snapshotted at chord Start.
- Remove the dictation use of getUserMedia, MediaRecorder, the AudioWorklet,
  the dual-copy fallback and the faster-than-real-time guard. The web-only
  build (no Tauri) keeps its browser path.
- Microphone selection: the saved `input_device_id` is a WebKit device ID. A
  Tauri command lists native input devices, and the setting stores a stable
  native identifier. If the saved device is missing, fall back to the system
  default.
- Log the time from key-down to the first non-zero sample, and the ratio of
  audio to wall-clock time per take.
- Owner files: new `app/src/dictation/` module, `hotkey_monitor.rs`,
  command registration in `main.rs`, `Cargo.toml`,
  `app/src/components/DictateWindow/DictateWindow.tsx`,
  `app/src/lib/hooks/useCaptureRecordingSession.ts`, the microphone picker in
  `CapturesPage.tsx`, and the frontend tests for these.
- Acceptance: no leading run of zeros longer than the device's own startup,
  audio length ≈ wall time on every take (including the first after launch),
  and release-to-final no worse than today. Tests run without a microphone
  (fake frames). The user does the final hands-on check with the hotkey.

### 2. Whisper load time

- Measure what the 24 s is (file read, conversion, graph build). If it is
  conversion, load a pre-converted MLX Whisper, or cache the converted
  weights once, without changing accuracy.
- Owner files: `backend/backends/mlx_backend.py` (loading),
  `backend/services/model_startup.py`, model repo mapping and readiness, and
  backend tests.
- Acceptance: a logged startup load of a few seconds on this machine, and
  transcripts identical on the benchmark fixtures.

### 3. Previews never delay the final result

- On `finish`, don't let a queued preview run. Don't re-run recognition on
  audio a preview already covered, if the text can be reused safely. Avoid
  re-transcribing the whole growing phrase every 2 s when a cheaper rule
  gives the same result.
- Owner files: `backend/services/capture_stream.py`,
  `backend/routes/capture_stream.py`, `backend/tests/test_capture_stream.py`.
- Acceptance: release-to-final on the 2.0 s case drops by the preview's
  time, with no change in final text on the benchmark fixtures.

### 4. Recognition without temporary files (goes with item 2)

- Pass PCM arrays to Whisper in memory instead of writing a WAV per window,
  if mlx-audio supports it. Otherwise document why not.
- Owner files: `backend/backends/mlx_backend.py` (transcribe),
  `backend/services/transcribe.py`. Coordinate with item 3, which calls
  `recognize()`.

### 5. Text insertion without the clipboard

- When the focused element supports it (AXValue, or setting the selected
  text through the accessibility APIs), insert the text directly. Verify
  that it landed, and fall back to the clipboard ⌘V path when it did not.
  Never double-insert.
- Owner files: a new Rust module (`app/src/text_insert.rs`),
  with the smallest possible hook into `paste_final_text` in `main.rs`.
- Acceptance: unit-tested decision logic, plus a documented capability
  check. Real apps are checked by hand (TextEdit, a browser textarea,
  VS Code, a terminal).

### 6. Engine comparison (research only)

- Benchmark WhisperKit (Swift, Core ML / Neural Engine, streaming) against
  the current MLX Whisper: latency, accuracy on the fixtures, memory and
  power. Recommend switching or staying. No app changes.
- Output: a report in `docs/plans/`.

## Rules for every item

- Nothing adds time after release. Measure before and after.
- Write the failing test first. Keep the suite green, apart from the known
  failures in `test_progress.py` and `test_profile_duplicate_names.py`.
- Never touch the user's real data directory. Copy it into a scratch
  directory for experiments.
- Hand-off: changed files, commands run and their results, measured numbers
  with the machine and models, and what still needs a hands-on check.
