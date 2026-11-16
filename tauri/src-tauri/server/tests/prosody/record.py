"""Record what Python's prosody makes of the parity recordings, for tests/prosody.rs.

Each take is made of parity recordings and pauses, played at a sample rate
in frames as the app sends them into backend/services/prosody.py's
``Expression``. Its phrases are recognized by Whisper (turbo, from the
Hugging Face cache) with word alignments, as the worker returns them. For
each take: the voice frame by frame, the words placed in the recording, the
measurements saved with a capture, and the "!" and drawn-out words written
into its text and into a reworded copy.

The Samantha voice is calm, so the "!" cutoff and the stretch rule used for
the written text are looser than the defaults: what's checked is the
arithmetic, not the thresholds.

expression_learning is recorded on seeded made-up dictations and edits
(the recordings never say anything with energy), through ``gather``/``learn``
and through ``load`` from a database.

Run from the repository root:

    backend/venv/bin/python tauri/src-tauri/server/tests/prosody/record.py
"""

import asyncio
import json
import math
import random
import sys
import wave
from datetime import datetime, timedelta
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT))

from sqlalchemy import create_engine  # noqa: E402
from sqlalchemy.orm import sessionmaker  # noqa: E402

from backend.backends import get_stt_backend  # noqa: E402
from backend.backends.word_timing import Word  # noqa: E402
from backend.database import session as database_session  # noqa: E402
from backend.database.models import Base, Capture, CaptureFeedback  # noqa: E402
from backend.services import expression_learning, prosody  # noqa: E402

AUDIO = ROOT / "scripts" / "parity" / "audio"
OUT = Path(__file__).resolve().parent / "python.json"
CLIPS = sorted(path.name for path in AUDIO.glob("*.wav"))

# Looser than the defaults, so the Samantha voice gets some.
LAX = prosody.StretchRule(pace=1.2, louder_db=-3.0, plateau=(0.05, 1.5), words=frozenset(["so", "the"]))


def clip(name: str) -> np.ndarray:
    with wave.open(str(AUDIO / name)) as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1 and w.getsampwidth() == 2
        return np.frombuffer(w.readframes(w.getnframes()), dtype="<i2")


