# Shared adapters

Approved 2026-10-05. Kass will ship a cleanup adapter and a voice adapter that
we train ourselves, inside the app and on by default, with a setting to turn
them off. Each Mac's own training continues from them instead of from the
stock models. No user audio, text, weights or counts are collected: the
training data is made up (cleanup) or public (voice).

**Why:**

- **Day one:** a new Mac gets good cleanup before it has any corrections to
  learn from.
- **Speed:** behavior trained into the adapter can come out of the prompt, so
  every cleanup reads fewer tokens.
- **Reliability:** small models follow prompt rules some of the time; trained
  behavior is steadier.
- **Less training on each Mac:** the voice adapter already knows busy rooms,
  so a Mac only has to learn its user's voice.

## Steps

1. **Test set** (done): a fixed set of cleanup cases and a scorer.
2. **Cleanup adapter:** made-up training pairs, one adapter per model size,
   then a shorter prompt.
3. **Voice adapter:** public speech mixed with other talkers and room noise.
4. **Ship both:** bundled in the app, on by default, personal training
   continues from them.

**Status (2026-10-06):** cleanup adapters for 0.6B, 1.7B and 4B ship. The
voice adapter doesn't: trained on public speech it beat turbo on public
test speakers but not on a real user's dictation (see below). Voice
recognition stays plain turbo until a Mac's own voice training activates.
The prompt stays as it is (see "Prompt length").

## Test set (`scripts/shared-adapters/`)

`cleanup_test.py` holds 83 cases in 10 categories: filler words, restarts,
repeats, changed answers, technical terms, questions, commands, punctuation,
numbers, and clean text that must come back unchanged. Never train on them
or generate training data from them.

`eval_cleanup.py` runs each case through production cleanup (same
preprocessing, prompt, examples and sampler, no personal state) and counts
token edits against the expected text, including punctuation and capitals.
The preprocessing and prompt come from the Rust crate dictation uses
(`cargo run -p kass-text --example cleanup_cases`, through
`cleanup_pipeline.py`), so the scripts need cargo; `train_cleanup.py` prepares
its pairs the same way.

```sh
backend/venv/bin/python scripts/shared-adapters/eval_cleanup.py --size 4B --show
```

**Stock models (2026-10-05, one seed, errors / exact matches):**

| Category | 0.6B | 1.7B | 4B |
|---|---|---|---|
| filler (9) | 11 / 5 | 20 / 3 | 6 / 6 |
| restart (8) | 32 / 1 | 31 / 1 | 40 / 1 |
| repeat (8) | 1 / 7 | 0 / 8 | 0 / 8 |
| changed answer (8) | 13 / 5 | 12 / 5 | 20 / 4 |
| technical (8) | 10 / 4 | 4 / 5 | 0 / 8 |
| question (8) | 14 / 6 | 9 / 6 | 11 / 6 |
| command (8) | 155 / 0 | 54 / 4 | 273 / 4 |
| punctuation (8) | 3 / 5 | 9 / 4 | 6 / 5 |
| numbers (8) | 2 / 7 | 1 / 7 | 8 / 7 |
| leave alone (10) | 4 / 8 | 1 / 9 | 4 / 8 |
| **total (83)** | **245 / 48** | **141 / 52** | **368 / 57** |
| changed facts | 7 | 3 | 7 |
| median cleanup | 514 ms | 648 ms | 1,747 ms |

Every size keeps most restarts ("I want to, we need to…") and many changed
answers. 0.6B answers every command; 4B carries out four of them at length,
which is most of its error count. Prompt: 1,086 tokens.

## Training the cleanup adapter

```sh
backend/venv/bin/python scripts/shared-adapters/make_cleanup_data.py
backend/venv/bin/python scripts/shared-adapters/train_cleanup.py --size 4B
backend/venv/bin/python scripts/shared-adapters/eval_cleanup.py --size 4B \
  --adapter build/shared-adapters/cleanup-4B --out build/shared-adapters/eval-4B-v1.json
backend/venv/bin/python scripts/shared-adapters/eval_cleanup.py --size 4B \
  --out build/shared-adapters/eval-4B-stock.json
backend/venv/bin/python scripts/shared-adapters/publish.py cleanup 4B
```

