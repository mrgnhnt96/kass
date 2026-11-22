"""
Entry point for PyInstaller-bundled kass server.

This module provides an entry point that works with PyInstaller by using
absolute imports instead of relative imports.
"""

import os
import sys

# The app can close before this server notices and exits. Protect output for the
# entire process lifetime, not only while the initial pipe is still connected.
from backend.utils.safe_output import protect_standard_streams

protect_standard_streams()

# PyInstaller + multiprocessing: child processes re-execute the frozen binary
# with internal arguments. freeze_support() handles this and exits early.
import multiprocessing

multiprocessing.freeze_support()

if "--improve-model" in sys.argv:
    from backend.services.model_improvement.worker import main as improvement_main

    improvement_main(sys.argv[sys.argv.index("--improve-model") + 1 :])
    sys.exit(0)

# Fast path: handle --version before any heavy imports so the Rust
# version check doesn't block on loading MLX, transformers etc.
if "--version" in sys.argv:
    from backend import __version__

    print(f"kass-server {__version__}")
    sys.exit(0)

import logging

# Set up logging FIRST, before any imports that might fail
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    stream=sys.stderr,  # Log to stderr so the Rust server captures it
)
logger = logging.getLogger(__name__)

# Log startup immediately to confirm binary execution
logger.info("=" * 60)
logger.info("kass-server starting up...")
logger.info(f"Python version: {sys.version}")
logger.info(f"Executable: {sys.executable}")
logger.info(f"Arguments: {sys.argv}")
logger.info("=" * 60)

try:
    logger.info("Importing argparse...")
    import argparse

    logger.info("Importing uvicorn...")
    import uvicorn

    logger.info("Standard library imports successful")

    # Import the FastAPI app from the backend package
    logger.info("Importing backend.config...")
    from backend import config

    logger.info("Importing backend.database...")
    from backend import database

    logger.info("Importing backend.main (this may take a while due to MLX/transformers)...")
    from backend.main import app

    logger.info("Backend imports successful")
except Exception as e:
    logger.error(f"Failed to import required modules: {e}", exc_info=True)
    sys.exit(1)


def _log_to_file(data_dir):
    """Keep server logs on disk; the desktop app discards the sidecar's stderr."""
    from logging.handlers import RotatingFileHandler

    try:
        log_dir = os.path.join(data_dir, "logs")
        os.makedirs(log_dir, exist_ok=True)
        handler = RotatingFileHandler(os.path.join(log_dir, "server.log"), maxBytes=2 * 1024 * 1024, backupCount=3)
        handler.setFormatter(logging.Formatter("%(asctime)s - %(name)s - %(levelname)s - %(message)s"))
        logging.getLogger().addHandler(handler)
    except Exception:
        logger.warning("Could not open the server log file", exc_info=True)


def _start_parent_watchdog(parent_pid, data_dir=None):
    """Monitor parent process and exit if it dies.

    This is the clean shutdown mechanism: the server monitors its parent and
    shuts itself down gracefully when the app is gone.
    """
    import os
    import signal
    import threading
    import time

    # Set up a file logger so we can debug in production
    watchdog_logger = logging.getLogger("watchdog")
    if data_dir:
        try:
            log_dir = os.path.join(data_dir, "logs")
            os.makedirs(log_dir, exist_ok=True)
            fh = logging.FileHandler(os.path.join(log_dir, "watchdog.log"))
            fh.setFormatter(logging.Formatter("%(asctime)s - %(message)s"))
            watchdog_logger.addHandler(fh)
        except Exception:
            pass
    watchdog_logger.setLevel(logging.INFO)

    def _is_pid_alive(pid):
        """Check if a process with the given PID exists."""
        try:
            os.kill(pid, 0)
            return True
        except (OSError, PermissionError):
            return False

    def _watch():
        watchdog_logger.info(f"Parent watchdog started, monitoring PID {parent_pid}, server PID {os.getpid()}")
        # Verify parent is alive before starting the loop
        alive = _is_pid_alive(parent_pid)
        watchdog_logger.info(f"Parent PID {parent_pid} initial check: alive={alive}")
        if not alive:
            watchdog_logger.warning(f"Parent PID {parent_pid} not found on first check — disabling watchdog")
            return
        while True:
            if not _is_pid_alive(parent_pid):
                watchdog_logger.info(f"Parent process {parent_pid} gone, shutting down server...")
                os.kill(os.getpid(), signal.SIGTERM)
                return
            time.sleep(2)

    t = threading.Thread(target=_watch, daemon=True)
    t.start()


if __name__ == "__main__":
    try:
        parser = argparse.ArgumentParser(description="kass backend server")
        parser.add_argument(
            "--host",
            type=str,
            default="127.0.0.1",
            help="Host to bind to (use 0.0.0.0 for remote access)",
        )
        parser.add_argument(
            "--port",
            type=int,
            default=8000,
            help="Port to bind to",
        )
        parser.add_argument(
            "--data-dir",
            type=str,
            default=None,
            help="Data directory for the database and recorded audio",
        )
        parser.add_argument(
            "--parent-pid",
            type=int,
            default=None,
            help="PID of parent process to monitor; server exits when parent dies",
        )
        parser.add_argument(
            "--version",
            action="store_true",
            help="Print version and exit (handled above, kept for argparse help)",
        )
        args = parser.parse_args()

        if args.parent_pid is not None and args.parent_pid <= 0:
            parser.error("--parent-pid must be a positive integer")

        # Register parent watchdog to start after server is fully ready
        if args.parent_pid is not None:
            _parent_pid = args.parent_pid
            _data_dir = args.data_dir

            @app.on_event("startup")
            async def _on_startup():
                _start_parent_watchdog(_parent_pid, _data_dir)

        logger.info(f"Parsed arguments: host={args.host}, port={args.port}, data_dir={args.data_dir}")

        # Set data directory if provided
        if args.data_dir:
            logger.info(f"Setting data directory to: {args.data_dir}")
            config.set_data_dir(args.data_dir)
            _log_to_file(args.data_dir)

        # Initialize database after data directory is set
        logger.info("Initializing database...")
        database.init_db()
        logger.info("Database initialized successfully")

        logger.info(f"Starting uvicorn server on {args.host}:{args.port}...")
        uvicorn.run(
            app,
            host=args.host,
            port=args.port,
            log_level="info",
            # Rust serves Kass's WebSocket; the worker has none.
            ws="none",
        )
    except Exception as e:
        logger.error(f"Server startup failed: {e}", exc_info=True)
        sys.exit(1)
