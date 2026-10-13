"""ORM model definitions for the kass SQLite database."""

import uuid
from datetime import datetime

from sqlalchemy import JSON, Boolean, Column, DateTime, Float, ForeignKey, Integer, String, Text, UniqueConstraint
from sqlalchemy.ext.declarative import declarative_base

from ..services.commands import default_transforms
from ..utils.capture_chords import (
    default_command_chord,
    default_push_to_talk_chord,
    default_toggle_to_talk_chord,
)

Base = declarative_base()


class CaptureSettings(Base):
    """Singleton row holding user defaults for the capture/refine flow.

    Kept server-side so every window, CLI client, and API consumer reads the
    same preferences. The ``id`` column is always 1.
    """

    __tablename__ = "capture_settings"

    id = Column(Integer, primary_key=True, default=1)
    stt_model = Column(String, nullable=False, default="turbo")
    language = Column(String, nullable=False, default="auto")
    auto_refine = Column(Boolean, nullable=False, default=True)
    llm_model = Column(String, nullable=False, default="0.6B")
    smart_cleanup = Column(Boolean, nullable=False, default=True)
    self_correction = Column(Boolean, nullable=False, default=True)
    preserve_technical = Column(Boolean, nullable=False, default=True)
    punctuation_style = Column(String, nullable=False, default="standard")
    allow_auto_paste = Column(Boolean, nullable=False, default=True)
    # Type cleaned text into the app while cleanup is still writing it
    # (docs/plans/STREAMING_INSERTION.md). Off until checked in more apps.
    live_text = Column(Boolean, nullable=False, default=False)
    # Fix the last dictation by voice ("fix that, Morgan not Megan";
    # docs/plans/VOICE_EDITS.md).
    voice_edits = Column(Boolean, nullable=False, default=True)
    # Write "!" and drawn-out words ("wayyy") from how something was said
    # (docs/plans/EXPRESSIVE_DICTATION.md). Off still measures and learns;
    # it only keeps them out of the text.
    expressive = Column(Boolean, nullable=False, default=True)
    # Chimes when dictation starts, stops or fails, played by the desktop app.
    sound_cues = Column(Boolean, nullable=False, default=True)
    sound_cue_volume = Column(Float, nullable=False, default=0.5)
    # Configured audio input deviceId (None means system default microphone)
    input_device_id = Column(String, nullable=True, default=None)
    # Default OFF — opting in is what triggers the macOS Input Monitoring TCC
    # prompt. We deliberately don't spawn the global keyboard tap until the
    # user flips this on so a fresh-install user doesn't see a scary
    # "Kass would like to receive keystrokes from any application" dialog
    # before they've even opened the Captures tab.
    hotkey_enabled = Column(Boolean, nullable=False, default=False)
    # Lists of keytap key names (e.g. "MetaRight", "ControlRight"). Right-hand
    # modifiers by default so they don't collide with left-hand shortcuts.
    chord_push_to_talk_keys = Column(JSON, nullable=False, default=default_push_to_talk_chord)
    chord_toggle_to_talk_keys = Column(JSON, nullable=False, default=default_toggle_to_talk_chord)
    # Command Mode (docs/plans/COMMAND_MODE.md): its chord (empty = off) and
    # the saved transforms ({id, name, instruction}). It rewrites on the
    # cleanup model, llm_model.
    chord_command_keys = Column(JSON, nullable=False, default=default_command_chord)
    command_transforms = Column(JSON, nullable=False, default=default_transforms)
    # Days of capture history to keep (docs/plans/HISTORY_RETENTION.md); 0 keeps it forever.
    history_retention_days = Column(Integer, nullable=False, default=30)
    # Nothing is deleted until the user has chosen a window, or there was
    # nothing old to delete when first asked.
    history_retention_confirmed = Column(Boolean, nullable=False, default=False)
    # First-run onboarding (docs/plans/ONBOARDING.md) was finished or closed.
    onboarding_completed = Column(Boolean, nullable=False, default=False)
    # Delete each recording once its transcript is saved (services/audio_retention.py).
    discard_audio = Column(Boolean, nullable=False, default=False)
    # Usage reports (services/usage_report.py): whether to send them, the
    # random id they're sent under, and the last local day sent (ISO date).
    share_usage = Column(Boolean, nullable=False, default=True)
    usage_device_id = Column(String, nullable=True)
    usage_sent_through = Column(String, nullable=True)
    # Start from the adapters Kass ships (services/shared_adapters.py).
    shared_adapters = Column(Boolean, nullable=False, default=True)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow)


