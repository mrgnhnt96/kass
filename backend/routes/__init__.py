"""Route registration for the model worker the Rust server (kass-server) calls."""

from fastapi import FastAPI


def register_routers(app: FastAPI) -> None:
    """Include all worker routers on the application."""
    from .dictionary import router as dictionary_router
    from .health import router as health_router
    from .learning import events_router as learning_events_router, router as learning_router
    from .models import router as models_router
    from .settings import router as settings_router
    from .stream_models import router as stream_models_router
    from .sync import router as sync_router

    app.include_router(health_router)
    app.include_router(learning_router)
    app.include_router(learning_events_router)
    app.include_router(models_router)
    app.include_router(settings_router)
    app.include_router(dictionary_router)
    app.include_router(sync_router)
    app.include_router(stream_models_router)
