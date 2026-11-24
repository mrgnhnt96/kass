# Rust inference (whisper.cpp + llama.cpp) vs the current MLX sidecar

Date: 2026-09-24. This is work item 6 in
[DICTATION_LATENCY_ROUND_2.md](DICTATION_LATENCY_ROUND_2.md). It is research
only, and no application code changed. It reuses the fixture approach from
[WHISPERKIT_COMPARISON.md](WHISPERKIT_COMPARISON.md).

## Recommendation: stay on MLX, and fix one MLX-side waste instead

Don't switch either model to Rust right now.

- **Whisper: stay.** whisper.cpp on Metal (large-v3-turbo, flash attention
  on) is **about the same speed or slightly slower** than MLX: a median of
  0.48–0.50 s against MLX's 0.41–0.47 s across the same interleaved rounds,
  and 0.343 s against 0.32 s on a quiet spot check. That's 0 to +0.05 s
  after release. The transcripts match MLX word for word on 11 of 12
  fixtures. The one difference is "All right" instead of "Alright". Load
  time is the same (under 1 s warm), and resident memory is the same
  (1.8 GB) unless a quantized model is used.
- **Cleanup LLM: stay, but take the cheap win.** llama.cpp's Qwen3-4B
  Q4_K_M is **about 0.05 s faster per warm cleanup** (median 0.29–0.30 s
  against MLX's 0.34–0.38 s). Most of that gap is not the engine. mlx-lm
  rebuilds a 151k-entry detokenizer vocabulary table on every call, which
  costs about **78 ms of Python per cleanup** (see "Where MLX loses time").
  Once that is fixed, MLX should match or beat llama.cpp, because MLX
  generates each token faster (10.5–13 ms against 16–17 ms). The GGUF quant
  also **changed 3 of 12 real cleanups**, and one of those is a correctness
  regression. For "Alright, is it better now?", Q4_K_M answered the question
  with "Yes." instead of cleaning it up.
- **Both in one process has a hard blocker.** The two crates each
  statically link their own copy of ggml. Linked together, the build
  reports 343 duplicate symbols, and the binary **crashes on the first
  llama.cpp call** (`GGML_ASSERT(a->op == GGML_OP_FLASH_ATTN_EXT)`), because
  llama.cpp resolved a function from whisper's older ggml. Making it work
  means building both against one shared ggml, or running one engine out of
  process.

**What to do next (a new work item, not done here):** stop the per-call
detokenizer rebuild in the MLX cleanup path. For example, reuse one
detokenizer's token map, or decode the generated ids once at the end. The
expected result is about −0.08 s on every dictation that runs cleanup, with
a small patch and no model or engine change. That beats both switches.

Revisit a Rust move only if the Python sidecar is removed for startup,
bundle-size or distribution reasons (see "When to revisit"). Latency is not
the reason: it gains close to nothing.

## Per-dictation impact (release → text)

The current logged total is about 0.65–0.8 s: Whisper 0.31–0.35 s, cleanup
0.30–0.45 s, and insertion about 0.08 s.

| Change | Whisper | Cleanup | Release → text |
|---|---:|---:|---:|
| Switch Whisper to whisper.cpp (f16, FA) | +0.00 to +0.05 s | – | **0 to +0.05 s (worse or equal)** |
| Switch cleanup to llama.cpp Q4_K_M, in process | – | −0.03 to −0.08 s | **about −0.05 s** (output changes on 3/12) |
| Both, in process (after fixing the ggml clash) | +0.00 to +0.05 s | −0.05 s | **about 0 s** |
| Remove the Tauri→sidecar hop (WebSocket on localhost) | – | – | a few ms (not measured) |
| **Fix the MLX detokenizer rebuild (recommended)** | – | about −0.08 s | **about −0.08 s** |

## What was run, and what comes from documentation

**Measured on this machine:** Apple M2 Max, 64 GB, macOS 26.6.2 (25G83).

- **whisper.cpp**, through `whisper-rs` 0.16.0 with the `metal` feature, in
  process (`scripts/benchmark-rust-inference/wrbench`).
  - The crate bundles whisper.cpp 1.8.3. With that version, the first model
    load compiled Metal shaders for 19.5 s. The results in the table were
    built against whisper.cpp master (`d09f61a`, 1.9.4) by patching a copy
    of `whisper-rs-sys` (the API is unchanged). The 1.8.3 build was only
    smoke-tested, not benchmarked.
  - Also built: `whisper-cli` from source (CMake, Metal, embedded library).
- **llama.cpp**, through `llama-cpp-2` 0.1.157 (`metal` feature), in process
  (`llbench`). It keeps one context, trims the KV cache to the prefix shared
  with the new prompt, prefills the rest, then samples greedily. This
  mirrors `MLXQwenLLMBackend._reusable_cache`.
  - Also measured: `llama-server` from source (llama.cpp master `84e76d8`),
    with one slot and `cache_prompt`, driven over HTTP (`llama_llm.py`).
- **MLX (current path):** `MLXSTTBackend.transcribe_array` and
  `MLXQwenLLMBackend.generate` (with its KV reuse), called through the repo's
  own backends.
  - Versions: MLX 0.32.2, mlx-audio 0.4.1, mlx-lm 0.31.1, `HF_HUB_OFFLINE=1`,
    `config.set_data_dir(<scratch copy>)`.
  - Models: the cached `openai/whisper-large-v3-turbo` and
    `mlx-community/Qwen3-4B-4bit`.
- **Models downloaded** into a scratch folder: whisper.cpp
  `ggml-large-v3-turbo.bin` (f16, 1.62 GB, 61 s), `-q8_0` (874 MB, 48 s) and
  `-q5_0` (574 MB, 39 s), plus Qwen's official `Qwen3-4B-Q4_K_M.gguf`
  (2.50 GB, 65 s) and `Qwen3-4B-Q8_0.gguf` (4.28 GB, 83 s). For comparison,
  the MLX caches are 1.5 GB (Whisper) and 2.1 GB (4B).
- **Whisper fixtures** (16 kHz mono):
  - 8 of the user's real captures closest to 2, 5, 9 and 14 s (two each,
    `make_real_fixtures.py`), copied read-only from a scratch copy of
    `captures/` and resampled with `afconvert`.
  - 4 synthetic `say` fixtures from `make_fixtures.py`: `short` 2.1 s, `six`
    6.3 s, `fifteen` 13.9 s and `tech` 12.5 s.