class Capture(Base):
    """A single voice input capture (dictation, recording, or uploaded file).

    Stores the original audio alongside the raw transcript and, optionally, a
    refined version produced by the LLM. Refinement flags are serialized as
    JSON so we can reproduce the prompt that generated the refined text.
    """

    __tablename__ = "captures"

    id = Column(String, primary_key=True, default=lambda: str(uuid.uuid4()))
    audio_path = Column(String, nullable=False)
    source = Column(String, nullable=False, default="file")  # dictation | recording | file | command
    language = Column(String, nullable=True)
    duration_ms = Column(Integer, nullable=True)
    transcript_raw = Column(Text, nullable=False, default="")
    transcript_refined = Column(Text, nullable=True)
    stt_model = Column(String, nullable=True)
    llm_model = Column(String, nullable=True)
    refinement_flags = Column(Text, nullable=True)  # JSON blob
    # JSON: what the content check found when cleanup may have added or lost content.
    refinement_review = Column(Text, nullable=True)
    # The app that had focus when dictation started (None for uploads and
    # for dictation inside Kass itself).
    app_bundle_id = Column(String, nullable=True)
    app_name = Column(String, nullable=True)
    # Command captures: the text that was selected, the instruction that ran
    # (a transform's, when one was named) and that transform's name.
    # transcript_raw is what was said, transcript_refined the rewrite.
    command_selection = Column(Text, nullable=True)
    command_instruction = Column(Text, nullable=True)
    command_transform = Column(String, nullable=True)
    # The writing style the capture was cleaned up with (docs/plans/PER_APP_STYLE.md).
    style_id = Column(String, nullable=True)
    # The app's App Store category (LSApplicationCategoryType), which suggests
    # a style for a new app.
    app_category = Column(String, nullable=True)
    # The style this capture's corrections teach, when the user left them
    # behind as its app moved to another style. None follows the app.
    teaches_style_id = Column(String, nullable=True)
    # The Mac that dictated it, when Kass is shared over the network (utils/macs.py).
    mac_id = Column(String, nullable=True)
    mac_name = Column(String, nullable=True)
    # The recording was deleted once its transcript was saved (services/audio_retention.py).
    # A command run from ⌘K has no recording and is not marked.
    audio_deleted = Column(Boolean, nullable=False, default=False)
    # JSON: how it was said, per sentence (services/prosody.py), saved after
    # the text is delivered. None until then and for unmeasured captures.
    prosody = Column(Text, nullable=True)
    created_at = Column(DateTime, default=datetime.utcnow)


class WritingStyle(Base):
    """A named writing style that apps are assigned to (docs/plans/PER_APP_STYLE.md).

    Its calibration, habits, examples and rules live in the writing style and
    correction notes files, keyed by ``id``.
    """

    __tablename__ = "writing_styles"

    id = Column(String, primary_key=True, default=lambda: uuid.uuid4().hex)
    name = Column(String, nullable=False)
    position = Column(Integer, nullable=False, default=0)
    # The style of every app the user hasn't assigned. Exactly one row.
    is_default = Column(Boolean, nullable=False, default=False)
    punctuation_style = Column(String, nullable=False, default="standard")
    # No longer settings (both always on); kept so existing rows still load.
    capitalize_first = Column(Boolean, nullable=False, default=True)
    smart_cleanup = Column(Boolean, nullable=False, default=True)
    preserve_technical = Column(Boolean, nullable=False, default=True)
    # How the user says they write in this style's apps (docs/plans/TEACH_BY_REPLYING.md).
    description = Column(Text, nullable=True)
    created_at = Column(DateTime, default=datetime.utcnow)


class AppStyle(Base):
    """The style the user chose for an app. An app without a row uses the default and is "new"."""

    __tablename__ = "app_styles"

    bundle_id = Column(String, primary_key=True)
    app_name = Column(String, nullable=True)
    style_id = Column(String, ForeignKey("writing_styles.id"), nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow)


