# ABOUTME: pytest fixtures for the integration test tier.
# ABOUTME: Auto-skips every integration test when no Postgres is reachable, so
# ABOUTME: the default `pytest` run needs neither a database nor credentials.

from __future__ import annotations

import os
import pathlib

import pytest

MIGRATION = (
    pathlib.Path(__file__).resolve().parent.parent
    / "supabase"
    / "migrations"
    / "0001_reflect_memory_phase1.sql"
)


def _dsn() -> str | None:
    """Connection string for the integration DB, or None to skip.

    Reads ``DATABASE_URL`` (the spec's standard name); falls back to
    ``REFLECT_TEST_DATABASE_URL`` for a dedicated throwaway test database. No
    default — absence means "skip", never "connect to something unexpected".
    """
    return os.environ.get("DATABASE_URL") or os.environ.get("REFLECT_TEST_DATABASE_URL")


@pytest.fixture(scope="session")
def _migrated_dsn() -> str:
    """Apply the Phase 1 migration once per session; skip cleanly if no DB."""
    dsn = _dsn()
    if not dsn:
        pytest.skip("no DATABASE_URL / REFLECT_TEST_DATABASE_URL set — integration tests skipped")

    psycopg = pytest.importorskip("psycopg", reason="psycopg not installed — integration skipped")

    try:
        conn = psycopg.connect(dsn)
    except Exception as exc:  # noqa: BLE001 — any connect failure means "skip"
        pytest.skip(f"Postgres not reachable ({exc}) — integration tests skipped")

    try:
        conn.autocommit = True
        with conn.cursor() as cur:
            # Migration is parameter-free, so psycopg sends it as a multi-statement
            # script. It is re-runnable (IF NOT EXISTS / CREATE OR REPLACE).
            cur.execute(MIGRATION.read_text())
    finally:
        conn.close()
    return dsn


@pytest.fixture
def conn(_migrated_dsn):
    """A fresh, clean (truncated) mapping-row connection per test."""
    import psycopg
    from psycopg.rows import dict_row

    c = psycopg.connect(_migrated_dsn, row_factory=dict_row)
    with c.cursor() as cur:
        cur.execute(
            "truncate reflect_memory.memory_items, reflect_memory.entities, "
            "reflect_memory.edges cascade;"
        )
    c.commit()
    try:
        yield c
    finally:
        c.close()


@pytest.fixture
def store(conn):
    from ainb_reflect_memory import MemoryStore

    return MemoryStore(conn)