- **Cleanup prompts:** `build_prompts.py` runs the real `refine_transcript`
  with a capturing backend, on a read-only scratch copy of `kass.db`,
  `writing-style.json` and `correction-learning.json`.
  - Settings used: "learned" punctuation, with smart cleanup, self-correction
    and technical-term preservation all on. Personal examples and notes were
    included.
  - This produced 12 prompts from the 12 most recent dictations that reach
    the model. Each is 2,480–2,502 tokens, of which the first 2,466 are
    shared and cached. That leaves 14–36 new tokens per call. Outputs were
    3–32 tokens (median 11). Sampling was greedy (temperature 0).
- **Method:** every engine runs in its own process. Each round runs every
  engine back to back, and rounds repeat (Whisper: 3 rounds × 4 runs per
  fixture; LLM: 3 + 4 + 5 rounds × 3 passes over 12 prompts). The first call
  in each process is reported separately as "first" or "cold".
- **Memory:** macOS `phys_footprint` and resident size, from
  `proc_pid_rusage`. Footprint leaves out mmap'd model files, which is how
  llama.cpp loads GGUF weights, so the two numbers are compared together.

**Not measured:**

- candle. It was dropped to keep the time box. It has Whisper and Qwen3
  examples, but the prompt-prefix KV reuse, samplers and quantized Metal
  paths would be ours to write and maintain. llama.cpp already provides
  them, so it was the candidate worth measuring.
- Power.
- 8–16 GB machines.
- Precompiled `default.metallib` builds. They need Xcode's Metal Toolchain
  component, which isn't installed here.
- A Tauri build.
- The launch-to-ready time of a Rust-only app (an estimate is given below).

