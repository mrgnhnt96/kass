# Sentence-aware streaming cleanup

## Problem

Streaming dictation cut audio into phrases at every 0.7 s pause and cleaned each phrase on its own. A pause is often the speaker thinking in the middle of a sentence, so two things went wrong:

1. Whisper ends every phrase cut at a pause as if it were finished, with a trailing `-`, `.` or `...`, and capitalizes the next phrase. The pause put those marks there, not the speaker.
2. Cleanup only ever saw one pause-delimited piece, so it couldn't repair a sentence split across pauses.

The user's example (7 phrases). Raw: `Wait, but if I pause- for a- second. Does that mean- that my pauses cannot be cleaned? From in between. Pauses.` Delivered: `Wait, but if I pause for a- Second. Does that mean that my pauses cannot be cleaned? From in between Pauses.`

## What changed

1. **Seam marks** (`phrase_seams.strip_pause_mark`, `continue_phrase`). A phrase cut at a pause loses a trailing period, dash or dots. An abbreviation's period stays (a final token with another `.` in it, like `U.S.`). The next phrase's first word is lowercased unless it is `I`, an acronym, or a word capitalized mid-sentence anywhere else in the dictation. That last test is a general rule for names; there is no word list. A question or exclamation mark is decided when the next phrase arrives (`continue_after_seam`).
   - *A `?` or `!` at a seam* is dropped, and the next phrase keeps the capital Whisper gave it, as the weaker hint that a sentence may start. Cleanup takes a mark as a sentence end, and it kept pause-made ones: `Wait, what if I pause? for a second` came out `Wait, what if I pause? For a second.` A capital that cleanup sees mid-sentence it lowercases when the words run on, and keeps as a sentence start when they don't. When the next word is always capitalized (`I`, a name), the capital can't be that hint, so the mark stays (`What's the other one? I have a reminder`). See *Question marks at pauses* below.
   - *Raw transcript saved:* the de-marked text. That is what cleanup actually receives, so personal examples and adapters trained from `transcript_raw` match production input, and the Captures view no longer shows `pause- for a-`. Whisper still gets its own text (`self.heard`) as `previous_text`, so recognition is unchanged.
2. **Sentence-aware cleanup** (`sentence_tail.settle`, `StreamingCapture.clean_tail`). Each accepted phrase is added to the *open tail*, the raw words since the last settled sentence, and the whole tail is cleaned again. Every sentence of the result except the last is *settled*: final, never cleaned again.
   - *Mapping back to raw:* `settle` aligns cleaned and raw words with difflib on normalized tokens. It settles only when both the last settled word and the first open word match raw words. Otherwise a boundary on the wrong raw word would repeat or lose words, so it waits for the next cleanup. Raw words that cleanup dropped at the boundary stay open.
   - *What counts as a sentence end:* a terminal mark after a letter, or a line break. `1.` and `$5.` don't count.
   - *Bound:* a tail over **30 raw words** with no sentence end is settled where it is and joined with the old seam rules. Among 374 of the user's cleaned sentences the median is 7 words, p95 21, p99 32.
   - *After release:* only the open tail plus the last phrase is cleaned.
   - *What still applies:* the content check, spoken-correction handling and `needs_final_refinement`, learned corrections (applied to the composed text), the punctuation styles and `close_phrase`, the degraded full-audio path, and provisional text (it now offers only settled text, the part that can't change).
   - *Spoken corrections:* a phrase that corrects settled text (a cue such as "no", "wait", "sorry", "I meant", "or rather", or a restatement of earlier words; see `retracted_by` in [SPOKEN_SELF_CORRECTIONS.md](SPOKEN_SELF_CORRECTIONS.md)) reopens the last settled chunk, so the retraction is cleaned together with what it retracts. If older settled text remains, the whole take is cleaned again at release, and the streamed text is kept if the content check rejects that cleanup.
3. **A cleanup running at release.** When speech follows the last cut, a cleanup started while speaking would be replaced by the one after release. `finish()` now stops it between tokens (`generation_stop`) instead of waiting for it. If the remaining audio turns out to have no words, the tail is cleaned once at finish. If nothing follows, the running cleanup is the final one and is kept.
4. **Lookup decoding** (`qwen_llm_backend._generate_lookup`, `generation_hint`). A cleanup mostly copies its transcript, and a re-clean of a grown tail mostly repeats the previous cleanup. Generation now proposes continuations taken from the previous cleanup of the tail (the hint) and from the transcript, and checks up to 24 proposed tokens in one model call.
   - *The output is unchanged:* each position is sampled from the model's own distribution and kept only while it equals the proposal, which is standard speculative sampling with a deterministic draft. With greedy decoding, 30 of 30 saved dictations produced identical output, in a median 0.11 s instead of 0.29 s.
   - *Prompt cache:* it still holds exactly the tokens fed. The stable system and example prefix is reused as before.
   - `refine_transcript` turns this on for every cleanup, not just streaming.

The timing line gains `after_release_tail_words`. `after_release_*` now counts only time after release, not the part of a cleanup that ran before it.

## Benchmark

**Setup**
- Machine and models: Apple M2 Max, Whisper large-v3-turbo, Qwen3 4B 4-bit.
- Settings: the user's own capture settings (learned punctuation style, 4B cleanup, English), personal examples, and writing-style profile.
- Data: a scratch copy of the data directory.
- Takes: all 52 saved dictations whose audio the production pause rule cuts into 2 or more voiced phrases (2 to 7 phrases, 3 to 31 s).
- Replay: each take was fed through `StreamingCapture` in 100 ms frames at real-time pace.

**Variants**
- **A:** current code (per-pause phrases), from commit 882b4c9.
- **A+:** A with lookup decoding.
- **B:** sentence-aware cleanup with plain decoding.
- **B+:** sentence-aware cleanup with lookup decoding. This is the branch.
- **C:** A's raw transcript (with Whisper's marks) cleaned in one call.
- **D:** B+'s de-marked raw transcript cleaned in one call. This is the fairest quality ceiling: C inherits Whisper's pause periods (`to the debt. Because`, `groceries. Bananas`).

