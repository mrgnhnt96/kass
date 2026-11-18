"""Route registration for the kass API."""

from fastapi import FastAPI


def register_routers(app: FastAPI) -> None:
    """Include all domain routers on the application."""
    from .captures import router as captures_router, worker_router as captures_worker_router
    from .commands import router as commands_router
    from .dictionary import router as dictionary_router
    from .health import router as health_router
    from .learning import router as learning_router
    from .llm import router as llm_router
    from .models import router as models_router
    from .settings import router as settings_router
    from .stream_models import router as stream_models_router
    from .styles import router as styles_router
    from .sync import router as sync_router
    from .transcription import router as transcription_router
    from .writing_style import worker_router as learned_worker_router

    app.include_router(health_router)
    app.include_router(transcription_router)
    app.include_router(llm_router)
    app.include_router(captures_router)
    app.include_router(captures_worker_router)
    app.include_router(learning_router)
    app.include_router(commands_router)
    app.include_router(models_router)
    app.include_router(settings_router)
    app.include_router(styles_router)
    app.include_router(learned_worker_router)
    app.include_router(dictionary_router)
    app.include_router(sync_router)
    app.include_router(stream_models_router)
