# Parity set

A fixed set of dictations, with the settings, dictionary entries, styles and
corrections each one needs, and what today's Python server made of them. Use it
to check that another server, such as the Rust one, gives the same results at
least as fast.

| File | What it is |
| --- | --- |
| `corpus.json` | The base settings, the recordings and the cases. Each case is a list of steps. |
| `audio/*.wav` | The recordings: macOS's Samantha voice, mono 16-bit 16 kHz. No real user's audio. |
| `make_audio.py` | Makes the recordings from `corpus.json`. Run it only to add or change one. |
| `parity.py` | `record` plays the cases against a server; `check` compares a recording with the baseline. |
| `baseline/python.json` | What the Python server produced, five rounds, with release-to-final timings. |

## Check a server

Run from the repository root with a Python that has `websockets` (the backend
venv does):

```sh
backend/venv/bin/python scripts/parity/parity.py record --out /tmp/candidate.json \
    --server-cmd "path/to/server --port {port} --data-dir {data_dir}"
backend/venv/bin/python scripts/parity/parity.py check /tmp/candidate.json
```

`record` starts the server once per round on a free port and an empty temporary
data dir, so it never touches the Kass you use. With no `--server-cmd` it starts
the Python backend in this checkout. `--url` uses a server that's already
running instead; it must have no captures, because the harness deletes every
capture after each case, along with the dictionary entries and styles the case
made.

`check` passes when:

- every case matches a result the baseline saw. The cleanup and command models
  sample their text (temperature 0.2), so a few cases word their output
  differently from round to round; the baseline keeps each variant it saw. In
  those cases only, new wording of the cleaned text with everything else the
  same is shown as `varies` and doesn't fail. In any case, a different
  `refinement_review` with everything else the same, the cleaned text
  included, is shown as `varies` too: the review checks the sampled draft, so
  it can flip (say, reject to none) while the text kept doesn't change.
  Anything else that differs fails, including new wording in a case that
  never varied.
- dictation release-to-final p50 and p95 are at most 10% above the baseline's
  (`--slower-ok` changes the margin).

Release-to-final is the time from sending `finish` after the last audio frame to
the `final` event. The audio is sent in 100 ms frames at real-time speed, as the
app does. Timings depend on the machine, so compare recordings made on the same
Mac; the baseline names the one it was made on.

## Record a new baseline

Only when the Python server's output is meant to change:

```sh
backend/venv/bin/python scripts/parity/parity.py record --out scripts/parity/baseline/python.json
```

## What a case records

Each step's result, after the harness swaps ids for labels (`@capture:0`,
`@style:Chat`, `<id>`) and drops fields that change every run (times, paths,
session ids):

- `take`: the final capture (raw and cleaned text, style, review), the voice
  edit or why it was declined, refinement errors, and the style events.
- `upload`, `command`: the capture the server returned.
- `dictionary`, `style_create`, `style_patch`, `style_assign`, `feedback`: the
  server's reply.
- `read`: the response of a `GET`, such as the dictionary after a correction.