**Contention:** other agents' benchmarks and builds were running
throughout. The load average ranged from 4 to 59. Whisper round 1 and LLM
session 2, round 4, were the noisiest. Interleaving means every engine saw
the same conditions, and per-round medians are shown so the spread is
visible.

## Results: Whisper

### Warm latency (seconds): median of warm runs (min), all 3 rounds pooled

| Fixture | MLX (current) | ggml f16 | ggml f16 + FA | q8_0 + FA | q5_0 + FA | f16 + FA, no timestamps |
|---|---:|---:|---:|---:|---:|---:|
| real02a (2.0 s) | 0.540 (0.29) | 0.707 (0.66) | 0.443 (0.41) | 0.462 (0.43) | 0.565 (0.49) | 0.419 (0.36) |
| real02b (2.0 s) | 0.446 (0.32) | 0.664 (0.64) | 0.466 (0.41) | 0.539 (0.47) | 0.606 (0.51) | 0.488 (0.38) |
| real05a (5.0 s) | 0.453 (0.38) | 0.688 (0.65) | 0.475 (0.43) | 0.527 (0.50) | 0.576 (0.52) | 0.487 (0.45) |
| real05b (4.9 s) | 0.414 (0.39) | 0.687 (0.63) | 0.455 (0.40) | 0.510 (0.47) | 0.574 (0.50) | 0.480 (0.45) |
| real09a (9.2 s) | 0.433 (0.41) | 0.686 (0.62) | 0.479 (0.43) | 0.645 (0.50) | 0.575 (0.53) | 0.516 (0.48) |
| real09b (9.3 s) | 0.605 (0.45) | 0.956 (0.65) | 0.658 (0.49) | 0.643 (0.53) | 0.580 (0.55) | 0.521 (0.46) |
| real14a (13.9 s) | 0.464 (0.41) | 0.793 (0.62) | 0.523 (0.49) | 0.540 (0.52) | 0.563 (0.54) | 0.498 (0.44) |
| real14b (13.9 s) | 0.456 (0.39) | 0.709 (0.60) | 0.507 (0.47) | 0.601 (0.50) | 0.562 (0.55) | 0.501 (0.45) |
| short (2.1 s) | 0.482 (0.34) | 0.614 (0.53) | 0.473 (0.43) | 0.502 (0.43) | 0.528 (0.51) | 0.584 (0.45) |
| six (6.3 s) | 0.481 (0.41) | 0.878 (0.61) | 0.501 (0.48) | 0.537 (0.51) | 0.550 (0.52) | 0.507 (0.42) |
| fifteen (13.9 s) | 0.425 (0.38) | 0.806 (0.65) | 0.502 (0.46) | 0.510 (0.49) | 0.628 (0.53) | 0.473 (0.43) |
| tech (12.5 s) | 0.589 (0.43) | 0.670 (0.63) | 0.518 (0.49) | 0.546 (0.50) | 0.577 (0.53) | 0.503 (0.44) |
| **All warm, median** | **0.470** | 0.696 | **0.501** | 0.539 | 0.571 | 0.496 |
| Per-round median (r1 / r2 / r3) | 0.675 / 0.462 / 0.406 | 0.806 / 0.703 / 0.659 | 0.618 / 0.488 / 0.480 | 0.587 / 0.570 / 0.506 | 0.660 / 0.556 / 0.555 | 0.560 / 0.455 / 0.487 |
| First call after load | 0.451 | 0.661 | 0.499 | 0.525 | 0.693 | 0.472 |
| Load, warm OS caches | 0.42 s | 0.70 s | 0.56 s | 0.36 s | 0.31 s | 0.56 s |
| Resident / footprint (MB) | 1,690 / 1,650–2,490 (peak 3,050) | 1,830 / 1,810 | 1,830 / 1,800 | 1,040 / 1,040 | 750 / 720 | 1,830 / 1,810 |

On a quiet spot check (load average 2.5, `real05b`), whisper.cpp f16 + FA
took 0.341–0.355 s against MLX's 0.32 s.

Findings:

- **Flash attention matters.** `whisper-rs` defaults it off, and it costs
  about 0.2 s per call.
