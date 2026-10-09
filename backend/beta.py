"""
Beta features: they ship in every release but only run for beta users
(Settings › General › Beta updates).

The app writes `update-channel` into the data dir while the beta channel is
on (tauri src-tauri/src/updater.rs). It's read on every check, so switching
applies right away, without restarting the server.

Add a name to BETA_FEATURES and gate the feature with `enabled(name)`. To
make it public, remove the name: every check still using it then raises,
which shows what to clean up. The app keeps its own list in
app/src/lib/betaFeatures.ts.
"""

from . import config

BETA_FEATURES: frozenset[str] = frozenset(set())

CHANNEL_FILE = "update-channel"


def enabled(feature: str) -> bool:
    """Whether a beta feature runs: only for beta users."""
    if feature not in BETA_FEATURES:
        raise ValueError(f"{feature!r} isn't in BETA_FEATURES")
    try:
        return (config.get_data_dir() / CHANNEL_FILE).read_text().strip() == "beta"
    except OSError:
        return False
