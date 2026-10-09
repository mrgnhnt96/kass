"""Bounded rolling-window dictation, using the existing file-based STT backend.

Recognition windows end on pauses where possible. Full audio is archived once;
only a bounded backlog and one inference window are kept in memory.

Only work the final result uses runs: phrases cut at pauses, then the rest at
finish. There are no provisional previews or speculative cleanups. No client
shows them and they were rarely reused.

Cleanup is sentence-aware (docs/plans/SENTENCE_AWARE_CLEANUP.md). A pause is
often the speaker thinking mid-sentence, so each phrase is added to the open
tail, the raw words since the last settled sentence, and the whole tail is
cleaned again. Every sentence of that cleanup but the last is settled: final,
never cleaned again. After release only the open tail is cleaned. A cleanup
still running at release that the rest of the speech would replace is stopped
between tokens rather than waited for.

A client that asks for it (``"provisional": true`` in start) also gets
``provisional`` events after release: the part of the final text that is
unlikely to change, while the last phrase is still being cleaned up. It is a
prediction, not a promise; ``final`` is the only authoritative text
(docs/plans/STREAMING_INSERTION.md).

The target app arrives in an ``app`` message right after key-down, before
any phrase is cleaned, and picks the writing style the session cleans up in
(docs/plans/PER_APP_STYLE.md). A dictation that opens by asking
for a style by name ("use formal mode", "make this more personal") is written
in that style instead; the words themselves are dropped.

A dictation that opens with "fix that", "edit" or "Kass" and says what to
change is a voice edit of the text before the caret, whoever wrote it, which
the client sends in a ``last_take`` message with how much of its end Kass's
last take wrote (docs/plans/VOICE_EDITS.md). Its words are never
cleaned up; at finish they become the take's text before and after the
edit, which the client applies.

A ``command`` session (docs/plans/COMMAND_MODE.md) records a spoken
instruction for text selected in another app. It is recognized like a
dictation but never cleaned up; the client sends the selection while the user
speaks, the model is prefilled with it, and at finish the selection is
rewritten by the instruction.
"""

import asyncio
import contextlib
import json
import logging
import re
import struct
import threading
import time
import uuid
import wave

import numpy as np

from .. import config, models
from ..backends.qwen_llm_backend import generation_hint, generation_listener, generation_stop
from ..database import Capture
from ..utils import memory
from . import dictionary as dictionaries, expression_learning, prosody, voice_edits
from .captures import _to_response
from .commands import (
    MAX_SELECTION_CHARS,
    ensure_model_ready,
    prefill,
    record_command,
    resolve_instruction,
    rewrite,
    settings_transforms,
)
from .content_check import Verdict, check_refinement, summarize_reviews
from .known_names import known_names
from .phrase_seams import (
    close_phrase,
    continue_after_seam,
    continue_phrase,
    continues_sentence,
    join_phrases,
    load_word_data,
    match_raw_start,
    open_phrase,
    strip_pause_mark,
)
from .refinement import (
    RefinementFlags,
    keep_said_punctuation,
    prepare_refinement,
    refine_transcript,
    style_first_word,
)
from .sentence_tail import MAX_OPEN_WORDS, settle
from .speech_detect import SpeechDetector
from .spoken_punctuation import period_after_closers
from .styles import flags_for, snapshot as styles_snapshot, spoken_style
from .transcribe import get_whisper_model
from .voice_commands import mark_commands
from .writing_style import apply_learned, apply_style, habits, is_ready

logger = logging.getLogger(__name__)
# Words held back while learned corrections are active: a rule replaces up to
# three words and needs the word after them as context.
CORRECTION_HOLDBACK = 4
MAX_FRAME_BYTES = 65536
MAX_SECONDS = 3600
# Whisper keeps at most ~224 prompt tokens; this stays comfortably inside it.
PHRASE_CONTEXT_CHARS = 600
# The field's text kept from before the caret: enough for a sentence and the
# names in it.
FIELD_CONTEXT_CHARS = 600
# The end of the last take a voice edit may change (docs/plans/VOICE_EDITS.md).
LAST_TAKE_CHARS = 1000
# The opening words are checked for a style request ("make this formal") before
# the first phrase ends, which may be many seconds away: at the first short
# pause after this much speech, or once this much was said without one.
STYLE_PEEK_AFTER_S = 1.0
STYLE_PEEK_PAUSE_S = 0.2
STYLE_PEEK_BY_S = 2.5
_CORRECTION_CUE = re.compile(r"\b(actually|no wait|no actually|make that|I mean|scratch that)\b", re.I)


def join_overlap(prefix: str, tail: str) -> str:
    """Remove only matching boundary tokens from overlapping recognition windows."""
    left, right = prefix.split(), tail.split()

    def key(value: str) -> str:
        return re.sub(r"[^\w]", "", value).lower()

    for count in range(min(32, len(left), len(right)), 0, -1):
        if [key(w) for w in left[-count:]] == [key(w) for w in right[:count]]:
            return " ".join(left + right[count:])
    return " ".join(part for part in (prefix, tail) if part)


def _has_corrections() -> bool:
    from . import correction_learning

    return bool(correction_learning._compiled)


def stable_prefix(text: str, holdback: int) -> str:
    """The start of ``text`` minus its last ``holdback`` words and the punctuation before them.

    Those can still change: the model may be mid-word, and seam punctuation,
    learned habits and learned corrections all look at the following word.
    """
    words = list(re.finditer(r"\S+", text))
    if len(words) < holdback or (holdback and len(words) == holdback):
        return ""
    kept = text[: words[-holdback].start()] if holdback else text
    return re.sub(r"[^\w\s]+$", "", kept.rstrip()).rstrip()


def guard_phrase_refinement(raw: str, refined: str, flags: RefinementFlags) -> tuple[str, Verdict]:
    """Keep a phrase's cleanup unless the content check rejects it.

    Restructuring is allowed. A cleanup that may have added or left out
    content is kept and flagged for review; one that answered or obeyed the
    dictation, or changed a negation, number or technical term, falls back to
    the prepared transcript.
    """
    return check_refinement(raw, refined, flags)