- **Quantization doesn't make it faster.** q8 and q5 are slower than f16 on
  this GPU, because the turbo decoder is small and the encoder is
  compute-bound. They only save memory: 1.0 GB and 0.75 GB against 1.8 GB.
- **Shrinking `audio_ctx` breaks turbo.** Fitting the encoder window to the
  clip gives garbage on short clips (for example `short` → `"'"`, `six` →
  "The"), with a WER of 0.38 against MLX. This is a known limitation of
  large-v3-turbo. It is not usable, so it is left out of the table.

**Agreement.** Every whisper.cpp variant (f16, q8, q5, with or without FA)
matches MLX word for word on 11 of 12 fixtures. The one difference, on
`real02a`, is "All right" against "Alright". WER against the scripts on the
synthetic fixtures is 0.053 for every engine, from the same `tech`
normalization ("7" and "$5"). Without timestamp tokens, `tech` gains sentence
periods and "-5 dollars", so that mode changes output. Runs of the same
engine never differed from each other.

**Metal shader compile on first launch.** Neither engine ships precompiled
kernels in these builds (`GGML_METAL_EMBED_LIBRARY`), so the first load after
a new binary compiles them:

- 19.5 s (whisper-rs 0.16 / whisper.cpp 1.8.3)
- 25–46 s (whisper.cpp master under load)
- 20.5 s for llama.cpp's flash-attention kernels alone

After that, macOS caches them and loads take under 1 s. The cost comes back
for every new binary, which means after every app update. Shipping a
precompiled `default.metallib` should remove it, but that needs Xcode's
Metal Toolchain, which isn't installed here, so it was not measured.

## Results: cleanup LLM (Qwen3-4B, ~2,490-token prompt, 2,466 tokens cached)

Final session: 5 interleaved rounds × 3 passes × 12 prompts, with 35 warm
calls per process.

