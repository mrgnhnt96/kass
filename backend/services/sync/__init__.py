"""
This Mac's server standing in for a shared Kass (beta ``network``).

A Mac set to use another Mac's Kass still runs its own server, started with
``KASS_SHARED_SERVER`` naming the shared one. That server stands in: the
app dictates on it whenever the shared Kass can't be reached.

The Rust server does the sharing (kass-server's ``sync`` module): it copies
what dictation reads from the shared Kass, sends it what the user did on
the stand-in once it's back, wakes the stand-in's models when the app falls
back to it, and frees them again. The worker only skips loading its models
at startup.
"""

import os

SHARED_SERVER_ENV = "KASS_SHARED_SERVER"


def shared_server() -> str | None:
    """The shared Kass this server stands in for, or None when it is the one in use."""
    url = os.environ.get(SHARED_SERVER_ENV, "").strip().rstrip("/")
    return url or None


def standing_in() -> bool:
    return shared_server() is not None
