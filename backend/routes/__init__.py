"""Route registration for the kass API."""

from fastapi import FastAPI


def register_routers(app: FastAPI) -> None:
    """Include all domain routers on the application."""
    from .capture_stream import router as capture_stream_router
    from .captures import router as captures_router
    from .commands import router as commands_router
    from .dictionary import router as dictionary_router
    from .health import router as health_router
    from .links import router as links_router
    from .llm import router as llm_router
    from .models import router as models_router
    from .settings import router as settings_router
    from .styles import router as styles_router
    from .sync import router as sync_router
    from .tasks import router as tasks_router
    from .transcription import router as transcription_router
    from .usage import router as usage_router
    from .writing_style import router as writing_style_router

    app.include_router(health_router)
    app.include_router(transcription_router)
    app.include_router(llm_router)
    app.include_router(capture_stream_router)
    app.include_router(captures_router)
    app.include_router(commands_router)
    app.include_router(models_router)
    app.include_router(settings_router)
    app.include_router(styles_router)
    app.include_router(tasks_router)
    app.include_router(writing_style_router)
    app.include_router(dictionary_router)
    app.include_router(links_router)
    app.include_router(usage_router)
    app.include_router(sync_router)