| | MLX 4-bit (current) | llama-cpp-2 Q4_K_M (in process) | llama-server Q4_K_M (HTTP) | llama-cpp-2 Q8_0 |
|---|---:|---:|---:|---:|
| Load (warm OS caches) | 0.72 s | 0.76 s | 0.85 s | 0.78 s |
| Cold call (whole 2.5k prompt + output) | 4.66 s | 4.44 s | 4.31 s | 4.30 s |
| **Warm call, median** | **0.352 s** | **0.296 s** | 0.302 s | 0.335 s |
| Warm call, p90 | 0.491 s | 0.445 s | 0.495 s | 0.576 s |
| Warm time to first token (reuse 2,466; read 14–36 new tokens) | 0.214 s | 0.114 s | 0.123 s | 0.118 s |
| Per further output token | 13.3 ms (mlx-lm's own: 10.5 ms) | 17.4 ms | 16.9 ms | 21.6 ms |
| Modeled: 10 / 20 / 30 output tokens | 0.33 / 0.47 / 0.60 s | 0.27 / 0.44 / 0.62 s | 0.28 / 0.45 / 0.61 s | 0.31 / 0.53 / 0.75 s |
| Resident / footprint at end (MB) | 2,450 / 2,750 (peak 3,500) | 3,100 / 690 | 3,110 / 670 | 4,800 / 690 |
| Output identical to MLX | – | 9/12 | 9/12 | 7/12 |

Per-round warm medians across all three sessions:

| Session | MLX | llama-cpp-2 Q4_K_M | llama-server Q4_K_M | llama-cpp-2 Q8_0 |
|---|---|---|---|---|
| 1 | 0.297 / 0.300 / 0.326 | 0.246 / 0.231 / 0.246 | 0.243 / 0.248 / 0.281 | 0.266 / 0.289 / 0.324 |
| 2 | 0.355 / 0.353 / 0.453 / 0.779 | 0.283 / 0.275 / 0.343 / 0.510 | 0.284 / 0.291 / 0.366 / 0.447 | 0.309 / 0.319 / 0.375 / 0.503 |
| 3 | 0.380 / 0.361 / 0.374 / 0.338 / 0.339 | 0.297 / 0.289 / 0.295 / 0.298 / 0.293 | 0.302 / 0.292 / 0.302 / 0.301 / 0.311 | 0.318 / 0.335 / 0.347 / 0.332 / 0.341 |

**Prompt-cache reuse works the same way in both engines.** Each warm call
reused exactly the 2,466 shared tokens and read only the new transcript.
llama-cpp-2 does this with `clear_kv_cache_seq(0, shared, -1)` and a decode
of the suffix. llama-server does it with `cache_prompt` on a single slot, and
the server's `cache_n` = 2,466. A cold 2.5k-token prefill takes about 4.3 s
in llama.cpp and 4.7 s in MLX. The startup warm-up already pays that cost.

**Output agreement at temperature 0** (the prompts are the user's real
dictations):

| Prompt (raw transcript) | MLX 4-bit | llama.cpp Q4_K_M | llama.cpp Q8_0 |
|---|---|---|---|
| "Newline, what else can we improve if we were to use REST?" | kept "Newline," | dropped "Newline" | dropped "Newline" |
| "Alright, is it better now?" | "Alright, is it better now?" | **"Yes."** (answered the question) | "Yes, it's better now." |
| "I want to redesign the UI, …" (starts "New line") | kept "New line." | dropped it | dropped it |
| "The transcribe still took a little bit though" | unchanged | same as MLX | "transcription" |
| "…everything we can. That is not pertinent…" | unchanged | same as MLX | joined into one sentence |

The GGUF and MLX builds quantize differently, so the greedy output is not
the same model. Whether "Newline" should survive is a separate question. But
answering the user's question instead of cleaning it up is a real
regression. A switch would need a full accuracy evaluation on the
correction-learning set, not just a speed test.

### Where MLX loses time (the actionable part)

The 0.1 s time-to-first-token gap (0.214 s against 0.114 s) was profiled on
4 warm `_generate_sync` calls:

- **About 78 ms per call**:
  - `mlx_lm.tokenizer_utils.BPEStreamingDetokenizer.__init__`, reached
    through `TokenizerWrapper.detokenizer`, builds a new streaming
    detokenizer on every `stream_generate`.
  - Its constructor calls `tokenizer.vocab` twice (`get_vocab()`: 35 ms
    each) to rebuild an id → token list of about 151k entries.
- **About 9 ms per call**: `wired_limit` walks the model's parameters to
  sum their size, and sets and then resets the wired limit.
- **About 110 ms**: mlx-lm's own "prompt" time for the suffix. This is the
  18-token prefill plus the first step, the cache-state `mx.eval` and
  `mx.clear_cache()`.
- Chat template and tokenization take only 3 ms.

Removing the detokenizer rebuild alone should bring MLX's warm cleanup to
about 0.27–0.30 s, the same as llama.cpp. MLX then keeps its faster
per-token generation and its identical outputs.

Whisper on MLX was profiled the same way. Its time is spent waiting on the
GPU inside `mlx_audio` decoding, and there was no comparable Python
overhead.

## Startup and memory impact

- **Model loads are not the startup problem.** When the OS file caches are
  warm, both engines load Whisper in 0.3–0.7 s and the 4B in 0.7–0.9 s. The
  app's "about 22 s to ready" is the PyInstaller one-file sidecar unpacking
  and importing (item 1).
- **A Rust-only app** would drop the sidecar's spawn, unpack and imports.
  Launch-to-ready would probably be about 1–2 s plus the prompt warm-up.
  That is an estimate and was not measured. However:
  - Every new build pays a **20–45 s Metal shader compile** on first use
    unless a precompiled metallib is shipped.
  - Item 1 (a one-folder bundle) goes after the same 22 s at a fraction of
    the cost.
- **Memory is about the same.**

| | Resident |
|---|---|
| The running sidecar today (Whisper + 4B + Python) | 4.16 GB |
| whisper.cpp f16 | about 1.8 GB |
| llama.cpp Q4_K_M, 4,096 context (about 2.4 GB mmap'd weights + 576 MB KV cache + compute buffers) | about 3.1 GB |
| **Rust total** | **about 4.9 GB** |

  MLX's KV cache grows only as far as the prompt, while llama.cpp allocates
  the whole context up front. A 3,072-token context would trim about
  150 MB. Real savings would come only from a quantized Whisper (q5: −1.1 GB,
  identical text on these 12 fixtures). MLX could use a quantized Whisper
  too.
- **Bundle size.**
  - Today the app is 291 MB, and 267 MB of that is the Python sidecar.
  - The Rust engines add 2.4–4 MB (whisper-rs) and about 7 MB (llama-cpp-2)
    of code to the Tauri binary.
  - So dropping Python would shrink the app to about 30 MB, but only if
    *all* of the backend moved: 10.5k lines of Python, the routes, the
    SQLite layer, correction learning and personal examples.
  - Models download separately either way, at 1.6 GB + 2.5 GB in GGUF
    against 1.5 GB + 2.1 GB in MLX.

## Integration cost if Kass ever moves inference into Rust

1. **One ggml, not two.** `whisper-rs-sys` and `llama-cpp-sys-2` each
   statically link their own ggml. Linked together, that gives 343
   duplicate symbols and a crash. The fix is to build whisper.cpp and
   llama.cpp from matching sources against one shared ggml:
   - `llama-cpp-sys-2` has a `system-ggml` feature.
   - `whisper-rs-sys` forwards `WHISPER_*` and `GGML_*` CMake variables such
     as `WHISPER_USE_SYSTEM_GGML`.

   The two projects' release cadences then have to be pinned together.
   Alternatively, run one engine in a helper process, which gives up part of
   the "no sidecar" benefit.
2. **Build toolchain.** CMake is needed, and it isn't on this machine (it
   was installed with pip into a scratch venv). CMake's `GGML_NATIVE` SME
   probe *hangs* on the M2 until it is killed. Set `GGML_NATIVE=OFF` or pin
   the CPU features. A cold `cargo build` of whisper-rs took 2.5 min and
   llama-cpp-2 about 1 min, and each ggml change rebuilds both.
3. **Metal kernels.** Either accept the 20–45 s compile on the first launch
   after each update, or add a build step that compiles `default.metallib`.
   That step needs the Xcode Metal Toolchain component on the build and CI
   machines. The library has to be signed and bundled in `Resources`. Being
   inside the main binary makes signing and notarization simpler than
   today's sidecar, since there is no second executable. As far as I know,
   Metal's runtime shader compile needs no JIT entitlement under the
   hardened runtime, but that was not tested here.
4. **Porting.** The following would move from Python to Rust:
   - Whisper decoding options: `initial_prompt` from the previous text, and
     the ellipsis-token suppression that `mlx_backend` applies.
   - The KV-cache reuse logic.
   - The Qwen chat template, which llama.cpp embeds in the GGUF, though its
     `enable_thinking=False` handling must be matched.
   - The refinement prompt builder, personal examples and correction notes.

   The whole thing has to be re-tested to show identical cleanup output.
5. **Personal LoRA adapters.** Model improvement trains MLX LoRA adapters
   on the 4B. llama.cpp can load GGUF LoRAs, but the adapters would need
   conversion (MLX → PEFT → `convert_lora_to_gguf.py`). Training would still
   need Python/MLX, or it would be lost. The user currently has no active
   adapter, and the latest run was rejected.
6. **Model management.** New download sources (ggml and GGUF files), cache
   layout, progress reporting and deletion.

## When to revisit

- **Removing Python becomes a goal in its own right**, for app size (291 MB
  → about 30 MB), launch time after item 1, or distribution simplicity.
  Latency is then neutral, and the ggml sharing, metallib build and LoRA
  story above become the work.
- **mlx-lm or mlx-audio stop being maintained, or regress on new macOS
  versions.**
- **Smaller Macs are targeted.** llama.cpp and whisper.cpp quantized models
  (q5 Whisper at 0.75 GB) may matter more on 8–16 GB machines. Measure
  there.

## Reproduce

Scripts are in `scripts/benchmark-rust-inference/`. Use a scratch directory
outside the repo. A worktree has no venv, so point `PY` at the main
checkout's `backend/venv/bin/python`. Never point anything at the real data
directory; copy it first.

`build_prompts.py` ran the worker's Python copy of cleanup, which was deleted
on 2026-11-23 once the Rust server owned cleanup. To rerun, build
`prompts.json` (`{"flags", "entries": [{"transcript", "prompt", "system",
"examples", "chat", "n_tokens"}]}`) from the Rust prompt instead: the
`kass-text` example `cleanup_cases` gives the stock prompt, and kass-server's
`/capture/learning/training/prompts` the user's styles (both without personal
examples or notes, which this study included).

