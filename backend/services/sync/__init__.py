"""
This Mac's server standing in for a shared Kass (beta ``network``).

A Mac set to use another Mac's Kass still runs its own server, started with
``KASS_SHARED_SERVER`` naming the shared one. That server stands in: the
app dictates on it whenever the shared Kass can't be reached.

While the shared Kass is up, the stand-in copies what dictation reads from
it (``mirror``): settings, styles, the dictionary, what was learned and the
models in use. What the user does on the stand-in is recorded (``changes``)
and sent to the shared Kass once it's back (``push``), where it's applied
as if it had been done there (``importer``), the later edit winning.

The stand-in loads no models until the app falls back to it (``wake``), and
never trains or relearns: the shared Kass learns from what it's sent.
"""

import os

SHARED_SERVER_ENV = "KASS_SHARED_SERVER"


def shared_server() -> str | None:
    """The shared Kass this server stands in for, or None when it is the one in use."""
    url = os.environ.get(SHARED_SERVER_ENV, "").strip().rstrip("/")
    return url or None


def standing_in() -> bool:
    return shared_server() is not None
