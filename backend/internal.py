"""
Internal features: they ship in every release but only run on the developer's
own builds, before they're ready for beta users.

`cargo xtask install` writes an empty `internal` file into the data dir, so
every build installed from a checkout has them and the release DMG doesn't.
Delete the file to turn them off. It's read on every check, so the change
applies without restarting the server.

Add a name to INTERNAL_FEATURES and gate the feature with `enabled(name)`.
To move it to beta, remove the name: every check still using it then raises,
which shows what to change.
"""

from . import config

INTERNAL_FEATURES: frozenset[str] = frozenset()

MARKER_FILE = "internal"


def is_internal() -> bool:
    return (config.get_data_dir() / MARKER_FILE).is_file()


def enabled(feature: str) -> bool:
    """Whether an internal feature runs: only on internal builds."""
    if feature not in INTERNAL_FEATURES:
        raise ValueError(f"{feature!r} isn't in INTERNAL_FEATURES")
    return is_internal()