**Runs**
- Three runs, variants interleaved per take with alternating order.
- Model training was not running (checked per take); the running Kass app was idle.
- Run 1: A, B, C. Run 2: A, A+, B+, C, D. Run 3: A, B+.

### Latency (release to final text; the part the user waits for)

| Variant | Takes | Release to final, p50 | p90 | Cleanup after release, p50 | p90 |
| --- | ---: | ---: | ---: | ---: | ---: |
| A (today) | 156 | 0.62 s | 0.83 s | 0.28 s | 0.41 s |
| A+ | 52 | 0.59 s | 0.67 s | 0.23 s | 0.31 s |
| B (sentence-aware, plain decoding) | 52 | 0.76 s | 0.94 s | 0.43 s | 0.59 s |
| **B+ (branch)** | 104 | **0.67 s** | **0.84 s** | 0.31 s | 0.48 s |

- **B+ against A per take:** B+ is +0.02 s at the median (paired, same take) and +0.17 s at the paired p90. The overall p90 is unchanged (0.84 vs 0.83 s).
- **Recognition after release:** about 0.33 s in every variant; this change doesn't touch it.
- **Outlier:** one take needed about 2.2 s in every variant because Whisper itself took about 1.9 s after release.
- **Without lookup decoding:** sentence-aware cleanup alone (B) costs about +0.14 s. Lookup decoding wins most of that back.
- **Cost floor:** a warm 4B cleanup costs about 0.15 s even for a few tokens, so shorter tails can't win much more.
- **Whole-dictation cleanup at release:** C with plain decoding takes 0.34 s p50 and 0.60 s p90 for cleanup alone, and grows with length. D with the B+ output as its hint takes 0.16 s p50 and 0.29 s p90. That was measured in isolation, with a hint that already contained the last phrase, so it is optimistic.

### Quality (52 takes, run 2)

Counts are summed over all takes.

| Variant | Words differing from D | Differing ignoring case and punctuation | Identical to D | Trailing `-` artifacts | Capital mid-sentence after a pause | Extra sentence breaks vs C |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A | 107 | 20 | 12/52 | 1 | 15 | 8 |
| A+ | 103 | 17 | 12/52 | 1 | 15 | 8 |
| **B+** | **8** | **3** | **46/52** | **0** | **0** | **0** |

- **Against C instead of D:** A differs by 68 words (24 ignoring case and punctuation) and B+ by 59 (7). The rest of B+'s difference from C is C keeping Whisper's pause periods.
- **Sentence-aware alone (B, run 1):** 0 artifacts and 7 words differing from C ignoring case and punctuation. Lookup decoding doesn't change quality.

### Side by side

