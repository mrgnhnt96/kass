"""The model calls the Rust server (kass-server) makes for /models/*.

The Rust server owns the model files: it downloads, measures, deletes and
moves them, and answers /models/* and /tasks/*. The worker only says which
models are loaded, loads one whose files are downloaded, and unloads one.
"""

import asyncio
import logging

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

router = APIRouter(prefix="/worker", tags=["worker"])
logger = logging.getLogger(__name__)


class ModelName(BaseModel):
    model_name: str


def _config(model_name: str):
    from ..backends import get_model_config

    config = get_model_config(model_name)
    if not config:
        raise HTTPException(status_code=400, detail=f"Unknown model: {model_name}")
    return config


@router.post("/models/loaded")
async def loaded_models():
    """The names of the models loaded now."""
    from ..backends import check_model_loaded, get_all_model_configs

    return {"loaded": [config.model_name for config in get_all_model_configs() if check_model_loaded(config)]}


@router.post("/models/load")
async def load_model(request: ModelName):
    """Load a model the Rust server just downloaded; its error when it can't."""
    from ..backends import get_model_load_func

    load = get_model_load_func(_config(request.model_name))
    try:
        result = load()
        if asyncio.iscoroutine(result):
            await result
    except Exception as e:
        logger.exception("Loading %s failed", request.model_name)
        return {"error": str(e)}
    return {}


@router.post("/models/unload")
async def unload_model(request: ModelName):
    """Free a model's memory; whether it was loaded, or the error."""
    from ..backends import unload_model_by_config

    config = _config(request.model_name)
    try:
        return {"was_loaded": await unload_model_by_config(config)}
    except Exception as e:
        logger.exception("Unloading %s failed", request.model_name)
        return {"error": str(e)}
