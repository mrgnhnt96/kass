"""Existing installs retain their settings when microphone options are added."""

from sqlalchemy import create_engine, inspect, text

from backend.database.migrations import _migrate_capture_settings


def test_microphone_migration_preserves_existing_settings_and_is_idempotent():
    engine = create_engine("sqlite:///:memory:")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, auto_refine BOOLEAN)"))
        connection.execute(text("INSERT INTO capture_settings VALUES (1, 0)"))
    for _ in range(2):
        _migrate_capture_settings(engine, inspect(engine), {"capture_settings"})
    with engine.begin() as connection:
        row = connection.execute(text("SELECT auto_refine, input_device_id FROM capture_settings WHERE id = 1")).one()
        assert tuple(row) == (0, None)
        connection.execute(text("UPDATE capture_settings SET input_device_id = 'usb'"))
    _migrate_capture_settings(engine, inspect(engine), {"capture_settings"})
    with engine.connect() as connection:
        assert tuple(
            connection.execute(text("SELECT auto_refine, input_device_id FROM capture_settings WHERE id = 1")).one()
        ) == (0, "usb")


def test_sound_cues_are_on_at_half_volume_after_upgrading(tmp_path):
    from sqlalchemy import create_engine, text

    from backend.database.migrations import run_migrations

    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR)"))
        connection.execute(text("INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo')"))
    run_migrations(engine)
    with engine.connect() as connection:
        row = connection.execute(text("SELECT sound_cues, sound_cue_volume FROM capture_settings")).one()
        assert tuple(row) == (1, 0.5)


def test_live_text_is_off_until_turned_on(tmp_path):
    from sqlalchemy import create_engine, text

    from backend.database.migrations import run_migrations

    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR)"))
        connection.execute(text("INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo')"))
    run_migrations(engine)
    with engine.connect() as connection:
        assert connection.execute(text("SELECT live_text FROM capture_settings")).scalar_one() == 0


def test_expressive_text_is_on_after_upgrading(tmp_path):
    from sqlalchemy import create_engine, text

    from backend.database.migrations import run_migrations

    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR)"))
        connection.execute(text("INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo')"))
    run_migrations(engine)
    with engine.connect() as connection:
        assert connection.execute(text("SELECT expressive FROM capture_settings")).scalar_one() == 1


def test_installs_from_before_onboarding_skip_it(tmp_path):
    from sqlalchemy import create_engine, text

    from backend.database.migrations import run_migrations

    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR)"))
        connection.execute(text("INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo')"))
    run_migrations(engine)
    with engine.connect() as connection:
        assert connection.execute(text("SELECT onboarding_completed FROM capture_settings")).scalar_one() == 1


def test_new_installs_start_with_onboarding():
    from backend.models import CaptureSettingsResponse

    assert CaptureSettingsResponse.model_fields["onboarding_completed"].default is False


def test_free_space_is_measured_where_the_cache_will_be(tmp_path):
    from backend.routes.models import free_mb_at

    assert free_mb_at(tmp_path / "not" / "created" / "yet") > 0
