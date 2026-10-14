# Vocabulary correction layer

The broader training/evaluation/deployment loop is implemented in
[MODEL_IMPROVEMENT.md](MODEL_IMPROVEMENT.md). This document describes only its
lightweight vocabulary-rule component.

The model-improvement scheduler (MODEL_IMPROVEMENT.md) runs this local CPU-only
job before every adapter run (every six hours when idle). Captures → Learning from
corrections exposes status, Check now, and Undo last update. No cloud uploads or
model fine-tuning occur in this layer.

## Voice edits beta

Everything marked (beta) below is the `voice_edits` beta feature
(`backend/beta.py`, `app/src/lib/betaFeatures.ts`): it runs only while
Settings › General › Beta updates is on. Without it, only manual reports are accepted (others get 409), reports
cannot be withdrawn (the endpoint answers 404), Captures' Undo only hides a
refined correction from the cleanup examples, the newest third of reports is held
out, and learning waits for the six-hourly run. Turning the beta on or off
re-evaluates the rules on the next run.

With the beta, learning also runs at the next idle minute (two minutes without
foreground work) after a report is saved or withdrawn.

## Reports and their sources (beta)

`POST /captures/{id}/feedback` files a report on the capture that wrote the text.
Its `source` says how it was made:

- `manual`: Captures → Report incorrect output (the default).
- `voice_fix`: the user fixed Kass's own text by voice ("Fix that, Morgan not
  Megan"). It counts like a manual report.
- `redictation`: the user dictated over Kass's text again. This is weaker
  evidence, so it feeds correction learning and adapter training data only, never
  cleanup examples, writing-style habits or known names. A rule it supports also
  needs an explicit (manual or voice fix) report asking for the same change.

Spoken reports (every source but `manual`) have spelled letters joined before
saving (`spelling.join_spelling`), so "M-E-G-H-A-N" is filed as "MEGHAN". File
spoken reports on `refined`: rules are case-sensitive and run after refinement,
and `transcript_refined` is what was inserted.

`DELETE /captures/{id}/feedback/{report_id}` withdraws one report and everything
it taught. Cleanup examples, habits and known names drop it at once. At the next
idle minute rules are relearned without it: a rule that lost its support is
removed, and a block its contradiction caused is lifted. If a cleanup adapter is
active, the adapter job also runs again, since the report may be in its training
data. Captures' Undo on a report withdraws it.

## Activation contract

- Read at most 500 recent reports; use the newest report per capture/target.
  Evaluation is bounded to reports of at most 1,000 characters.
- Deduplicate identical source utterances. Without the beta, the chronological
  first two thirds propose candidates and the last third is held out from
  candidate generation. With it (beta), every report is evidence as soon as it is
  saved: no chronological hold-out keeps new reports from proposing rules.
- Learn only one short, spelling-similar vocabulary replacement per report
  (one to three words for one to three, compared by letters so a split or
  join counts: "domain" for "to main"). It is anchored on each side by an
  unchanged word or, where the edit has none, the start or end of the take:
  "Commit, merge, domain" teaches "merge domain" → "merge to main" only at
  the end of a take. A comma may stand for a pause between the anchor and the
  words and is dropped with them. Numeric edits, insertions, deletions, broad
  rewrites and edits across other punctuation do not qualify. Rules learned
  before edges keep matching a word on each side with spaces only.
- Without the beta, require two distinct training recordings supporting the same
  contextual rule and an improvement on a distinct held-out recording. With it
  (beta), require two distinct recordings whose reports propose the same rule, at
  least one of them an explicit report, and an improvement on a third distinct
  recording; each run checks active rules the same way and drops those that lost
  this support (a withdrawn or deleted report), without blocking them. Match case
  and language either way. Whisper mishears a phrase differently each time, so
  the third recording may instead be a report that wrote the rule's corrected
  phrase (its anchors and new words, at the same edge) where its take had
  something else ("merged, been" → "merge to main").
- Reject any candidate that increases token edit distance on any saved example,
  changes any user-corrected expected text, or changes the independent unchanged
  examples in `correction_rules.KNOWN_GOOD`.
- Cap the active set at 32 rules. Benchmark the rule layer on 4,000 characters
  for 25 runs; median processing time must be at most 5 ms. This measures rule
  overhead, not full STT/LLM latency or model accuracy.
- Publish the rules and evaluation metrics atomically, retaining ten previous
  versions. A new contradictory report withdraws the affected rule on the next
  run and blocks it: permanently without the beta, while that report exists with
  it. Manual rollback blocks withdrawn additions from automatic reactivation
  permanently.

## Live dictation

Approved rules run after refinement, from an immutable in-memory snapshot.
There is no per-dictation report lookup, file read, additional model prompt, or
model invocation. Raw STT remains intact. Refinement must be enabled for these
corrections to take effect. Inputs over 4,000 characters and inputs with more
than 128 matching spans skip the rule layer. Overlapping matches are skipped.

This is conservative personal vocabulary adaptation, not autonomous model
training. The checks validate the deterministic correction layer on recorded
text; they do not establish general model quality or rerun recorded audio.
The model-improvement worker additionally trains refinement adapters and compares
actual outputs on independent recordings, as documented in MODEL_IMPROVEMENT.md.

## Persistence and controls

`correction-learning.json` in the server's data directory contains the active
revision, previous versions, blocked rule IDs (with the reports behind each
contradiction), data fingerprint and last metrics. Deleting a capture removes its
reports; with the beta, a rule they supported is dropped on the next run if it
lost its support. Without it, already learned rules stay.
Identical report data is not reevaluated each interval. Restart loads the last
published rules. Failure to load starts with no rules; job failures retain the
previous active state and log the failure.

- `GET /capture/learning`: status and last evaluation metrics
- `POST /capture/learning/run`: run the job in a worker thread
- `POST /capture/learning/rollback`: restore the prior safe version
- `DELETE /captures/{id}/feedback/{report_id}`: withdraw one report (beta)

Validation: `backend/venv/bin/python -m pytest backend/tests/test_correction_learning.py
backend/tests/test_capture_feedback.py backend/tests/test_personal_examples.py
backend/tests/test_model_improvement.py` (run as one command).