```sh
S=/path/to/scratch; PY=backend/venv/bin/python
D="$HOME/Library/Application Support/com.mrgnhnt.kass"
mkdir -p $S/data $S/caps && cp "$D"/{kass.db,writing-style.json,correction-learning.json} $S/data/ && cp "$D"/captures/*.wav $S/caps/
$PY scripts/benchmark-rust-inference/make_real_fixtures.py $S/caps $S/fx
$PY scripts/benchmark-stt-engines/make_fixtures.py $S/fx && rm $S/fx/paused40.*
# $S/prompts.json: see the note above

# models (about 9.8 GB for all five; f16 Whisper + Q4_K_M is enough)
# ggerganov/whisper.cpp: ggml-large-v3-turbo{,-q8_0,-q5_0}.bin; Qwen/Qwen3-4B-GGUF: Qwen3-4B-Q4_K_M.gguf, Qwen3-4B-Q8_0.gguf -> $S/models

# Rust benches (need cmake on PATH; `pip install cmake` into a scratch venv works)
(cd scripts/benchmark-rust-inference/wrbench && CARGO_TARGET_DIR=$S/target-wr cargo build --release)
(cd scripts/benchmark-rust-inference/llbench && CARGO_TARGET_DIR=$S/target-ll cargo build --release)
# llama-server: build llama.cpp from source with -DGGML_METAL=ON (kill the hung
# GGML_MACHINE_SUPPORTS_sme try-compile if configure stalls)

PY=$PY WRBENCH=$S/target-wr/release/wrbench scripts/benchmark-rust-inference/run_whisper_rounds.sh $S 3 4
$PY scripts/benchmark-rust-inference/summarize_whisper.py $S/whisper.jsonl $S/fx
PY=$PY LLBENCH=$S/target-ll/release/llbench LLAMA_SERVER=/path/to/llama-server \
  scripts/benchmark-rust-inference/run_llm_rounds.sh $S 5 3
$PY scripts/benchmark-rust-inference/summarize_llm.py $S/llm.jsonl
# one binary with both engines (reproduces the ggml clash):
(cd scripts/benchmark-rust-inference/llbench && cargo build --release --features with-whisper)
```