def at_rate(audio: np.ndarray, rate: int) -> np.ndarray:
    """16 kHz audio held sample by sample at ``rate``: integer math the Rust test repeats exactly."""
    count = len(audio) * rate // 16000
    return audio[(np.arange(count, dtype=np.int64) * 16000) // rate]


def take(parts: list) -> tuple[np.ndarray, list[tuple[int, int]]]:
    """Clips (a file name) and silences (seconds) at 16 kHz, and each clip's span in samples."""
    pieces, spans, at = [], [], 0
    for part in parts:
        piece = clip(part) if isinstance(part, str) else np.zeros(round(part * 16000), dtype="<i2")
        if isinstance(part, str):
            spans.append((at, at + len(piece)))
        pieces.append(piece)
        at += len(piece)
    return np.concatenate(pieces), spans


CASES = [dict(name=f"clip {name}", parts=[name], rate=16000, frame_ms=100) for name in CLIPS]
CASES += [
    dict(name="phrases at 48 kHz", parts=[CLIPS[0], 1.0, CLIPS[1], 0.8, CLIPS[2]], rate=48000, frame_ms=100),
    dict(name="phrases at 44.1 kHz", parts=[CLIPS[4], 1.2, CLIPS[5], 0.75, CLIPS[6]], rate=44100, frame_ms=100),
    dict(name="phrases at 24 kHz, odd frames", parts=[CLIPS[7], 0.9, CLIPS[8]], rate=24000, frame_ms=37),
    dict(name="phrases at 32 kHz, small frames", parts=[CLIPS[10], 0.7, CLIPS[11]], rate=32000, frame_ms=20),
    # A forced cut: the second phrase starts a second before the first ends.
    dict(name="overlapping phrases", parts=[CLIPS[12], CLIPS[13]], rate=48000, frame_ms=100, overlap=1.0),
]


class Aligned:
    """A phrase's alignment as Expression takes it: its words, worked out already."""

    def __init__(self, words):
        self._words = words

    def words(self):
        return self._words


def floats(values) -> list:
    return [None if math.isnan(value) else float(value) for value in values]


async def recognize(stt, pcm: np.ndarray) -> tuple[str, list[Word]]:
    alignments = []
    text = await stt.transcribe_array(
        pcm, 16000, language="en", model_size="turbo", check_speech=False, alignments=alignments
    )
    alignment = alignments[0] if alignments else None
    return text.strip(), alignment.words() if alignment is not None else []


def reworded(text: str) -> str:
    """Text a cleanup might return: the first word dropped and one word changed."""
    tokens = text.split()
    if len(tokens) > 3:
        tokens = tokens[1:]
        tokens[len(tokens) // 2] = "something"
    return " ".join(tokens)


async def heard(stt, case: dict) -> dict:
    """Play a take into an Expression with its phrases recognized."""
    audio16, spans = take(case["parts"])
    rate = case["rate"]
    phrases = []
    for index, (start, end) in enumerate(spans):
        if index and case.get("overlap"):
            start -= round(case["overlap"] * 16000)
        text, words = await recognize(stt, audio16[start:end])
        phrases.append(dict(start=start * rate // 16000, text=text, words=[[w.text, w.start, w.end] for w in words]))
    expression = prosody.Expression(rate)
    audio = at_rate(audio16, rate)
    frame = rate * case["frame_ms"] // 1000
    for offset in range(0, len(audio), frame):
        expression.feed(audio[offset : offset + frame])
    for phrase in phrases:
        words = [Word(*word) for word in phrase["words"]]
        expression.phrase(Aligned(words), phrase["start"], released=True)
    words = await expression.words()
    frames = expression.track.frames()
    return dict(case=case, phrases=phrases, words=words, frames=frames)


def written(text: str, voice, baseline) -> list:
    return [
        dict(text=s["text"], end=s["end"], energy=s["energy"], said_period=s["said_period"])
        for s in prosody.written_sentences(text, voice, baseline)
    ]


def record(take: dict, baseline, cutoff: float) -> dict:
    voice = prosody.Voice(take["frames"], take["words"])
    text = " ".join(phrase["text"] for phrase in take["phrases"])
    lax = prosody.measure_voice(voice, None, LAX)
    texts = []
    for variant in [text, reworded(text)]:
        texts.append(
            dict(
                text=variant,
                written=written(variant, voice, baseline),
                exclaimed=prosody.exclaim(variant, voice, baseline, cutoff),
                stretched=prosody.stretch(variant, voice, lax["stretched"]),
                expressed=prosody.stretch(prosody.exclaim(variant, voice, baseline, cutoff), voice, lax["stretched"]),
                saved=prosody._measure_voice(voice, baseline, variant),
            )
        )
    case = take["case"]
    return dict(
        name=case["name"],
        parts=case["parts"],
        rate=case["rate"],
        frame=case["rate"] * case["frame_ms"] // 1000,
        phrases=take["phrases"],
        hz=floats(take["frames"].hz),
        db=floats(take["frames"].db),
        words=[[w.text, w.start, w.end] for w in take["words"]],
        semitones=floats(voice.st),
        measured=prosody.measure_voice(voice, baseline),
        lax=lax,
        texts=texts,
    )


# -- expression_learning ------------------------------------------------------

VOCABULARY = "we did it that was great news so way good then home see you soon the cake is ready now".split()


def made_up(rng: random.Random, name: str) -> tuple[str, dict, str]:
    """A dictation's saved measurements and the text it returned."""
    sentences = []
    for _ in range(rng.randint(1, 3)):
        words = rng.sample(VOCABULARY, rng.randint(2, 5))
        words[0] = words[0].capitalize()
        sentences.append(" ".join(words) + rng.choice([".", ".", ".", "!", "?"]))
    shapes = []
    for sentence in sentences:
        for word in sentence.split()[1:-1]:
            shapes.append(
                [word, round(rng.uniform(0.5, 9), 2), round(rng.uniform(-3, 4), 2), round(rng.uniform(0, 1.2), 2)]
            )
    measured = dict(
        written=[dict(text=s, energy=round(rng.uniform(-1, 4.5), 2)) for s in sentences],
        shapes=shapes,
    )
    return name, measured, " ".join(sentences)


def edited(rng: random.Random, delivered: str) -> str:
    """The text as a user might leave it: endings swapped and words drawn out or written plainly."""
    out = []
    for token in delivered.split():
        if token.endswith(".") and rng.random() < 0.6:
            token = token[:-1] + "!"
        elif token.endswith("!") and rng.random() < 0.5:
            token = token[:-1] + "."
        core = token.rstrip(".!?")
        if core.isalpha() and rng.random() < 0.25:
            token = prosody.stretch_word(token, rng.choice([3, 4, 5]))
        out.append(token)
    if rng.random() < 0.2:
        out.insert(rng.randint(0, len(out)), "really")
    return " ".join(out)


def learning_case(seed: int) -> dict:
    rng = random.Random(seed)
    captures = [made_up(rng, f"c{seed}-{index}") for index in range(rng.choice([4, 12, 60, 120]))]
    edits, redictated = {}, set()
    for name, _, delivered in captures:
        if rng.random() < 0.3:
            edits[name] = edited(rng, delivered)
            if rng.random() < 0.3:
                redictated.add(name)
    # A few drawn out in the delivered text, written plainly by the user.
    for name, measured, delivered in captures[:3]:
        if name in edits and measured["shapes"]:
            word = measured["shapes"][0][0]
            stretched = delivered.replace(word, prosody.stretch_word(word, 4), 1)
            captures[captures.index((name, measured, delivered))] = (name, measured, stretched)
    learned = expression_learning.learn(expression_learning.gather(captures, edits, frozenset(redictated)))
    return dict(
        seed=seed,
        captures=[[name, measured, delivered] for name, measured, delivered in captures],
        edits=edits,
        redictated=sorted(redictated),
        learned=learned_json(learned),
    )


def learned_json(learned) -> dict:
    rule = learned.rule
    return dict(
        cutoff=learned.cutoff,
        pace=rule.pace,
        louder_db=rule.louder_db,
        plateau=list(rule.plateau),
        words=sorted(rule.words),
    )


def database_case(seed: int) -> dict:
    """Rows in captures and capture_feedback, and what load and load_baseline read from them."""
    rng = random.Random(seed)
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine)
    database_session.SessionLocal = factory
    start = datetime(2026, 11, 1, 9, 0, 0)
    captures, reports = [], []
    with factory() as db:
        for index in range(40):
            name, measured, delivered = made_up(rng, f"d{index}")
            measured["sentences"] = [
                dict(text=s["text"], level=round(rng.uniform(5, 20), 2), span=round(rng.uniform(1, 9), 2),
                     loudness=round(rng.uniform(-40, -15), 2))
                for s in measured["written"]
            ]
            saved = json.dumps(measured) if index % 13 else "not json"
            if index % 17 == 5:
                saved = None
            created = (start + timedelta(minutes=rng.randint(0, 5000))).isoformat(sep=" ")
            captures.append(dict(id=name, prosody=saved, refined=delivered if index % 9 else None, created_at=created))
            db.add(
                Capture(
                    id=name, audio_path=f"captures/{name}.wav", source="dictation", transcript_raw=delivered.lower(),
                    transcript_refined=delivered if index % 9 else None, stt_model="turbo", prosody=saved,
                    created_at=datetime.fromisoformat(created),
                )
            )
            for report in range(rng.choice([0, 0, 1, 2])):
                feedback = dict(
                    id=f"f{index}-{report}", capture_id=name, target=rng.choice(["refined", "refined", "raw"]),
                    expected_text=edited(rng, delivered), source=rng.choice(["manual", "voice_fix", "redictation"]),
                    created_at=(datetime.fromisoformat(created) + timedelta(minutes=report + 1)).isoformat(sep=" "),
                )
                reports.append(feedback)
                db.add(
                    CaptureFeedback(
                        id=feedback["id"], capture_id=name, target=feedback["target"],
                        expected_text=feedback["expected_text"], snapshot="{}", source=feedback["source"],
                        created_at=datetime.fromisoformat(feedback["created_at"]),
                    )
                )
        db.commit()
    baseline = prosody.load_baseline()
    learned = expression_learning.load()
    engine.dispose()
    return dict(
        seed=seed,
        captures=captures,
        reports=reports,
        baseline=None if baseline is None else dict(
            level=list(baseline.level), span=list(baseline.span), loudness=list(baseline.loudness)
        ),
        learned=learned_json(learned),
    )


async def main() -> None:
    stt = get_stt_backend()
    takes = [await heard(stt, case) for case in CASES]
    found = []
    for take in takes:
        voice = prosody.Voice(take["frames"], take["words"])
        found += prosody.measure_voice(voice, None)["sentences"]
    baseline = prosody.Baseline.of(found)
    assert baseline is not None, f"only {len(found)} sentences"
    energies = sorted(
        s["energy"] for take in takes for s in prosody.written_sentences(
            " ".join(p["text"] for p in take["phrases"]), prosody.Voice(take["frames"], take["words"]), baseline
        )
    )
    cutoff = float(np.median(energies))
    out = dict(
        clips=CLIPS,
        baseline_sentences=found,
        baseline=dict(level=list(baseline.level), span=list(baseline.span), loudness=list(baseline.loudness)),
        cutoff=cutoff,
        lax=dict(pace=LAX.pace, louder_db=LAX.louder_db, plateau=list(LAX.plateau), words=sorted(LAX.words)),
        takes=[record(take, baseline, cutoff) for take in takes],
        learning=[learning_case(seed) for seed in range(12)],
        database=[database_case(seed) for seed in range(3)],
    )
    OUT.write_text(json.dumps(out, separators=(",", ":")) + "\n")
    print(f"Recorded {len(out['takes'])} takes to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    asyncio.run(main())