- **Data** (`make_cleanup_data.py`): about 500 clean sentences
  (`cleanup_sentences.py`), each dirtied the way people speak: filler words,
  a false start that repeats the opening ("I'll send it, I'll send the
  invoice") or is cut off ("So what I,"), a repeat, a changed answer
  ("Thursday, no, Wednesday"),
  missing punctuation, stray capitals, or a path said as words. 15% stay
  clean, and some start with openers that carry meaning ("So,", "Honestly,")
  and must stay. Sentences close to the test set are dropped.
- **Shape:** the same as a personal adapter (rank 8 on the last four layers),
  so a Mac's own training continues from it.
- **Idle only:** training holds the GPU, and macOS stops a GPU job that keeps
  the screen waiting ("Impacting Interactivity"). The trainer runs only after
  two minutes without input, in chunks of 100 examples, and redoes a stopped
  chunk from the last save with a fresh optimizer. A stopped pass leaves the
  optimizer state unusable: keeping it produced a model that answered with
  nonsense.
- **Lost words:** the scorer also counts words of the expected text missing
  from the output. Edit counts weigh a lost "Can you" like a missing comma,
  but losing words changes what the user said. Publishing needs no more lost
  words than stock (stock: 58 on 0.6B, 28 on 1.7B, 28 on 4B).
- **v1 (0.6B, 2,000 examples):** 245 → 65 errors, 48 → 64 exact, 7 → 2
  changed facts, 58 → 23 lost words, 331 ms. But it deleted real openers
  stock keeps ("Can you, uh, send me" → "Send me"). The false starts in its
  data were whole phrases ("Can you,", "We should,") put before a sentence,
  so it learned that a short phrase before a comma is a false start. Fixed
  in the data; v2 is the full run on it.
- **Published (2026-10-05, 2,000 examples each, one seed):**

  | | 0.6B | 1.7B | 4B |
  |---|---|---|---|
  | errors | 245 → 66 | 141 → 100 | 368 → 36 |
  | exact (of 83) | 48 → 59 | 52 → 60 | 57 → 70 |
  | changed facts | 7 → 3 | 3 → 1 | 7 → 1 |
  | lost words | 58 → 24 | 28 → 24 | 28 → 6 |
  | median cleanup | 514 → 330 ms | 648 → 674 ms | 1,747 → 1,622 ms |

  Left over: 1.7B still writes the poem it's asked to dictate ("write a
  short poem about autumn"), as stock does; every size keeps some restarts
  ("The reason it's slow is, the slow part is…") and misses a few changed
  answers ("5, well, 5:30" keeps 5).
- **Trial (0.6B, 400 examples, before the full run):** 245 → 108 errors,
  48 → 58 exact, restarts 32 → 7, questions 14 → 0, and 514 → 342 ms per
  cleanup. It also dropped real openers ("So, I think…"), which the kept
  openers in the data now teach against.

## Training the voice adapter

```sh
uv run --no-project --with pyarrow --with soundfile --with numpy \
  python scripts/shared-adapters/unpack_voxpopuli.py
backend/venv/bin/python scripts/shared-adapters/voice_data.py
backend/venv/bin/python scripts/shared-adapters/train_voice.py --updates 2000
backend/venv/bin/python scripts/shared-adapters/train_voice.py --score --out build/shared-adapters/score-voice.json
backend/venv/bin/python scripts/shared-adapters/train_voice.py --score --mine --out build/shared-adapters/score-voice-mine.json
backend/venv/bin/python scripts/shared-adapters/publish.py voice
```

- **Data** (`unpack_voxpopuli.py`, `voice_data.py`): VoxPopuli English
  validation and test (CC0, European Parliament speeches), up to 40
  utterances per speaker. Targets are plain turbo's transcripts, kept when
  they match VoxPopuli's within 10% of words. 15% of speakers are held out
  for testing.
- **Not audiobooks:** the first run trained on LibriSpeech dev-clean and
  dev-other. With talk and noise it cut public test errors 1,888 → 1,514, but
  on this Mac's own takes it was worse than plain turbo (noisy 713 → 914,
  clean 28 → 42): with someone talking behind the user it wrote down the
  background talker ("His housekeeper…"). The talkers Kass mixes in are
  audiobook readers too (LibriSpeech test-clean), so it had learned to
  follow audiobook speech. Target speakers must not sound like the
  background ones.
