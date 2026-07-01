"""Read-only loader for ``reflect.toml``.

The reflect ecosystem ships a ``reflect.toml`` (under ``plugins/reflect/``) that
documents tunables for several modes. Historically the ``reflect issues`` CLI
hard-coded its click defaults and never consulted that file, so the documented
``[issues]`` block (``repo`` / ``limit`` / ``model``) was inert.

This module loads the config once and exposes the parsed table. Resolution
order (first hit wins):

1. ``$REFLECT_CONFIG`` — explicit path override.
2. ``$REFLECT_STATE_DIR/reflect.toml`` — alongside the queue/ledger/db.
3. ``~/.reflect/reflect.toml`` — the default state dir.
4. The repo-bundled ``plugins/reflect/reflect.toml`` (best-effort, by walking up
   from this file) so an editable checkout works without extra setup.

The loader is tolerant: a missing or malformed file yields ``{}`` rather than
raising, so the CLI always has working defaults.
"""

from __future__ import annotations

import os
import tomllib
from functools import lru_cache
from pathlib import Path
from typing import Optional

_CONFIG_NAME = "reflect.toml"


def _candidate_paths() -> list[Path]:
    candidates: list[Path] = []

    override = os.environ.get("REFLECT_CONFIG")
    if override:
        candidates.append(Path(override).expanduser())

    state_dir = os.environ.get("REFLECT_STATE_DIR")
    if state_dir:
        candidates.append(Path(state_dir).expanduser() / _CONFIG_NAME)

    candidates.append(Path.home() / ".reflect" / _CONFIG_NAME)

    # Walk up from this file looking for plugins/reflect/reflect.toml so an
    # editable repo checkout picks up the bundled defaults.
    here = Path(__file__).resolve()
    for parent in here.parents:
        bundled = parent / "plugins" / "reflect" / _CONFIG_NAME
        if bundled.exists():
            candidates.append(bundled)
            break

    return candidates


def config_path() -> Optional[Path]:
    """Return the first existing ``reflect.toml`` in resolution order, or None."""
    for path in _candidate_paths():
        if path.exists():
            return path
    return None


@lru_cache(maxsize=1)
def load_config() -> dict:
    """Load and parse ``reflect.toml``. Returns ``{}`` if absent/unreadable."""
    path = config_path()
    if path is None:
        return {}
    try:
        with path.open("rb") as fh:
            return tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError):
        return {}


def issues_config() -> dict:
    """Return the ``[issues]`` table (``{}`` if missing)."""
    table = load_config().get("issues", {})
    return table if isinstance(table, dict) else {}
