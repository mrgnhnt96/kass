"""What the Rust server tells the worker about the learned writing style.

The ``/writing-style`` routes, teach sessions included, live in the Rust
server (tauri/src-tauri/server/src/routes/writing_style.rs and teach.rs),
which writes the files each style learned in
(tauri/src-tauri/server/src/learned.rs); it tells the worker here.
"""

from fastapi import APIRouter

from ..services import correction_notes, personal_examples, writing_style

worker_router = APIRouter(prefix="/worker/learned", tags=["worker"])


@worker_router.post("/changed")
def learned_changed():
    """The Rust server changed writing-style.json or correction-notes.json:
    read them afresh, with the examples grouped from them."""
    writing_style.reload()
    correction_notes.reload()
    personal_examples.invalidate()
    return {"ok": True}
