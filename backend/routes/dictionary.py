"""What the Rust server needs from the worker for the dictionary (docs/plans/DICTIONARIES.md).

The dictionary's routes and its kass.db rows live in the Rust server; the worker
keeps reading kass.db for dictations and only hears when to reload it, and counts
prompt terms with Whisper's tokenizer.
"""

import logging

from fastapi import APIRouter
from pydantic import BaseModel

from ..services import dictionary

router = APIRouter(prefix="/worker/dictionary", tags=["worker"])
logger = logging.getLogger(__name__)


class TokenCountsRequest(BaseModel):
    terms: list[str]


class TokenCountsResponse(BaseModel):
    # None when no Whisper model is loaded: the server estimates instead.
    counts: list[int] | None


def _tokenizer():
    from ..services.transcribe import get_whisper_model

    try:
        whisper = get_whisper_model()
        if whisper.is_loaded():
            return whisper.model.get_tokenizer(language="en")
    except Exception:
        logger.warning("Could not read Whisper's tokenizer; the server estimates term sizes", exc_info=True)
    return None


@router.post("/changed")
async def changed():
    dictionary.invalidate()
    return {"ok": True}


@router.post("/token-counts", response_model=TokenCountsResponse)
async def token_counts(request: TokenCountsRequest):
    tokenizer = _tokenizer()
    if tokenizer is None:
        return TokenCountsResponse(counts=None)
    return TokenCountsResponse(counts=[len(tokenizer.encode(" " + term)) for term in request.terms])
