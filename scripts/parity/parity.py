#!/usr/bin/env python3
"""Replay the parity corpus against a Kass server and diff the results.

The corpus (corpus.json, audio/) is a fixed set of takes with the settings,
dictionary entries, styles and corrections each one needs. `record` plays it
against a server over the public HTTP and WebSocket API, the way the app does
(100 ms PCM frames at real-time speed), and saves what the server produced:
transcripts, cleaned text, voice edits, commands, dictionary and correction
results, and how long each dictation took from release to its final text.
`check` compares a recording with a baseline (baseline/python.json, from
today's Python server) and fails on any difference.

Record against a server the harness starts on an empty data dir and a free
port, once per round (default: the Python backend in this checkout):

    backend/venv/bin/python scripts/parity/parity.py record --out /tmp/rust.json \\
        --server-cmd "target/release/kass-server --port {port} --data-dir {data_dir}"
    backend/venv/bin/python scripts/parity/parity.py check /tmp/rust.json

or against one already running (it must have no captures; after each case the
harness deletes every capture and the dictionary entries and styles the case
made):

    backend/venv/bin/python scripts/parity/parity.py record --url http://127.0.0.1:18000 --out /tmp/x.json

Never point it at the Kass you use: it deletes every capture after each case.

`check` passes when every case's result matches one the baseline saw and the
dictation release-to-final p50 and p95 are no more than --slower-ok (default
10%) above the baseline's. The cleanup and command models sample their text,
so in a case whose wording varied between the baseline's rounds, new wording
is shown as "varies" and allowed when everything else matches. A review of the
cleanup's draft that differs while everything else, the text kept included,
matches is shown as "varies" in any case. Needs the
`websockets` package (in backend/venv).
"""

import argparse
import asyncio
import contextlib
import difflib
import json
import os
import platform
import re
import shlex
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
CORPUS = ROOT / "corpus.json"
AUDIO = ROOT / "audio"
BASELINE = ROOT / "baseline" / "python.json"
FRAME_MS = 100
FINAL_TIMEOUT = 180
PYTHON_SERVER = "{python} -m backend.main --port {port} --data-dir {data_dir}"
DROPPED_KEYS = {
    "created_at",
    "updated_at",
    "audio_path",
    "session_id",
    "revision",
    "covered_samples",
    "mac_id",
    "mac_name",
    "cache_mb_per_style",
}
MODEL_TEXT = {"transcript_refined", "refinement_review"}
# The check of the cleanup's draft. The draft is sampled, so the review can
# change while the text kept stays the same (a rejected draft falls back to a
# transcript an accepted one matched).
REVIEW = {"refinement_review"}
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


class ServerError(RuntimeError):
    pass


class Client:
    """The server's HTTP API, and what the current case has made."""

    def __init__(self, url: str):
        self.url = url.rstrip("/")
        self.labels: dict[str, str] = {}
        self.captures: list[str] = []
        self.styles: dict[str, str] = {}

    def call(self, method: str, path: str, body=None, *, raw: bytes | None = None, headers=None, ok_missing=False):
        data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
        request = urllib.request.Request(self.url + path, data=data, method=method)
        if body is not None:
            request.add_header("Content-Type", "application/json")
        for key, value in (headers or {}).items():
            request.add_header(key, value)
        try:
            with urllib.request.urlopen(request, timeout=FINAL_TIMEOUT) as response:
                text = response.read().decode()
                return json.loads(text) if text else None
        except urllib.error.HTTPError as error:
            if ok_missing and error.code == 404:
                return None
            detail = error.read().decode(errors="replace")
            raise ServerError(f"{method} {path}: {error.code} {detail}") from error

    def get(self, path):
        return self.call("GET", path)

    def note_capture(self, capture_id: str | None) -> None:
        if capture_id and capture_id not in self.labels:
            self.labels[capture_id] = f"@capture:{len(self.captures)}"
            self.captures.append(capture_id)

    def refresh_styles(self) -> dict:
        listing = self.get("/writing-styles")
        self.styles = {style["name"]: style["id"] for style in listing["styles"]}
        for style in listing["styles"]:
            self.labels[style["id"]] = f"@style:{style['name']}"
        return listing

    def style_id(self, name: str) -> str:
        listing = self.refresh_styles()
        if name == "@default":
            return next(style["id"] for style in listing["styles"] if style["is_default"])
        return self.styles[name]

    def resolve(self, text: str) -> str:
        def capture(match):
            return self.captures[int(match.group(1))]

        return re.sub(r"@capture:(\d+)", capture, text)

    def normalize(self, value):
        if isinstance(value, dict):
            return {key: self.normalize(item) for key, item in value.items() if key not in DROPPED_KEYS}
        if isinstance(value, list):
            return [self.normalize(item) for item in value]
        if isinstance(value, str):
            if value in self.labels:
                return self.labels[value]
            return UUID.sub("<id>", value)
        return value