class CaptureFeedback(Base):
    """Correction paired with the model output observed by the user.

    Never edited; the user can withdraw it (capture_feedback.withdraw_feedback).
    """

    __tablename__ = "capture_feedback"

    id = Column(String, primary_key=True, default=lambda: str(uuid.uuid4()))
    capture_id = Column(String, ForeignKey("captures.id"), nullable=False, index=True)
    target = Column(String, nullable=False)
    expected_text = Column(Text, nullable=False)
    notes = Column(Text, nullable=False, default="")
    snapshot = Column(Text, nullable=False)
    # How the report was made: "manual" in Captures, "voice_fix" when the user
    # fixed Herga's text by voice, "redictation" when they dictated it again.
    # A redictation is weaker evidence: it teaches learning only, never the
    # writing style (examples, habits, names), and a rule needs an explicit report.
    source = Column(String, nullable=False, default="manual", server_default="manual")
    # The voice edit capture that filed it; deleting that capture withdraws it.
    filed_by = Column(String, nullable=True)
    # Copied from the capture when history retention deletes it, so the
    # correction keeps teaching the same style (docs/plans/HISTORY_RETENTION.md).
    # Null while the capture exists: read the capture's own columns then.
    app_bundle_id = Column(String, nullable=True)
    teaches_style_id = Column(String, nullable=True)
    created_at = Column(DateTime, default=datetime.utcnow)

    EXPLICIT_SOURCES = ("manual", "voice_fix")

    @classmethod
    def teaches_style(cls):
        """Filter for reports that teach the writing style: explicit ones."""
        return cls.source.in_(cls.EXPLICIT_SOURCES)


class RetiredCapture(Base):
    """What usage stats count from a capture history retention deleted.

    Numbers only, no text or audio, one row per capture so medians stay exact
    and folding the same capture twice cannot count it twice.
    """

    __tablename__ = "retired_captures"

    capture_id = Column(String, primary_key=True)
    created_at = Column(DateTime, nullable=False, index=True)
    source = Column(String, nullable=False)
    app_bundle_id = Column(String, nullable=True)
    # Words delivered (refined, else raw) and words said, as usage stats count them.
    words = Column(Integer, nullable=False, default=0)
    raw_words = Column(Integer, nullable=False, default=0)
    duration_ms = Column(Integer, nullable=True)
    fixed = Column(Boolean, nullable=False, default=False)


class TakeReport(Base):
    """How one take ended, from the app, for usage reports' speed and failure counts.

    Numbers only. Rows go once their day has been reported, or is too old to report.
    """

    __tablename__ = "take_reports"

    id = Column(Integer, primary_key=True, autoincrement=True)
    created_at = Column(DateTime, nullable=False, index=True, default=datetime.utcnow)
    mode = Column(String, nullable=False)  # dictation | command
    outcome = Column(String, nullable=False)  # delivered | failed
    # Key release to the text being in place, for delivered takes.
    latency_ms = Column(Integer, nullable=True)


class KnownName(Base):
    """A name from a capture history retention deleted, for known_names.py."""

    __tablename__ = "known_names"

    name = Column(String, primary_key=True)
    # The newest deleted capture that wrote it.
    last_seen_at = Column(DateTime, nullable=False)


class DictionaryEntry(Base):
    """A word or phrase dictation should get right (docs/plans/DICTIONARIES.md).

    A term has only ``written``; a replacement writes ``written`` where
    ``spoken`` was said. A phrase is a replacement whose ``written`` is text
    to insert as it is ("my email" → an address), not a word to spell.
    """

    __tablename__ = "dictionary_entries"
    # One entry per scope for the same word said, whatever its case.
    __table_args__ = (UniqueConstraint("scope", "scope_id", "key"),)

    id = Column(String, primary_key=True, default=lambda: str(uuid.uuid4()))
    scope = Column(String, nullable=False)  # global | style | app
    # A style id or bundle id; "" for global, since SQLite never finds two nulls equal.
    scope_id = Column(String, nullable=False, default="")
    app_name = Column(String, nullable=True)
    written = Column(String, nullable=False)
    spoken = Column(String, nullable=True)
    key = Column(String, nullable=False)
    # Rows added together as one entry that applies in several places share
    # this; null means the row is an entry of its own (its id).
    group_id = Column(String, nullable=True, index=True)
    # Off: the word is only prompted and recased where spelled exactly, never
    # swapped in for words that sound like it ("Meghan" leaves "Megan" alone).
    match_sound = Column(Boolean, nullable=False, default=True, server_default="1")
    # Who added it: null for the user, "spoken_fix" for a word spelled aloud to fix it.
    source = Column(String, nullable=True)
    # The voice edit capture that added a spelled word; deleting that capture
    # removes the entry, unless the user has edited it since.
    added_by = Column(String, nullable=True)
    # On: ``written`` is inserted exactly, line breaks and all, and is never
    # a term Whisper is prompted with or that respells other words.
    phrase = Column(Boolean, nullable=False, default=False, server_default="0")
    created_at = Column(DateTime, default=datetime.utcnow)