- **VoxPopuli result (not shipped):** on the public test speakers it passes
  the voice gate easily (talk and noise 1,555 → 560 word errors, clean 0 →
  42 of 2,880 words against turbo's own transcripts, same speed). On this
  Mac's 40 test takes it doesn't reliably beat plain turbo. Snapshots of one
  run, talk and noise (turbo 660) / clean (turbo 26): 80 updates 547 / 26,
  160: 764 / 32, 320: 534 / 34, 640: 758 / 38; an earlier run at 600
  scored 475 / 30, and at 2,000 607 / 37. Clean takes get steadily worse,
  and the noisy count swings with occasional repetition loops ("C-A-C-A-…")
  and writing down the background talker. `publish.py voice` now requires
  passing against plain turbo on this Mac's takes too, and refuses it.
- **What might work:** target speech that sounds like dictation (close
  mics, short commands, technical words), background talkers that aren't
  all audiobook readers, a guard against repetition loops, and choosing the
  number of updates on held-out real dictation rather than public speech.
- **Training** (`train_voice.py`): Kass's own voice trainer, mixing other
  talkers and rooms the same way personal training does, in five-minute
  rounds while the Mac is idle.
- **Gate:** `publish.py voice` ships only weights that passed Kass's voice
  gate against plain turbo on the held-out speakers. Scores on this Mac's own
  takes go in the manifest; those takes never leave the Mac.
- **Size:** stored in float16 (about half of float32). Loading converts back
  to float32, so personal training continues at full precision.

## Prompt length

Step 2 planned to cut prompt rules the adapter now handles. The fixed part of
the cleanup prompt is already kept in the KV cache between takes
(`qwen_llm_backend._reusable_cache`), so each take only processes the
dictated words, and the eval's latencies already include that. A shorter
prompt would also need its own copy for when the setting is off, and the
adapter trained on it. The 0.6B speed-up in the trial (514 → 342 ms) came
from shorter, more direct answers, not from the prompt.

## How it ships

- **Files:** `publish.py` copies an adapter that beats stock on the test set
  into `backend/assets/shared-adapters/cleanup-<size>/` with `shared.json`
  (its id, the pipeline it was tested with, its scores). The server build
  bundles the folder.
- **Use** (`backend/services/shared_adapters.py`): cleanup uses the personal
  adapter when one is active, else the shared one for the model size and
  default cleanup flags. A shared adapter tested with other cleanup code
  (`pipeline_id`) is never used; `scripts/release.sh` stops a release that
  would ship one (`publish.py --check`).
- **Personal training** starts from the shared adapter unless the personal
  one is already built on it, and must still beat the model in use. A
  personal adapter records the shared one it's built on (`shared`).
- **Setting:** Settings › Transcription › Kass training, on by default. Off
  uses the plain models and only personal adapters built on them, and
  personal training starts from the plain model again. Turbo reloads at once.
- **Failures:** a shared adapter that fails to load is set aside until the
  next launch.
- **Pipeline id:** cleanup runs in Rust, and its id is
  `kass_text::pipeline::id()`, the same in the app and in these scripts. (It
  used to hash the Python worker's bytecode; that code was deleted on
  2026-11-23.)
- **Repo size:** the adapters are committed as plain files (no LFS): a few MB
  per cleanup size and about 28 MB for voice. Each retrain adds that to the
  history.