class StreamingCapture:
    def __init__(self, start: dict, settings, send):
        if start.get("type") != "start" or start.get("protocol_version") != 1:
            raise ValueError("Expected protocol version 1 start message")
        self.rate = start.get("sample_rate")
        if not isinstance(self.rate, int) or not 16000 <= self.rate <= 48000:
            raise ValueError("sample_rate must be between 16000 and 48000")
        if start.get("channels") != 1 or start.get("encoding") != "pcm_s16le":
            raise ValueError("Audio must be mono pcm_s16le")
        # Protocol addition: the client can show provisional cleaned text.
        self.provisional = start.get("provisional") is True
        # Protocol addition: the first milliseconds may hold the client's own
        # start cue, picked up by the microphone.
        start_cue_ms = start.get("start_cue_ms", 0)
        if not isinstance(start_cue_ms, int) or isinstance(start_cue_ms, bool) or not 0 <= start_cue_ms <= 1000:
            raise ValueError("start_cue_ms must be between 0 and 1000")
        self.shown = ""
        # Set from the app command at key-down (or finish, from older clients).
        self.app_bundle_id = None
        self.app_name = None
        self.app_category = None
        # Whether a cleanup has begun; the style is fixed from then on.
        self.cleanup_started = False
        # The style the user asked for by name at the start, over the app's,
        # and whether a recognized phrase said so (not only an early peek).
        self.spoken = None
        self.spoken_confirmed = False
        self.style_announced = None
        # Early looks at the opening words, and the speech (samples) at the last one.
        self.style_peeks = 0
        self.style_peeked_at = 0
        # The field's text before the caret, from the context command, and
        # whether the dictation continues its sentence
        # (docs/plans/MID_SENTENCE_DICTATION.md). Never saved.
        self.field_before = ""
        self.continues = False
        # The text before the caret, which a voice edit may change, from the
        # last_take command, and how many of its last chars Kass's last take
        # wrote, with that take's capture. Never saved but in an edit's.
        self.last_take = None
        self.last_take_capture_id = None
        self.last_take_own_chars = 0
        # A voice edit: the take opened with one, and whether the client was
        # told while the user speaks. At finish, what it changes.
        self.editing = False
        self.edit_announced = False
        self.edit = None
        # Words the user capitalizes mid-sentence, loaded when the run starts.
        self.names = frozenset()
        # The user's dictionary (docs/plans/DICTIONARIES.md): the global and
        # default style's entries until the app is known. Read once per change.
        self.dictionary = dictionaries.for_app(None)
        self.names_loading = None
        self.source = start.get("source", "dictation")
        if not isinstance(self.source, str) or self.source not in {"dictation", "recording", "command"}:
            raise ValueError("Invalid streaming capture source")
        # Command sessions: the selected text, from the selection command, and
        # what ran on it.
        self.selection = None
        self.prefilling = None
        self.command = None
        self.settings = settings
        # Whether a dictation may be a voice edit: the Voice edits setting.
        # Fixed for the take, like the rest of its settings.
        self.edits_on = not self.is_command and settings.voice_edits
        self.language = start.get("language", settings.language)
        if self.language is not None and (
            not isinstance(self.language, str) or not re.fullmatch(r"[A-Za-z-]{2,32}", self.language)
        ):
            raise ValueError("Invalid language")
        if self.language == "auto":
            self.language = None
        self.stt_model = start.get("stt_model") or settings.stt_model
        if not isinstance(self.stt_model, str) or self.stt_model not in {"base", "small", "medium", "large", "turbo"}:
            raise ValueError("Invalid STT model")
        from .model_improvement.manager import speech_model

        self.stt_model = speech_model(self.stt_model)
        self.id = str(uuid.uuid4())
        self.send = send
        self.path = config.get_captures_dir() / f"{self.id}.wav"
        self.archive = wave.open(str(self.path), "wb")  # noqa: SIM115 — session owns this until close()
        self.archive.setnchannels(1)
        self.archive.setsampwidth(2)
        self.archive.setframerate(self.rate)
        self.pending = bytearray()
        # Checked as audio arrives, so a phrase without a voice skips Whisper,
        # which would otherwise invent one ("Thank you.").
        self.speech = SpeechDetector(self.rate, ignore_before=self.rate * start_cue_ms // 1000)
        # How it was said: measured as it is said, saved after the final
        # event (docs/plans/EXPRESSIVE_DICTATION.md). With "Write how it was
        # said" on, a sentence said with clearly more energy ends with "!" and
        # a drawn-out word is written so. Off, it is still measured, but the
        # text never changes.
        self.expressive = bool(getattr(settings, "expressive", False) and settings.auto_refine) and not self.is_command
        self.expression = prosody.Expression(self.rate, eager=self.expressive)
        # The speaker's norm and what their edits taught
        # (expression_learning.py), read when the run starts, and the voice
        # and words so far, worked out on a thread after each phrase.
        self.baseline = None
        self.taught = expression_learning.Learned()
        self.baseline_loading = None
        self.voice = None
        self.cuts = []
        self.offset = 0
        self.samples = 0
        self.sequence = 0
        self.last_cut = 0
        self.revision = 0
        self.covered = 0
        # Whisper's own text, the context it continues each phrase from.
        self.heard = ""
        # What was said, without the endings Whisper gave phrases only because
        # the audio paused. Saved as the raw transcript and cleaned.
        self.raw = ""
        # Whether the last phrase's audio was cut at a pause.
        self.paused = False
        self.refined = ""
        # Settled text is final. The open tail is the raw words since; it is
        # cleaned again as phrases arrive.
        self.settled = ""
        # Whitespace after settled text that ends a sentence; None when it was
        # settled mid-sentence and joins the tail at a seam.
        self.settled_gap = None
        self.settled_reviews = []
        # What the latest settle replaced, so a spoken correction can reopen it.
        self.last_settle = None
        self.tail_raw = ""
        self.tail_cleaned = ""
        self.tail_dirty = False
        self.stop_cleanup = None
        self.release_tail_words = 0
        self.llm_model = None
        self.refinement_error = None
        self.needs_final_refinement = False
        # Whether the cleanup ended the latest phrase with a period. Pauses
        # strip it (open_phrase); finish restores it only if it was there.
        self.cleanup_closed = True
        self.reviews = []
        self.overlap = False
        self.degraded_reason = None
        self.backlogged = False
        self.peak_backlog = 0.0
        self.started_at = time.monotonic()
        self.paging_at_start = memory.counters()
        self.finished_at = None
        # Model time spent after release, the part of the wait we control.
        self.after_release = {"recognize": 0.0, "refine": 0.0}
        self.abort = False
        self.finished = False
        self.persisted = False
        self.wake = asyncio.Event()
        # The default style until the app is known.
        self.style = styles_snapshot().default
        self.flags: RefinementFlags = flags_for(self.style, settings)

    async def emit(self, kind, **payload):
        self.revision += 1
        await self.send(
            dict(
                type=kind,
                session_id=self.id,
                revision=self.revision,
                covered_samples=self.covered,
                degraded_reason=self.degraded_reason,
                **payload,
            )
        )

    def append(self, frame: bytes):
        if len(frame) < 10 or len(frame) > MAX_FRAME_BYTES or (len(frame) - 8) % 2:
            raise ValueError("Invalid PCM frame size")
        sequence, offset = struct.unpack("<II", frame[:8])
        if sequence != self.sequence or offset != self.samples:
            raise ValueError("Audio frames must have contiguous sequence and sample offsets")
        pcm = frame[8:]
        count = len(pcm) // 2
        if self.samples + count > self.rate * MAX_SECONDS:
            raise ValueError("Streaming session exceeds one hour")
        self.archive.writeframesraw(pcm)
        samples = np.frombuffer(pcm, dtype="<i2")
        self.speech.feed(samples)
        self.expression.feed(samples)
        self.samples += count
        self.sequence += 1
        if self.backlogged:
            return
        if len(self.pending) + len(pcm) > self.rate * 2 * 60:
            # Recognition fell a minute behind. Keep archiving and transcribe
            # the whole recording at finish rather than failing the dictation.
            logger.warning(
                "Streaming recognition fell %.1fs behind; finishing with full-audio recognition",
                len(self.pending) / (self.rate * 2),
            )
            self.backlogged = True
            self.degraded_reason = "Recognition fell behind; final output uses full-audio recognition."
            self.pending.clear()
            self.cuts.clear()
            self.wake.set()
            return
        self.pending.extend(pcm)
        self.peak_backlog = max(self.peak_backlog, len(self.pending) / (self.rate * 2))
        # A phrase ends at 0.7 s without a voice. Loudness can't tell: a fan's
        # hum on one microphone is as loud as speech on another, and it never
        # let a pause register. Nothing is removed from the recognizer's audio.
        if self.samples - self.last_cut >= self.rate * 2 and self.speech.quiet() >= self.rate * 0.7:
            self.cuts.append(self.samples)
            self.last_cut = self.samples
        self.wake.set()

    def set_app(self, bundle_id: str | None, name: str | None, category: str | None = None) -> bool:
        """The dictation's target app, and so its writing style. ``category``
        is its App Store category, saved to suggest styles for new apps.

        Returns whether the style is now that app's. It can't change once a
        cleanup has started; the app is still saved with the capture.
        """
        self.app_bundle_id, self.app_name = bundle_id, name
        self.app_category = category or self.app_category
        # Dictionary words are spelled right for the rest of the take, even
        # after the style is fixed.
        self.dictionary = dictionaries.for_app(bundle_id, self.spoken.id if self.spoken else None)
        self.names = self.names | self.dictionary.names
        if self.cleanup_started or self.spoken:
            return False
        self.style = styles_snapshot().for_app(bundle_id)
        self.flags = flags_for(self.style, self.settings)
        return True

    async def take_spoken_style(self, text: str) -> str:
        """``text`` without an opening request for a style by name ("use
        formal mode"), which picks the style the dictation is written in.

        Tells the client in a ``style`` event, with the style it replaced
        (``from_name``, None when it was already that style), so the pill can
        show the change.
        """
        if self.is_command:
            return text
        style, rest = spoken_style(text, styles_snapshot())
        if style is None:
            if self.spoken is not None and not self.spoken_confirmed:
                # An early peek heard a request the phrase doesn't have.
                await self.use_style(None)
            return text
        await self.use_style(style)
        self.spoken_confirmed = True
        return rest

    async def use_style(self, style) -> None:
        """Write the take in ``style``, asked for by name; None goes back to
        the app's. The client hears of a change in a ``style`` event."""
        styles = styles_snapshot()
        previous = self.style
        target = style or styles.for_app(self.app_bundle_id)
        self.spoken = style
        self.style = target
        self.flags = flags_for(target, self.settings)
        self.dictionary = dictionaries.for_app(self.app_bundle_id, style.id if style else None)
        self.names = self.names | self.dictionary.names
        if previous.id == target.id and (style is None or self.style_announced == target.id):
            # Heard again (a peek, then the phrase, then full audio), or undone
            # back to the style it already was: nothing changed to show.
            return
        self.style_announced = target.id
        await self.emit(
            "style", style_id=target.id, name=target.name, from_name=previous.name if previous.id != target.id else None
        )

    def style_peek_due(self) -> bool:
        """Whether to look at the opening words for a style request or a voice
        edit now: the first phrase hasn't been recognized, and there is enough
        speech."""
        if (
            self.raw
            or self.spoken
            or self.edit_announced
            or self.is_command
            or self.finished
            or self.cuts
            or self.style_peeks >= 2
        ):
            return False
        start = self.speech.first_voice()
        if start is None:
            return False
        said = self.samples - start
        if said >= self.rate * STYLE_PEEK_BY_S:
            # A second look only when a pause brought the first one early.
            return self.style_peeks == 0 or said - self.style_peeked_at >= self.rate * 0.5
        return (
            self.style_peeks == 0
            and said >= self.rate * STYLE_PEEK_AFTER_S
            and self.speech.quiet() >= self.rate * STYLE_PEEK_PAUSE_S
        )

    async def peek_style(self) -> None:
        """Recognize the audio so far only to find a style request, so the
        style (and the pill's chip) changes as soon as it is said, or a voice
        edit, so its cue plays while the user still speaks. The text still
        comes from the phrase, which confirms or undoes a style change."""
        start = self.speech.first_voice() or 0
        self.style_peeks += 1
        self.style_peeked_at = self.samples - start
        if len(styles_snapshot().styles) < 2 and not self.edit_possible:
            self.style_peeks = 2
            return
        text = await self.recognize(bytes(self.pending), measure=False)
        if self.edit_possible and voice_edits.starts_edit(text) and not self.raw:
            await self.announce_edit()
            return
        style, _ = spoken_style(text, styles_snapshot())
        if style is not None and not self.raw and not self.finished:
            await self.use_style(style)

    def ignore_cue(self, start, end) -> None:
        """A sound Kass played during the take (the style cue), between
        two sample offsets: the microphone may have picked it up, and a chime
        reads as a voice. Only voice detection skips it; Whisper hears the
        audio as it was."""
        if not all(isinstance(value, int) and not isinstance(value, bool) for value in (start, end)):
            raise ValueError("Cue offsets must be sample counts")
        if not 0 <= start <= end or end - start > self.rate:
            raise ValueError("A cue lasts at most a second")
        self.speech.ignore(start, end)

    def set_context(self, before) -> None:
        """The field's text before the caret, known shortly after the take starts."""
        if not isinstance(before, str):
            raise ValueError("Context must be text")
        self.field_before = before[-FIELD_CONTEXT_CHARS:]
        self.continues = continues_sentence(self.field_before)

    def set_last_take(self, text, capture_id=None, own_chars=None) -> None:
        """The text before the caret, which a voice edit may change, known
        shortly after the take starts; ``own_chars`` of its end were written
        by the capture ``capture_id`` (all of it when not given, as older
        clients sent only Kass's own take)."""
        if not isinstance(text, str) or len(text) > LAST_TAKE_CHARS:
            raise ValueError(f"The last take must be text of at most {LAST_TAKE_CHARS} characters")
        if capture_id is not None and not isinstance(capture_id, str):
            raise ValueError("Invalid capture id")
        if own_chars is None:
            own_chars = len(text) if capture_id else 0
        if isinstance(own_chars, bool) or not isinstance(own_chars, int) or not 0 <= own_chars <= len(text):
            raise ValueError("Invalid own_chars")
        self.last_take, self.last_take_capture_id = text, capture_id
        self.last_take_own_chars = own_chars

    @property
    def edit_possible(self) -> bool:
        """Whether there is a take a voice edit could change."""
        return self.edits_on and bool(self.last_take)

    @property
    def vocabulary(self) -> tuple[str, ...]:
        """Whisper's prompt terms: the app's own command words while voice
        edits are on, then the dictionary's."""
        terms = self.dictionary.terms
        if not self.edits_on:
            return terms
        return (*voice_edits.COMMAND_TERMS, *(term for term in terms if term not in voice_edits.COMMAND_TERMS))

    async def take_edit(self, text: str) -> None:
        """Hold the take as a voice edit when it opens like one ("fix that,
        ..."): from here its words are never cleaned up, which could reorder
        or drop them. Finish decides what it changes, or cleans it up as a
        dictation after all."""
        if self.edits_on and voice_edits.starts_edit(text):
            self.editing = True
            await self.announce_edit()

    async def announce_edit(self) -> None:
        """Tell the client, once and while the user speaks, that the take is
        an edit of its last one, so it can play the edit cue."""
        if self.edit_announced or self.finished or not self.edit_possible:
            return
        self.edit_announced = True
        await self.emit("edit")

    async def finish_edit(self) -> None:
        """Plan the edit the take said. A take that opened like one but says
        no edit is a dictation, cleaned up whole."""
        planned = voice_edits.plan(self.raw, self.last_take if self.edit_possible else None)
        if planned is not None:
            self.edit = planned
            return
        self.editing = False
        self.raw = mark_commands(self.raw)
        if self.settings.auto_refine:
            await self.reconcile_refinement()

    def edit_result(self) -> dict | None:
        """For the final event: the last take's text before and after the
        edit, which the client applies, or why it was declined."""
        if isinstance(self.edit, voice_edits.Planned):
            return dict(before=self.edit.before, after=self.edit.after)
        if isinstance(self.edit, voice_edits.Declined):
            return dict(declined=self.edit.message)
        return None

    def learn_from_edit(self) -> None:
        """What the edit teaches (a report on the take it fixed, a spelled
        word). Blocking: run after the final event is sent."""
        if isinstance(self.edit, voice_edits.Planned):
            # Only a fix of what Kass wrote is reported on its capture.
            fixed_own = voice_edits.changes_end(self.edit, self.last_take_own_chars)
            capture_id = self.last_take_capture_id if fixed_own else None
            voice_edits.learn_from(self.edit, capture_id, self.app_bundle_id, self.id)

    @property
    def is_command(self) -> bool:
        return self.source == "command"

    def set_selection(self, text) -> None:
        """The text a command session rewrites, read in the target app at key-down.

        The model is prefilled with it at once, while the user is still
        speaking the instruction.
        """
        if not self.is_command:
            raise ValueError("Only command sessions take a selection")
        if not isinstance(text, str):
            raise ValueError("Selection must be text")
        if len(text) > MAX_SELECTION_CHARS:
            raise ValueError(f"Selection is too long for Command Mode ({MAX_SELECTION_CHARS:,} characters at most)")
        self.selection = text
        self.prefilling = asyncio.create_task(self.prefill_command())

    async def prefill_command(self) -> None:
        try:
            ensure_model_ready(self.settings.llm_model)
            await prefill(self.selection, self.settings.llm_model)
        except Exception:
            # Only a head start: the rewrite at finish reports real failures.
            logger.warning("Command prefill failed", exc_info=True)

    async def finish_command(self) -> None:
        """Rewrite the selection by what was said, or by the transform it names."""
        try:
            if self.selection is None:
                raise ValueError("Select text to rewrite first")
            instruction, transform = resolve_instruction(self.raw, settings_transforms(self.settings))
            ensure_model_ready(self.settings.llm_model)
            started = time.monotonic()
            try:
                self.refined, self.llm_model = await rewrite(self.selection, instruction, self.settings.llm_model)
            finally:
                self._spent("refine", started)
            self.command = (instruction, transform)
            self.refinement_error = None
            await self.emit("refined", text=self.refined)
        except Exception as error:
            logger.warning("Command failed: %s", error)
            self.refinement_error = str(error)

    def start_like_raw(self, text: str) -> str:
        """``text`` with the first word cased as in the raw transcript.

        Cleanup capitalizes the start of every text; where the dictation
        continues the field's sentence, the transcript decided its case.
        Elsewhere the style decides (``style_first_word``). Text that starts
        with a spoken line break starts a new line, not the field's sentence.
        """
        if text.lstrip(" \t")[:1] == "\n":
            return text
        if self.continues:
            return match_raw_start(text, self.raw)
        return style_first_word(text, self.flags, self.names)

    def learned(self, text: str) -> str:
        return apply_learned(text, self.flags.style)

    def corrected(self, text: str) -> str:
        """Learned corrections, then the dictionary, so the user's own entries win."""
        from .correction_learning import apply_learned_corrections

        return self.dictionary.apply(apply_learned_corrections(text, self.language))

    def holdback(self, minimum: int) -> int:
        """Words provisional text holds back: a correction or dictionary entry may still change them."""
        span = self.dictionary.span + 1 if self.dictionary.span else 0
        return max(CORRECTION_HOLDBACK if _has_corrections() else minimum, span)

    def _spent(self, stage: str, started: float) -> None:
        if self.finished_at is not None:
            self.after_release[stage] += time.monotonic() - max(started, self.finished_at)

    async def recognize(self, pcm, start=None, measure=True):
        """Whisper's text for ``pcm``, which starts at sample ``start``.

        ``measure`` keeps the phrase's word times for the expression
        measurements; a peek at the opening words doesn't.
        """
        # Earlier phrases give Whisper the sentence it is continuing, so a
        # phrase cut at a pause neither trails off with "..." nor restarts
        # with a capital letter.
        # The first phrase continues the field's sentence, when it does.
        earlier = self.heard or (self.field_before if self.continues else "")
        previous_text = earlier[-PHRASE_CONTEXT_CHARS:]
        samples = np.frombuffer(pcm, dtype="<i2")
        start = self.offset if start is None else start
        if not len(samples) or not self.speech.heard(start, start + len(samples)):
            return ""
        started = time.monotonic()
        alignments = [] if measure and not self.is_command else None
        try:
            text = (
                await get_whisper_model().transcribe_array(
                    samples,
                    self.rate,
                    self.language,
                    self.stt_model,
                    previous_text=previous_text,
                    check_speech=False,
                    vocabulary=self.vocabulary,
                    alignments=alignments,
                )
            ).strip()
        finally:
            self._spent("recognize", started)
        if alignments:
            self.expression.phrase(alignments[0], start, released=self.finished)
            if self.expressive:
                self.voice = asyncio.create_task(self.voice_so_far())
        return text

    async def voice_so_far(self) -> tuple[prosody.Voice, list[dict]] | None:
        """The voice up to now with every phrase's words, and the words drawn out, worked out off the event loop."""
        try:
            words = await self.expression.words()
            if not words or self.expression.failed:
                return None
            return await asyncio.to_thread(
                self._work_out_voice, self.expression.track.frames(), words, self.taught.rule
            )
        except Exception:
            logger.exception('Couldn\'t follow how the dictation was said; no "!" this time')
            return None

    @staticmethod
    def _work_out_voice(frames, words, rule) -> tuple[prosody.Voice, list[dict]]:
        voice = prosody.Voice(frames, words)
        return voice, prosody.measure_voice(voice, None, rule)["stretched"]

    def express(self, text: str, final: bool = False) -> str:
        """``text`` with "!" where it was said with clearly more energy, and words drawn out written so ("wayyy").

        Uses only what is already worked out: never waits, so after release
        it adds no time. A sentence not yet measured keeps its period.
        """
        voice = self.voice
        ready = (
            voice is not None
            and voice.done()
            and not voice.cancelled()
            and voice.exception() is None
            and voice.result() is not None
        )
        if self.expressive and text and ready:
            said, stretched = voice.result()
            exclaimed = prosody.exclaim(text, said, self.baseline, self.taught.cutoff)
            return prosody.stretch(exclaimed, said, stretched)
        if self.expressive and text and final:
            logger.info("How the dictation was said wasn't worked out in time; written as it is")
        return text

    def close_dictation(self, text, closed=None, learned=None):
        closed = self.cleanup_closed if closed is None else closed
        closed = close_phrase(text) if closed else text
        # Phrases were styled one at a time; habits like a dropped final period
        # only apply once the whole dictation is joined.
        if self.flags.punctuation_style == "learned":
            closed = (learned or self.learned)(closed)
        # Punctuation the speaker said wins over the style and the closing.
        if self.flags.smart_cleanup:
            closed = keep_said_punctuation(self.raw, closed)
        return period_after_closers(closed)

    def join(self, previous, phrase, raw_phrase, learned=None):
        if self.overlap:
            return join_overlap(previous, phrase)
        if self.flags.punctuation_style == "learned":
            # Join like Standard, then let the user's habits decide what each
            # sentence break becomes (comma, nothing, lowercase start...).
            return (learned or self.learned)(join_phrases(previous, phrase, raw_phrase, "standard"))
        return join_phrases(previous, phrase, raw_phrase, self.flags.punctuation_style)

    def compose(self, settled, text, raw, learned=None):
        """``text``, cleaned from ``raw``, after the settled text."""
        if not settled.strip():
            return self.start_like_raw(text)
        if not text:
            return settled
        if self.settled_gap is not None:
            # A tail that starts with a spoken break is the break the gap was.
            return settled + (text if text[0] == "\n" else self.settled_gap + text)
        return self.join(settled, text, raw, learned)

    # --- Provisional text ------------------------------------------------------

    def offers_provisional(self) -> bool:
        return (
            self.provisional
            and self.finished
            and self.settings.auto_refine
            and self.settings.allow_auto_paste
            and not self.overlap
            and not self.degraded_reason
            and not self.backlogged
            and not self.needs_final_refinement
            and not self.refinement_error
            and not self.editing
        )

    async def show(self, text: str, holdback: int) -> None:
        """Offer the client the part of ``text`` that should survive to the final."""
        if not self.offers_provisional():
            return
        stable = stable_prefix(text, holdback)
        if len(stable) > len(self.shown):
            self.shown = stable
            await self.emit("provisional", text=stable)

    async def show_cleaned_so_far(self) -> None:
        # Only settled text is final; the open sentence may still change.
        settled = self.corrected(self.settled) if self.settled else ""
        await self.show(settled, self.holdback(0))

    def _projection(self, prompt: str):
        """What finish would deliver if the cleanup of ``prompt`` ended now and passed the check.

        Mirrors refine_transcript's post-processing, then ``accept`` and
        ``close_dictation``. Habits are read once, not per token.
        """
        style = self.flags.style
        learned = (lambda text, h=habits(style): apply_style(text, h)) if is_ready(style) else (lambda text: text)
        prefix = self.settled

        def project(partial: str) -> str:
            # Keep a spoken line break the text starts with.
            refined = partial.lstrip(" \t").rstrip()
            if self.flags.punctuation_style == "learned":
                refined = learned(refined)
            text = self.corrected(self.compose(prefix, refined, prompt, learned))
            return self.express(self.close_dictation(text, refined.rstrip().endswith("."), learned))

        return project

    @contextlib.asynccontextmanager
    async def streaming_cleanup(self, prompt: str):
        """Offer provisional text while the cleanup of the final phrase generates."""
        if not self.offers_provisional():
            yield
            return
        loop = asyncio.get_running_loop()
        latest = [None]
        changed = asyncio.Event()
        done = [False]

        def update(partial):
            if not done[0]:
                latest[0] = partial
                changed.set()

        def listener(partial):  # MLX worker thread
            with contextlib.suppress(RuntimeError):
                loop.call_soon_threadsafe(update, partial)

        async def relay():
            # Built on the first token, while the model generates, so reading
            # the habits (~2 ms) never delays the cleanup itself.
            project = None
            while not done[0]:
                await changed.wait()
                changed.clear()
                if done[0] or latest[0] is None:
                    return
                project = project or self._projection(prompt)
                # Learned corrections and dictionary entries may span a few words; hold them back too.
                await self.show(project(latest[0]), self.holdback(1))

        relay_task = asyncio.create_task(relay())
        token = generation_listener.set(listener)
        try:
            yield
        finally:
            generation_listener.reset(token)
            done[0] = True
            changed.set()
            # Only waits for a send already in progress; never cancels one.
            await relay_task

    async def accept(self, text, paused=False):
        """Add a recognized phrase; ``paused`` when its audio was cut at a pause."""
        if text and not self.raw:
            # Dropped from the context too: the phrase after a lone "use
            # formal mode." starts the dictation.
            text = await self.take_spoken_style(text)
            # Read before any cleanup (docs/plans/VOICE_EDITS.md).
            await self.take_edit(text)
        earlier = self.heard
        self.heard = self.join(self.heard, text, text)
        if self.editing:
            # An edit's words are its instruction, read whole at finish.
            if text:
                self.raw = join_overlap(self.raw, text) if self.overlap else f"{self.raw} {text}".strip()
                self.paused = paused
            await self.emit("transcript", accepted_text=self.raw, provisional_text="", text=self.raw, final=False)
            return
        if text:
            if not self.overlap:
                # The pause, not the speaker, ended the phrase before this one
                # and capitalized this one.
                phrase = text
                if earlier:
                    before = self.tail_raw if self.paused else ""
                    tail, phrase = continue_after_seam(before, text, earlier, self.names)
                    if tail != before:
                        self.raw, self.tail_raw = self.raw[: len(tail) - len(before)], tail
                elif self.continues:
                    # Whisper capitalizes the start of the audio as if it
                    # began a sentence. The names loaded while it listened.
                    if self.names_loading:
                        await asyncio.shield(self.names_loading)
                    phrase = continue_phrase(text, self.field_before, self.names)
                phrase = strip_pause_mark(phrase) if paused else phrase
                self.raw = f"{self.raw} {phrase}".strip()
                self.tail_raw = f"{self.tail_raw} {phrase}".strip()
            else:
                self.raw = join_overlap(self.raw, text)
                self.tail_raw = join_overlap(self.tail_raw, text)
            # Across phrases too: "paste from" may end one and "clipboard" start the next.
            self.raw, self.tail_raw = mark_commands(self.raw), mark_commands(self.tail_raw)
            self.tail_dirty = True
            self.paused = paused
        await self.emit("transcript", accepted_text=self.raw, provisional_text="", text=self.raw, final=False)
        # A command's instruction is never cleaned up: it isn't the output.
        if not self.settings.auto_refine or self.is_command:
            return
        self.cleanup_started = self.cleanup_started or bool(self.raw)
        try:
            # Explicit corrections operate on the entire raw session so a later
            # "scratch that" can revise a previously accepted phrase.
            _, correction = prepare_refinement(self.raw, self.flags)
            if correction is not None:
                self.refined = self.start_like_raw(correction)
                self.cleanup_closed = True
                self.llm_model = self.settings.llm_model
                # Resolved as a whole; later phrases continue after it.
                self.settled, self.settled_gap, self.last_settle = correction, None, None
                self.tail_raw, self.tail_cleaned, self.tail_dirty = "", "", False
            elif text:
                if self.flags.self_correction and self.last_settle and _CORRECTION_CUE.search(text):
                    self.reopen()
                await self.clean_tail()
            self.refined = self.corrected(self.refined)
            await self.emit("refined", text=self.refined)
        except Exception as error:
            logger.exception("Streaming refinement failed")
            self.refinement_error = str(error)

    def reopen(self):
        """Put the latest settled text back in the open tail, for a correction of it."""
        settled, gap, raw, reviews = self.last_settle
        self.settled, self.settled_gap, self.settled_reviews = settled, gap, reviews
        self.tail_raw = f"{raw} {self.tail_raw}".strip()
        self.tail_cleaned = ""
        self.last_settle = None

    def settle(self, text, raw, gap):
        self.last_settle = (self.settled, self.settled_gap, raw, self.settled_reviews)
        # Settled text is shown as it is after release, so it gets its "!" now.
        self.settled = self.express(self.compose(self.settled, text, raw))
        self.settled_gap = gap
        self.settled_reviews = list(self.reviews)

    def speech_follows(self) -> bool:
        """Whether audio not yet recognized has a voice in it."""
        return self.samples > self.offset and self.speech.heard(self.offset, self.samples)

    async def clean_tail(self):
        """Clean the open tail and settle every finished sentence of the result."""
        prompt = self.tail_raw
        # Made while speaking, this cleanup may be replaced by the one after
        # release; finish() stops it then instead of waiting for it.
        stop = None if self.finished else threading.Event()
        self.stop_cleanup = stop
        if self.finished:
            self.release_tail_words = len(prompt.split())
        started = time.monotonic()
        # The last cleanup of this tail: most of the new one repeats it.
        tokens = generation_stop.set(stop), generation_hint.set(self.tail_cleaned)
        try:
            async with self.streaming_cleanup(prompt):
                refined, self.llm_model = await refine_transcript(
                    prompt, self.flags, model_size=self.settings.llm_model
                )
        finally:
            generation_stop.reset(tokens[0])
            generation_hint.reset(tokens[1])
            self.stop_cleanup = None
            self._spent("refine", started)
        if stop is not None and stop.is_set():
            return
        refined, verdict = guard_phrase_refinement(prompt, refined, self.flags)
        self.tail_dirty = False
        self.reviews = [*self.settled_reviews, *([verdict] if verdict.outcome != "ok" else [])]
        # A rejected cleanup falls back to the transcript, which is closed the
        # standard way.
        self.cleanup_closed = verdict.outcome == "reject" or refined.rstrip().endswith(".")
        if verdict.outcome == "reject" and self.flags.self_correction and _CORRECTION_CUE.search(prompt):
            self.needs_final_refinement = True
        finished = settle(prompt, refined) if verdict.outcome != "reject" and not self.overlap else None
        if finished:
            self.settle(finished.committed, finished.committed_raw, finished.gap)
            self.tail_raw, refined = finished.open_raw, finished.open_cleaned
        elif len(prompt.split()) > MAX_OPEN_WORDS and not self.finished:
            # No sentence ended in a long stretch: settle it where it is, so
            # the cleanup after release stays about one phrase long.
            self.settle(open_phrase(refined, prompt), prompt, None)
            self.tail_raw, refined = "", ""
        self.tail_cleaned = refined
        # The open sentence stays open while more may follow; finish closes it.
        shown = open_phrase(refined, self.tail_raw) if refined else ""
        self.refined = self.compose(self.settled, shown, self.tail_raw)

    async def finish_cleanup(self):
        """Deliver settled text and the cleaned tail, closed like a finished dictation."""
        try:
            if self.tail_dirty and self.tail_raw:
                # A cleanup was stopped at release, and nothing was said after it.
                await self.clean_tail()
            if self.needs_final_refinement:
                await self.reconcile_refinement()
                return
            text = self.corrected(self.compose(self.settled, self.tail_cleaned, self.tail_raw))
        except Exception as error:
            logger.exception("Streaming refinement failed")
            self.refinement_error = str(error)
            return
        if (closed := self.express(self.close_dictation(text), final=True)) != self.refined:
            self.refined = closed
            await self.emit("refined", text=self.refined)

    async def run(self):
        # Read while the first phrase is still being spoken; recognition never
        # waits for it.
        self.names_loading = asyncio.create_task(self.load_names())
        if self.expressive:
            self.baseline_loading = asyncio.create_task(self.load_baseline())
        try:
            await self._run()
        finally:
            self.names_loading.cancel()
            if self.baseline_loading is not None:
                self.baseline_loading.cancel()

    async def load_baseline(self):
        try:
            self.baseline, self.taught = await asyncio.to_thread(
                lambda: (prosody.load_baseline(), expression_learning.load())
            )
        except Exception:
            logger.exception('Could not read how the speaker usually sounds; no "!" this time')

    async def load_names(self):
        try:
            names = await asyncio.to_thread(lambda: (load_word_data(), known_names())[1])
        except Exception:
            logger.exception("Could not read the user's names; names in the word lists still count")
            return
        self.names = self.names | names | self.dictionary.names

    async def _run(self):
        while True:
            if self.abort:
                return
            if self.backlogged:
                if self.finished:
                    await self.reconcile_full_audio()
                    return
                self.wake.clear()
                await self.wake.wait()
                continue
            while self.cuts and self.cuts[0] <= self.offset:
                self.cuts.pop(0)
            if self.style_peek_due():
                await self.peek_style()
                continue
            available = len(self.pending) // 2
            cut = self.cuts[0] - self.offset if self.cuts else None
            forced = available >= self.rate * 20 and (cut is None or cut > self.rate * 20)
            if self.finished and (self.degraded_reason or forced):
                self.degraded_reason = (
                    "Continuous speech exceeded the safe phrase window; final output uses full-audio recognition."
                )
                await self.reconcile_full_audio()
                return
            size = self.rate * 20 if forced else cut
            paused = not forced and cut is not None
            offer = None
            if size is None and self.finished:
                size = available
                if size:
                    # Released: what was cleaned while speaking is offered
                    # while the last phrase is recognized, not before it.
                    offer = asyncio.create_task(self.show_cleaned_so_far())
            if size:
                pcm = bytes(self.pending[: size * 2])
                text = await self.recognize(pcm)
                if offer is not None:
                    await offer
                if self.abort:
                    return
                # Keep one second of context only for forced (unpaused) cuts.
                if forced:
                    self.degraded_reason = (
                        "Continuous speech exceeded the safe phrase window; final output uses full-audio recognition."
                    )
                advance = size - self.rate if forced else size
                del self.pending[: advance * 2]
                self.covered = max(self.covered, self.offset + size)
                self.offset += advance
                await self.accept(text, paused)
                self.overlap = forced
                continue
            if self.finished:
                if self.degraded_reason:
                    await self.reconcile_full_audio()
                elif self.is_command:
                    await self.finish_command()
                elif self.editing:
                    await self.finish_edit()
                elif self.needs_final_refinement:
                    await self.reconcile_refinement()
                elif self.settings.auto_refine:
                    await self.finish_cleanup()
                return
            self.wake.clear()
            await self.wake.wait()

    async def reconcile_full_audio(self):
        """Use the established batch path when a forced seam cannot be proven."""
        self.archive.close()
        if self.speech.heard(0, self.samples):
            before = self.field_before if self.continues else None
            self.raw = (
                await get_whisper_model().transcribe(
                    str(self.path),
                    self.language,
                    self.stt_model,
                    previous_text=before,
                    check_speech=False,
                    vocabulary=self.vocabulary,
                )
            ).strip()
            self.raw = await self.take_spoken_style(self.raw)
            # Heard again, whole: it may open as an edit now, or no longer.
            self.editing = False
            await self.take_edit(self.raw)
            if not self.editing:
                if self.continues:
                    self.raw = continue_phrase(self.raw, self.field_before, self.names)
                self.raw = mark_commands(self.raw)
        else:
            self.raw = ""
        if self.abort:
            return
        self.covered = self.samples
        await self.emit("transcript", accepted_text=self.raw, provisional_text="", text=self.raw, final=False)
        if self.editing:
            await self.finish_edit()
            return
        await self.reconcile_refinement()

    async def reconcile_refinement(self):
        """Resolve ambiguous spoken corrections with complete session context."""
        if self.is_command:
            await self.finish_command()
        elif self.settings.auto_refine:
            self.cleanup_started = True
            try:
                started = time.monotonic()
                refined, self.llm_model = await refine_transcript(
                    self.raw, self.flags, model_size=self.settings.llm_model
                )
                self._spent("refine", started)
                # The whole dictation was cleaned up again, so earlier phrase
                # verdicts no longer describe the result.
                self.refined, verdict = guard_phrase_refinement(self.raw, refined, self.flags)
                self.refined = self.start_like_raw(self.refined)
                self.reviews = [verdict]
                self.refined = self.express(self.corrected(self.refined), final=True)
                self.refinement_error = None
                await self.emit("refined", text=self.refined)
            except Exception as error:
                self.refinement_error = str(error)

    def finish(self):
        self.finished = True
        self.finished_at = time.monotonic()
        if self.stop_cleanup is not None and self.speech_follows():
            # The cleanup after release covers this tail and the rest.
            self.stop_cleanup.set()
        self.wake.set()

    def timing_summary(self) -> str:
        """One log line per dictation: enough to see where time went, no text."""
        now = time.monotonic()
        release = f"{now - self.finished_at:.2f}s" if self.finished_at else "not released"
        return (
            f"session={self.id} audio={self.samples / self.rate:.2f}s rate={self.rate} "
            f"wall={now - self.started_at:.2f}s release_to_final={release} "
            f"after_release_recognize={self.after_release['recognize']:.2f}s "
            f"after_release_refine={self.after_release['refine']:.2f}s "
            f"after_release_tail_words={self.release_tail_words} "
            f"peak_backlog={self.peak_backlog:.2f}s degraded={self.degraded_reason or 'no'} "
            f"refinement_error={'yes' if self.refinement_error else 'no'} "
            f"{memory.summary(self.paging_at_start)}"
        )

    def stored_audio(self) -> str:
        """The recording's storage path, or "" once it is deleted because the user keeps none."""
        from .audio_retention import discard, discards_now

        if discards_now(self.settings):
            discard(self.path)
            return ""
        return config.to_storage_path(self.path)

    def persist(self, db):
        self.archive.close()
        if self.is_command:
            return self.persist_command(db)
        if self.edit is not None:
            return self.persist_edit(db)
        stored = self.stored_audio()
        row = Capture(
            id=self.id,
            audio_path=stored,
            audio_deleted=not stored,
            source=self.source,
            language=self.language,
            duration_ms=round(self.samples / self.rate * 1000),
            transcript_raw=self.raw,
            stt_model=self.stt_model,
            transcript_refined=self.refined if self.settings.auto_refine and not self.refinement_error else None,
            llm_model=self.llm_model,
            refinement_flags=json.dumps(self.flags.to_dict()) if self.settings.auto_refine else None,
            refinement_review=json.dumps(review)
            if self.settings.auto_refine and (review := summarize_reviews(self.reviews))
            else None,
            app_bundle_id=self.app_bundle_id,
            app_name=self.app_name,
            app_category=self.app_category,
            style_id=self.style.id if self.settings.auto_refine else None,
            # Corrections teach the style asked for, not the app's.
            teaches_style_id=self.spoken.id
            if self.spoken and self.spoken.id != styles_snapshot().for_app(self.app_bundle_id).id
            else None,
        )
        db.add(row)
        db.commit()
        self.persisted = True
        db.refresh(row)
        return models.CaptureCreateResponse(
            **_to_response(row).model_dump(),
            auto_refine=self.settings.auto_refine,
            allow_auto_paste=self.settings.allow_auto_paste,
        )

    def persist_command(self, db):
        stored = self.stored_audio()
        row = Capture(
            id=self.id,
            audio_path=stored,
            audio_deleted=not stored,
            source=self.source,
            language=self.language,
            duration_ms=round(self.samples / self.rate * 1000),
            transcript_raw=self.raw,
            stt_model=self.stt_model,
            app_bundle_id=self.app_bundle_id,
            app_name=self.app_name,
            app_category=self.app_category,
            command_selection=self.selection,
        )
        if self.command is not None and not self.refinement_error:
            instruction, transform = self.command
            record_command(
                row,
                selection=self.selection,
                instruction=instruction,
                transform=transform,
                text=self.refined,
                model=self.llm_model,
            )
        db.add(row)
        db.commit()
        self.persisted = True
        db.refresh(row)
        # A command replaces its selection whatever the paste setting says.
        return models.CaptureCreateResponse(**_to_response(row).model_dump(), auto_refine=True, allow_auto_paste=True)

    def persist_edit(self, db):
        """A voice edit is saved like a command on the take it changed: the
        take before, what was said, and the take after (docs/plans/VOICE_EDITS.md)."""
        stored = self.stored_audio()
        row = Capture(
            id=self.id,
            audio_path=stored,
            audio_deleted=not stored,
            source="command",
            language=self.language,
            duration_ms=round(self.samples / self.rate * 1000),
            transcript_raw=self.raw,
            stt_model=self.stt_model,
            app_bundle_id=self.app_bundle_id,
            app_name=self.app_name,
            app_category=self.app_category,
            command_selection=self.last_take,
            command_transform=voice_edits.TRANSFORM_NAME,
        )
        if isinstance(self.edit, voice_edits.Planned):
            record_command(
                row,
                selection=self.edit.before,
                instruction=self.edit.instruction,
                transform={"name": voice_edits.TRANSFORM_NAME},
                text=self.edit.after,
                model=None,
            )
        else:
            row.command_instruction = self.edit.message
        db.add(row)
        db.commit()
        self.persisted = True
        db.refresh(row)
        # Like a command, an edit changes the field whatever the paste setting says.
        return models.CaptureCreateResponse(**_to_response(row).model_dump(), auto_refine=True, allow_auto_paste=True)

    def close(self):
        self.archive.close()
        if not self.persisted:
            with contextlib.suppress(OSError):
                self.path.unlink()