| Take | A (today) | B+ (branch) | D (whole-dictation ceiling) |
| --- | --- | --- | --- |
| User's example, 7 phrases | Wait, but if I pause for a- Second. Does that mean that my pauses cannot be cleaned? From in between Pauses. | Wait, but if I pause for a second, does that mean that my pauses cannot be cleaned? From in between pauses. | Wait, but if I pause for a second does that mean that my pauses cannot be cleaned? From in between pauses. |
| 2 phrases | Is it possible to clean? While I am talking? | Is it possible to clean while I am talking? | same as B+ |
| 3 phrases | When I started the recording, it started, or inserted, or a previous video Transcript, I'm not sure why. | When I started the recording, it inserted a previous transcript. I'm not sure why. | same as B+ |
| 3 phrases | ...can you select, can we select the model from the model's tab view? | ...can we select the model from the model's tab view? | same as B+ |
| 2 phrases | Here is a list of groceries Bananas, peppers, cucumbers. | Here is a list of groceries: bananas, peppers, cucumbers. | same as B+ |
| 3 phrases | Whenever I... Don't have any voice in the audio, the output is always "You're welcome." | Whenever I don't have any voice in the audio, the output is always "You're welcome." | same as B+ |
| 2 phrases | ...more depth to the debt. Because right now you just see a number... | ...more depth to the debt because right now you just see a number... | same as B+ |

With the seam `?` rule (below) the user's example now comes out as the ideal "...cannot be cleaned from in between pauses?".

## Decisions

- **Settle rule:** every cleaned sentence but the last. A sentence can't be continued after its end mark, and the last one may be.
- **Tail bound:** N = 30 raw words, from the user's own sentence lengths. It keeps the cleanup after release about one phrase long in the worst case.
- **Stop instead of reuse at release:** when speech follows, the running cleanup's result would be discarded anyway. Stopping it saves the wait, and the final cleanup still gets the previous tail cleanup as its hint.
- **Lookup decoding for all cleanups:** the output distribution is unchanged, and it is faster everywhere measured.

## Needs hands-on checking

- **Real dictation feel:** the benchmark replays saved audio. It doesn't include WebSocket, paste or UI time.
- **Run-on sentences where the pause was a real sentence end:** removing Whisper's pause periods sometimes joins two sentences the speaker meant as two (`...what to say next and that doesn't always mean...`). The whole-dictation cleanup of the same text (D) makes the same call, so this is the cost of not trusting pause periods. The user should judge whether it reads better or worse.
- **Casual style and forced settles:** with casual punctuation, or when the 30-word bound settles a tail mid-sentence, the old seam join applies at that one point. None of the 52 takes hit the bound.
- **Provisional text:** it is off by default and now only offers settled sentences.
- **The lookup decoding loop** replaces `mlx_lm.stream_generate` for cleanups. Its unit tests use a fake model; the real model was checked for identical greedy output on 30 dictations. Watch the log line `(N model calls, M proposed tokens kept)`.

## Question marks at pauses

Whisper ends a phrase cut at a pause with `?` as readily as with `.`. Keeping the `?` let cleanup end the sentence there (`Wait, what if I pause? For a second.`).

**Rule.** When the next phrase arrives, a seam `?`/`!` goes, and the next phrase keeps Whisper's capital instead of being lowercased. If its first word is `I` or a name (a capital `continue_phrase` keeps anyway), the mark stays. No word lists.

**Alternatives tried** (whole-dictation cleanup of the same text; 3 to 5 samples per case at the production temperature, which nearly always agreed):

| Seam text given to cleanup | 10 real seams in 61 takes: pause-made `?` fixed (of 4) | real `?` kept (of 6) | Synthetic, from the user's questions: split question rejoined (of 42) | real `?` kept (of 16) |
| --- | ---: | ---: | ---: | ---: |
| keep `?`, lowercase next (before) | 2 | 6 | 33 | 13 |
| drop `?`, lowercase next | 4 | 4 | 42 | 8 |
| `?` becomes `,`, `...` or `—` | 4 | 1 to 3 | not run | not run |
| **drop `?`, keep capital (except `I`, names)** | **4** | **6** | **41** (+1 fine but unaligned) | **12** |

The synthetic set splits each of the user's saved questions of 7+ words at 60% (`Can you figure out why? It took so long?`) and joins each saved question to the sentence after it. The one real `?` the rule loses there: `then what's the point of this It doesn't do anything` became `...of this. It doesn't...`.

**Streaming replay** (61 multi-phrase takes, one interleaved run, no model training running):

| | Release to final p50 | p90 | Seam `?` left mid-sentence | Real `?` lost | Identical to whole-dictation cleanup |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before | 0.62 s | 0.82 s | 2 | 0 | 53/61 |
| **Seam rule** | **0.63 s** | **0.81 s** | **0** | **0** | 51/61 |

Paired per take: +0.005 s median. The two fewer identical takes are sampling differences (`an`/`the`, a dropped `that`, a comma) in takes whose raw text the rule didn't change; the takes it did change all match their whole-dictation cleanup except where both runs already differed.

**Raw transcript:** a seam the rule changed keeps a capital mid-sentence (`cannot be cleaned From in between pauses.`). That is what cleanup received.