Run the first load of each new binary twice. The first load includes the
Metal shader compile.

## Sources

- whisper.cpp (<https://github.com/ggml-org/whisper.cpp>) at master
  `d09f61a` (v1.9.4). `whisper-rs` 0.16.0 and `whisper-rs-sys` 0.15.0 source
  (`build.rs`: Metal is `GGML_METAL_EMBED_LIBRARY=ON`, and `GGML_*` and
  `WHISPER_*` env vars are forwarded to CMake;
  `WhisperContextParameters::flash_attn` defaults to off).
- llama.cpp (<https://github.com/ggml-org/llama.cpp>) at master `84e76d8`.
  `llama-cpp-2` and `llama-cpp-sys-2` 0.1.157 source (features `metal`,
  `system-ggml` and `dynamic-link`). The `llama-server` options used are
  `--cache-reuse`, `cache_prompt` and `-np`.
- `ggml-metal-device.m`: per-kind runtime compile of the embedded Metal
  source (`compiled 'fa' library in 20.466 sec`).
- mlx-lm 0.31.1 source: `generate.py` (`stream_generate`, `wired_limit`,
  `generate_step`) and `tokenizer_utils.py`
  (`BPEStreamingDetokenizer.__init__`, `TokenizerWrapper.detokenizer`).
- Models: <https://huggingface.co/ggerganov/whisper.cpp> and
  <https://huggingface.co/Qwen/Qwen3-4B-GGUF>.
