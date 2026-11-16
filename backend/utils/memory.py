"""System memory pressure, logged so a slow dictation shows whether the Mac was swapping."""

import logging

import psutil

logger = logging.getLogger(__name__)

GB = 1024**3
MB = 1024**2


def summary(since: tuple[int, int] | None = None) -> str:
    """Free memory and swap now, plus paging since ``since`` when given."""
    try:
        swap = psutil.swap_memory()
        text = f"mem_available={psutil.virtual_memory().available / GB:.1f}G swap_used={swap.used / GB:.1f}G"
        if since is not None:
            text += f" pageins={(swap.sin - since[0]) / MB:.0f}M pageouts={(swap.sout - since[1]) / MB:.0f}M"
        return text
    except Exception:
        logger.debug("Could not read memory pressure", exc_info=True)
        return "memory=unknown"
