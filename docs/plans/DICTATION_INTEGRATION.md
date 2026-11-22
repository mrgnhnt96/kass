# Dictation integration

Integrated on `improve/dictation-reliability` from baseline `51f49de`.

## Included upstream work

- [PR #1013](https://github.com/jamiepine/voicebox/pull/1013): dictation can run without a downloaded LLM when automatic refinement is off. Imported `57281a0` and `4fb0268`.
- Selected commits from [PR #929](https://github.com/jamiepine/voicebox/pull/929): `0070c04` serializes MLX lifecycle/inference; `9b1beba` protects microphone acquisition/stop/cancel and adds opt-in warm microphone; `7e424a2` carries focus per take and supports fullscreen dictation. The full draft release was not imported.
- [PR #1014](https://github.com/jamiepine/voicebox/pull/1014), `f3a3c0e`: input device selection, adapted to the new recording lifecycle.

Integration adjustments retain the existing backend imports, combine both microphone settings, release a warm device between takes after selection changes, and protect the sample visualizer from leaking a microphone after unmount. Missing/unplugged selected microphones fall back to default; permission errors propagate without retrying a different microphone.

Other candidates (#616, #848, #921, #943, #1037) are not included in this initial recommended integration.

## Verification

- `bun run test:dictation`: 8 passed. Exercises the actual React recording hook with fake MediaRecorder/getUserMedia: early release, early cancellation, concurrent acquisition, late acquisition after unmount, device switching during a take, per-take context through delayed conversion, missing-device fallback and permission denial.
- `.venv/bin/python -m pytest backend/tests/test_dictation_settings_migration.py backend/tests/test_mlx_thread_affinity.py backend/tests/test_refinement_samples.py backend/tests/test_refinement_collapse.py -q`: 23 passed. MLX tests fake heavy inference, so this does not establish hardware inference quality or performance.
- `bun run ci`: app/web TypeScript checks and web production build passed.
- `bun run --cwd tauri build`: desktop frontend production build passed.
- `cargo check --locked --manifest-path app/Cargo.toml`: passed on Apple Silicon macOS.
- `cargo test --locked --manifest-path app/Cargo.toml --bin kass`: compiled and linked successfully; the binary contains zero Rust unit tests.
- `git diff --check`: passed.
- Biome checks on the new helper, device enumeration and regression tests passed. Imported/existing frontend code still reports lint warnings; Rust emits macro/deprecation warnings; frontend builds report bundle-size warnings.

The checkout initially lacked dependencies. Installed JS dependencies, a local Python test environment and Homebrew Rust for verification. `scripts/setup-dev-sidecar.js` generated the repository's development placeholders: these are not packaged inference executables. No complete packaged application was built or installed. No model downloads, real microphone recording or synthetic paste were performed.

## Required live validation before daily use

1. With refinement off and only Whisper installed, record and insert a sentence. Turn refinement on and verify a missing LLM is identified.
2. Hold/release the shortcut quickly, cancel while the microphone opens, and begin another take while the previous one converts. Confirm no stuck recording or lost/misassociated result.
3. Record in a text editor, browser and terminal, including native fullscreen Spaces and multiple windows. Confirm target insertion and clipboard restoration, and check that the pill never receives the paste.
4. Select a USB microphone, change it between takes and during a take, then unplug it. Verify the active take remains intact and the following take uses the expected microphone.
5. Toggle keep-microphone-ready, then disable dictation. Verify the OS microphone indicator clears when the device should be released. Repeat a voice-profile sample recording to check the shared hook.
6. Run transcription and refinement on actual MLX models while unloading/reloading models. Verify no Metal stream crashes.

These tests remain unperformed. Automated checks cannot establish that the native fullscreen/window-class changes are safe across macOS versions, or guarantee that arbitrary apps accept synthetic paste. Windows/Linux were not validated. Keep this integration separate from `main` until live behavior is confirmed.