def read_pcm(name: str) -> tuple[int, bytes]:
    with wave.open(str(AUDIO / f"{name}.wav")) as audio:
        if audio.getnchannels() != 1 or audio.getsampwidth() != 2:
            raise ValueError(f"{name}.wav must be mono 16-bit PCM")
        return audio.getframerate(), audio.readframes(audio.getnframes())


def take_output(capture: dict) -> str:
    return capture.get("transcript_refined") or capture.get("transcript_raw") or ""


async def stream_take(client: Client, spec: dict, previous: dict | None) -> tuple[dict, float | None]:
    """One dictation over /captures/stream, as the app sends it. Returns the
    result and the seconds from release (finish sent) to the final event."""
    import websockets

    rate, pcm = read_pcm(spec["audio"])
    source = spec.get("source", "dictation")
    ws_url = re.sub(r"^http", "ws", client.url) + "/captures/stream"
    events: list[dict] = []
    async with websockets.connect(ws_url, max_size=None, open_timeout=30) as socket_:
        await socket_.send(
            json.dumps(
                {
                    "type": "start",
                    "protocol_version": 1,
                    "sample_rate": rate,
                    "channels": 1,
                    "encoding": "pcm_s16le",
                    "source": source,
                }
            )
        )
        ready = json.loads(await asyncio.wait_for(socket_.recv(), 60))
        if ready.get("type") != "ready":
            return {"error": ready.get("message", ready.get("type"))}, None
        if "app" in spec:
            await socket_.send(json.dumps({"type": "app", **spec["app"]}))
        if "context" in spec:
            await socket_.send(json.dumps({"type": "context", "before": spec["context"]}))
        if "last_take" in spec:
            last = spec["last_take"]
            if last == "@previous":
                if previous is None:
                    raise ValueError("@previous needs an earlier take in the case")
                text = take_output(previous)
                message = {"type": "last_take", "text": text, "capture_id": previous["id"], "own_chars": len(text)}
            else:
                message = {"type": "last_take", "text": last["text"], "capture_id": None, "own_chars": 0}
            await socket_.send(json.dumps(message))
        if "selection" in spec:
            await socket_.send(json.dumps({"type": "selection", "text": spec["selection"]}))

        ended = asyncio.get_running_loop().create_future()

        async def receive():
            try:
                async for message in socket_:
                    event = json.loads(message)
                    events.append(event)
                    if event.get("type") in {"final", "error"}:
                        if not ended.done():
                            ended.set_result((time.perf_counter(), event))
                        return
            except Exception as error:
                if not ended.done():
                    ended.set_exception(error)
            if not ended.done():
                ended.set_exception(ServerError("Connection closed before the final event"))

        receiver = asyncio.create_task(receive())
        frame_bytes = rate * FRAME_MS // 1000 * 2
        started = time.perf_counter()
        for sequence, offset in enumerate(range(0, len(pcm), frame_bytes)):
            frame = pcm[offset : offset + frame_bytes]
            deadline = started + (offset + len(frame)) / (rate * 2)
            await asyncio.sleep(max(0.0, deadline - time.perf_counter()))
            if ended.done():
                break
            await socket_.send(struct.pack("<II", sequence, offset // 2) + frame)
        released = time.perf_counter()
        if not ended.done():
            finish = {"type": "finish"}
            if "app" in spec:
                finish["app"] = spec["app"]
            await socket_.send(json.dumps(finish))
        try:
            at, final = await asyncio.wait_for(ended, FINAL_TIMEOUT)
        finally:
            receiver.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await receiver
    if final["type"] == "error":
        return {"error": final.get("message")}, None
    capture = final["capture"]
    client.note_capture(capture["id"])
    result = {
        "capture": capture,
        "refinement_complete": final.get("refinement_complete"),
        "refinement_error": final.get("refinement_error"),
        "degraded_reason": final.get("degraded_reason"),
        "edit_announced": any(event.get("type") == "edit" for event in events),
        "styles_announced": [event.get("name") for event in events if event.get("type") == "style"],
    }
    if "edit" in final:
        result["edit"] = final["edit"]
    return result, at - released


def upload(client: Client, spec: dict) -> tuple[dict, float]:
    boundary = uuid.uuid4().hex
    audio = (AUDIO / f"{spec['audio']}.wav").read_bytes()
    fields = [("source", spec.get("source", "dictation"))]
    body = b""
    for name, value in fields:
        body += f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode()
    body += (
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{spec["audio"]}.wav"\r\n'
        "Content-Type: audio/wav\r\n\r\n"
    ).encode()
    body += audio + f"\r\n--{boundary}--\r\n".encode()
    started = time.perf_counter()
    capture = client.call(
        "POST", "/captures", raw=body, headers={"Content-Type": f"multipart/form-data; boundary={boundary}"}
    )
    client.note_capture(capture["id"])
    if spec.get("refine"):
        capture = client.call("POST", f"/captures/{capture['id']}/refine", {})
    return {"capture": capture}, time.perf_counter() - started


async def run_step(client: Client, step: dict, state: dict) -> dict:
    kind, spec = next(iter(step.items()))
    if kind == "take":
        result, seconds = await stream_take(client, spec, state.get("previous"))
        if "capture" in result:
            state["previous"] = result["capture"]
        if seconds is not None:
            state["timings"].append(
                {"case": state["case"], "source": spec.get("source", "dictation"), "seconds": seconds}
            )
        return {"take": result}
    if kind == "upload":
        result, seconds = upload(client, spec)
        state["previous"] = result["capture"]
        return {"upload": result}
    if kind == "command":
        capture = client.call("POST", "/commands/run", spec)
        client.note_capture(capture["id"])
        return {"command": capture}
    if kind == "dictionary":
        return {"dictionary": client.call("POST", "/dictionary", spec)}
    if kind == "style_create":
        style = client.call("POST", "/writing-styles", {"name": spec["name"]})
        client.labels[style["id"]] = f"@style:{style['name']}"
        patch = {key: value for key, value in spec.items() if key != "name"}
        if patch:
            style = client.call("PATCH", f"/writing-styles/{style['id']}", patch)
        return {"style_create": style}
    if kind == "style_patch":
        patch = {key: value for key, value in spec.items() if key != "style"}
        return {"style_patch": client.call("PATCH", f"/writing-styles/{client.style_id(spec['style'])}", patch)}
    if kind == "style_assign":
        body = {"style_id": client.style_id(spec["style"]), "app_name": spec.get("app_name")}
        listing = client.call("PUT", f"/writing-styles/apps/{spec['bundle_id']}", body)
        return {"style_assign": listing["apps"]}
    if kind == "feedback":
        capture_id = client.resolve(spec["capture"])
        capture = client.get(f"/captures/{capture_id}")
        body = {
            "target": spec["target"],
            "expected_text": spec["expected"],
            "snapshot": capture,
            "source": spec.get("source", "manual"),
        }
        return {"feedback": client.call("POST", f"/captures/{capture_id}/feedback", body)}
    if kind == "read":
        return {"read": spec, "response": client.get(client.resolve(spec))}
    if kind == "settle":
        await asyncio.sleep(spec)
        return {"settle": spec}
    raise ValueError(f"Unknown step {kind!r}")


def reset(client: Client, initial: dict) -> None:
    """Delete what a case made: captures with their reports, new dictionary
    entries and new styles; put the starting entries and styles back as they
    were."""
    while True:
        listing = client.get("/captures?limit=200")
        if not listing["items"]:
            break
        for capture in listing["items"]:
            for report in client.get(f"/captures/{capture['id']}/feedback"):
                client.call("DELETE", f"/captures/{capture['id']}/feedback/{report['id']}", ok_missing=True)
            client.call("DELETE", f"/captures/{capture['id']}", ok_missing=True)
    entries = {entry["id"]: entry for entry in initial["entries"]}
    for entry in client.get("/dictionary")["entries"]:
        if entry["id"] not in entries:
            client.call("DELETE", f"/dictionary/{entry['id']}", ok_missing=True)
            continue
        fields = ("written", "spoken", "places", "match_sound")
        start = entries[entry["id"]]
        if any(entry[key] != start[key] for key in fields):
            client.call("PATCH", f"/dictionary/{entry['id']}", {key: start[key] for key in fields})
    initial_styles = initial["styles"]
    original = {style["id"]: style for style in initial_styles}
    default = next(style["id"] for style in initial_styles if style["is_default"])
    client.call("PATCH", f"/writing-styles/{default}", {"is_default": True})
    for style in client.get("/writing-styles")["styles"]:
        if style["id"] not in original:
            client.call("DELETE", f"/writing-styles/{style['id']}", ok_missing=True)
    for style in initial_styles:
        fields = ("name", "punctuation_style", "preserve_technical", "description")
        client.call("PATCH", f"/writing-styles/{style['id']}", {key: style[key] for key in fields})
    client.labels = {key: value for key, value in client.labels.items() if value.startswith("@style:")}
    client.captures = []


def assert_empty(client: Client) -> None:
    captures = client.get("/captures?limit=1")["total"]
    if captures:
        raise SystemExit(
            f"The server already has {captures} captures. "
            "Point the harness at a server on a fresh data dir: it deletes every capture it finds."
        )


async def run_round(url: str, corpus: dict, cases: list[dict], log) -> tuple[dict, list[dict], float | None]:
    client = Client(url)
    assert_empty(client)
    client.call("PUT", "/settings/captures", corpus["settings"])
    initial = {"styles": client.refresh_styles()["styles"], "entries": client.get("/dictionary")["entries"]}
    warm = {"case": "warmup", "timings": []}
    await run_step(client, {"take": {"audio": "prose-short"}}, warm)
    warmup = warm["timings"][0]["seconds"] if warm["timings"] else None
    log(f"  warm-up take: {warmup:.3f} s release-to-final" if warmup is not None else "  warm-up take failed")
    reset(client, initial)
    results: dict[str, list] = {}
    timings: list[dict] = []
    for case in cases:
        client.call("PUT", "/settings/captures", {**corpus["settings"], **case.get("settings", {})})
        client.refresh_styles()
        state = {"case": case["id"], "timings": timings}
        steps = []
        for step in case["steps"]:
            try:
                steps.append(await run_step(client, step, state))
            except (ServerError, OSError) as error:
                steps.append({"failed": next(iter(step)), "error": str(error)})
        client.refresh_styles()
        results[case["id"]] = client.normalize(steps)
        log(f"  {case['id']}: {summary(results[case['id']])}")
        reset(client, initial)
    return results, timings, warmup


def summary(steps: list[dict]) -> str:
    texts = []
    for step in steps:
        body = step.get("take") or step.get("upload") or {}
        if "capture" in body:
            texts.append(repr(take_output(body["capture"])))
        elif "error" in body or "error" in step:
            texts.append(f"error: {body.get('error') or step.get('error')}")
        elif "command" in step:
            texts.append(repr(take_output(step["command"])))
    return " | ".join(texts) or "(no text)"


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_healthy(url: str, process: subprocess.Popen, timeout: float = 180) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise SystemExit(f"The server exited with {process.returncode} before it was ready")
        with (
            contextlib.suppress(OSError, urllib.error.URLError),
            urllib.request.urlopen(url + "/health", timeout=2) as response,
        ):
            if response.status == 200:
                return
        time.sleep(0.5)
    raise SystemExit(f"The server wasn't ready after {timeout:.0f} s")


@contextlib.contextmanager
def launched(command: str, log_path: Path):
    port = free_port()
    with tempfile.TemporaryDirectory(prefix="kass-parity-data-") as data_dir:
        argv = shlex.split(command.format(port=port, data_dir=data_dir, python=sys.executable))
        env = {**os.environ, "HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1"}
        with open(log_path, "ab") as log:
            process = subprocess.Popen(argv, cwd=REPO, stdout=log, stderr=subprocess.STDOUT, env=env)
        url = f"http://127.0.0.1:{port}"
        try:
            wait_healthy(url, process)
            yield url
        finally:
            process.terminate()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def timing_summary(timings: list[dict]) -> dict:
    out = {}
    for source in sorted({timing["source"] for timing in timings}):
        values = [timing["seconds"] for timing in timings if timing["source"] == source]
        out[source] = {
            "takes": len(values),
            "p50": percentile(values, 0.5),
            "p95": percentile(values, 0.95),
            "max": max(values),
        }
    return out


def record(args) -> int:
    corpus = json.loads(CORPUS.read_text())
    cases = corpus["cases"]
    if args.cases:
        wanted = set(args.cases.split(","))
        cases = [case for case in cases if case["id"] in wanted]
        if missing := wanted - {case["id"] for case in cases}:
            raise SystemExit(f"Not in the corpus: {', '.join(sorted(missing))}")
    rounds, timings, warmups = [], [], []
    log_path = Path(tempfile.gettempdir()) / f"kass-parity-{time.strftime('%Y%m%d-%H%M%S')}.server.log"
    if not args.url:
        print(f"Server output: {log_path}", file=sys.stderr)
    for number in range(args.rounds):
        print(f"Round {number + 1} of {args.rounds}", file=sys.stderr)

        def log(line):
            print(line, file=sys.stderr)

        if args.url:
            results, round_timings, warmup = asyncio.run(run_round(args.url, corpus, cases, log))
        else:
            command = args.server_cmd or PYTHON_SERVER
            with launched(command, log_path) as url:
                results, round_timings, warmup = asyncio.run(run_round(url, corpus, cases, log))
        rounds.append(results)
        timings.extend({**timing, "round": number} for timing in round_timings)
        warmups.append(warmup)
    variants = {}
    for case in cases:
        seen = []
        for results in rounds:
            if results[case["id"]] not in seen:
                seen.append(results[case["id"]])
        variants[case["id"]] = seen
    output = {
        "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "server": args.url or args.server_cmd or PYTHON_SERVER,
        "git_commit": subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True
        ).stdout.strip(),
        "machine": {"platform": platform.platform(), "processor": platform.processor()},
        "rounds": args.rounds,
        "cases": variants,
        "timings": {
            "release_to_final": timing_summary(timings),
            "warmup_seconds": warmups,
            "takes": timings,
        },
    }
    Path(args.out).write_text(json.dumps(output, indent=2, ensure_ascii=False) + "\n")
    unstable = [case for case, seen in variants.items() if len(seen) > 1]
    print(f"Saved {args.out}", file=sys.stderr)
    if unstable:
        print(f"Varied between rounds: {', '.join(unstable)}", file=sys.stderr)
    print(json.dumps(output["timings"]["release_to_final"], indent=2), file=sys.stderr)
    return 0


def without(value, keys):
    """The result without the fields named in `keys`, at any depth."""
    if isinstance(value, dict):
        return {key: without(item, keys) for key, item in value.items() if key not in keys}
    if isinstance(value, list):
        return [without(item, keys) for item in value]
    return value


def model_free(value):
    """The result without the text the cleanup and command models sample."""
    return without(value, MODEL_TEXT)


def dump(value) -> str:
    return json.dumps(value, indent=1, ensure_ascii=False)


def check(args) -> int:
    baseline = json.loads(Path(args.baseline).read_text())
    candidate = json.loads(Path(args.results).read_text())
    failures = 0
    for case, expected in baseline["cases"].items():
        if case not in candidate["cases"]:
            if not args.partial:
                print(f"MISSING  {case}")
                failures += 1
            continue
        verdict, shown, why = "ok", None, set()
        for got in candidate["cases"][case]:
            if got in expected:
                continue
            if without(got, REVIEW) in [without(seen, REVIEW) for seen in expected]:
                verdict, shown = "varies", got
                why.add("cleanup review only")
                continue
            if len(expected) > 1 and model_free(got) in [model_free(seen) for seen in expected]:
                verdict, shown = "varies", got
                why.add("model wording only")
                continue
            verdict, shown = "DIFFERS", got
            failures += 1
            break
        notes = []
        if len(expected) > 1:
            notes.append(f"{len(expected)} variants in the baseline")
        if verdict == "varies":
            notes.extend(sorted(why))
        print(f"{verdict:<8} {case}" + (f" ({'; '.join(notes)})" if notes else ""))
        if shown is not None:
            closest = max(expected, key=lambda seen: difflib.SequenceMatcher(None, dump(seen), dump(shown)).ratio())
            want, have = dump(closest).splitlines(), dump(shown).splitlines()
            for line in difflib.unified_diff(want, have, "baseline", "candidate", lineterm="", n=2):
                print("    " + line)
    for case in candidate["cases"]:
        if case not in baseline["cases"]:
            print(f"NEW      {case} (not in the baseline)")
    want = baseline["timings"]["release_to_final"].get("dictation", {})
    have = candidate["timings"]["release_to_final"].get("dictation", {})
    for key in ("p50", "p95"):
        if want.get(key) is None or have.get(key) is None:
            continue
        limit = want[key] * (1 + args.slower_ok)
        verdict = "ok" if have[key] <= limit else "SLOWER"
        failures += verdict != "ok"
        print(f"{verdict:<8} dictation release-to-final {key}: {have[key]:.3f} s (baseline {want[key]:.3f} s)")
    print("PASS" if not failures else f"FAIL: {failures} difference(s)")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    rec = commands.add_parser("record", help="replay the corpus and save the results")
    rec.add_argument("--out", required=True, help="results file to write")
    where = rec.add_mutually_exclusive_group()
    where.add_argument("--url", help="a running server on an empty data dir")
    where.add_argument(
        "--server-cmd",
        help="command that starts a server; {port}, {data_dir} and {python} are filled in "
        f"(default: {PYTHON_SERVER!r})",
    )
    rec.add_argument("--rounds", type=int, default=5, help="times to play the corpus (default 5)")
    rec.add_argument("--cases", help="comma-separated case ids (default: all)")
    chk = commands.add_parser("check", help="diff a recording against the baseline")
    chk.add_argument("results")
    chk.add_argument("--baseline", default=str(BASELINE))
    chk.add_argument("--slower-ok", type=float, default=0.10, help="allowed p50/p95 slowdown (default 0.10)")
    chk.add_argument("--partial", action="store_true", help="don't fail on cases the recording skipped")
    args = parser.parse_args()
    return record(args) if args.command == "record" else check(args)


if __name__ == "__main__":
    sys.exit(main())
