"""
Pydantic models for the worker's own responses.
"""

from pydantic import BaseModel


class HealthResponse(BaseModel):
    """Response model for health check."""

    status: str
    model_loaded: bool  # Whether a Whisper model is loaded
    model_downloaded: bool | None = None  # The Rust server answers this
    model_size: str | None = None  # Loaded Whisper model size
    gpu_available: bool
    gpu_type: str | None = None  # "Metal (Apple Silicon via MLX)", "MPS (Apple Silicon)", or None
    backend_type: str | None = None  # Always "mlx"
    version: str | None = None
    started_at: float | None = None  # Server process start, Unix seconds
    pid: int | None = None
    peak_memory_mb: int | None = None  # Peak resident memory of the server process
