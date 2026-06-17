#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Lifecycle events for Reflect (C4) — append-only JSONL + per-event shell hooks.

Reflect mutates the knowledge base at a handful of meaningful moments: a
learning is created, a learning is revised, a skill doc is refreshed, a
consolidation pass finishes. External tooling wants to react to those moments
(auto-update CLAUDE.md, ping Slack, append to a digest) without polling the DB.

This module is the emit side of that contract — a Hindsight-style webhook
fan-out, but local and dependency-free:

  * :func:`emit` appends exactly ONE JSONL line per lifecycle moment to
    ``$REFLECT_STATE_DIR/events.jsonl`` (default ``~/.reflect/events.jsonl``),
    then fires the configured shell hook for that event (if any).

  * The append is transactional/append-safe: a single ``os.write`` to a fd
    opened ``O_APPEND|O_CREAT|O_WRONLY``. On POSIX a write of a small line to
    an O_APPEND fd is atomic, so concurrent emitters never interleave or
    clobber each other's lines — no lock file, no read-modify-write window.

  * Shell hooks are configured under ``[events.on]`` in reflect.toml
    (``events.on.<event> = "<command>"``). The command runs once per matching
    event with the event name + JSON payload exported in the environment, so a
    hook can do ``$REFLECT_EVENT`` / ``$REFLECT_EVENT_PAYLOAD`` and branch.
    A per-event env override (``REFLECT_EVENTS_ON_<EVENT>``) is honoured first
    for tests and ad-hoc wiring.

Like the rest of the reflect hook surface, every public entry point is
best-effort: ``emit`` swallows its own I/O and subprocess errors so a broken
hook or an unwritable state dir can never break the drain/cascade that calls it.
``REFLECT_STATE_DIR`` is resolved at call time (not import time) so tests and
runtime callers can repoint it mid-process.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Mapping, Optional

_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))


# --- Closed event vocabulary ---------------------------------------------

#: The four lifecycle moments reflect emits. A closed set: ``emit`` rejects any
#: name outside it so a typo in a call site fails loudly in tests rather than
#: silently writing an un-subscribable event.
EVENT_TYPES: tuple[str, ...] = (
    "learning.created",
    "learning.updated",
    "skill.refreshed",
    "consolidation.completed",
)


class UnknownEvent(ValueError):
    """Raised when :func:`emit` is asked to emit an event outside EVENT_TYPES."""


# --- Paths (resolved at call-time, NOT import-time) -----------------------

def state_dir() -> Path:
    """Resolve ``REFLECT_STATE_DIR`` (or ``~/.reflect``) at call time.

    Per-call resolution mirrors ``silent_fail.state_dir`` so callers and tests
    can repoint the env after import without restarting the process.
    """
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def events_path() -> Path:
    """Path to the append-only lifecycle event log."""
    return state_dir() / "events.jsonl"


# --- Config-driven shell hooks -------------------------------------------

def _env_override_for(event: str) -> Optional[str]:
    """A per-event shell hook supplied via the environment, if present.

    ``learning.created`` -> ``REFLECT_EVENTS_ON_LEARNING_CREATED``. Honoured
    ahead of the TOML config so tests (and quick one-off wiring) can attach a
    hook without writing a reflect.toml.
    """
    key = "REFLECT_EVENTS_ON_" + event.upper().replace(".", "_")
    val = os.environ.get(key)
    return val if val else None


def _config_hook_for(event: str) -> Optional[str]:
    """The shell command configured under ``[events.on]`` for *event*, if any."""
    try:
        from reflect_config import load_config

        cfg = load_config()
    except Exception:
        return None
    on = (cfg.get("events") or {}).get("on") or {}
    if not isinstance(on, Mapping):
        return None
    cmd = on.get(event)
    return cmd if isinstance(cmd, str) and cmd.strip() else None


def hook_for(event: str) -> Optional[str]:
    """Resolve the shell hook command for *event* (env override wins over TOML)."""
    return _env_override_for(event) or _config_hook_for(event)


def _run_hook(event: str, payload: Mapping[str, Any]) -> None:
    """Run the configured shell hook for *event*, if one exists. Never raises.

    The command runs through the shell so users can write pipelines
    (``slack-notify | tee -a digest``). The event name and JSON payload are
    exported so the hook can branch on which event fired without re-parsing the
    JSONL tail.
    """
    cmd = hook_for(event)
    if not cmd:
        return
    try:
        env = {
            **os.environ,
            "REFLECT_EVENT": event,
            "REFLECT_EVENT_PAYLOAD": json.dumps(payload, sort_keys=True, default=str),
        }
        subprocess.run(cmd, shell=True, env=env, check=False)
    except Exception:
        # A broken hook must never break the lifecycle moment that fired it.
        pass


# --- Emit ----------------------------------------------------------------

def emit(event: str, payload: Optional[Mapping[str, Any]] = None) -> bool:
    """Record one lifecycle event and fire its configured shell hook.

    Appends exactly ONE JSON line to ``$REFLECT_STATE_DIR/events.jsonl`` of the
    shape ``{"event": <name>, "ts": <epoch>, "payload": {...}}`` then runs the
    ``[events.on.<event>]`` shell hook (if configured) for that event only.

    The append is a single ``os.write`` to an ``O_APPEND`` fd — atomic for a
    small line on POSIX, so concurrent emitters never interleave. Best-effort:
    on an I/O error it returns ``False`` rather than raising, so the drain /
    cascade caller is never broken by a full disk or read-only state dir.

    Returns ``True`` if the line was written, ``False`` on a write failure.
    Raises :class:`UnknownEvent` for an out-of-vocabulary event name — that is a
    programming error in a call site, not a runtime condition to swallow.
    """
    if event not in EVENT_TYPES:
        raise UnknownEvent(
            f"unknown lifecycle event {event!r}; expected one of {EVENT_TYPES}"
        )

    record: dict[str, Any] = {
        "event": event,
        "ts": time.time(),
        "payload": dict(payload) if payload else {},
    }
    line = json.dumps(record, sort_keys=True, default=str) + "\n"

    written = False
    try:
        path = events_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        # O_APPEND makes the write race-safe across processes: each os.write of
        # a single small line lands atomically at the current end of file, so
        # parallel emitters never clobber or interleave one another's lines.
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
        try:
            os.write(fd, line.encode("utf-8"))
        finally:
            os.close(fd)
        written = True
    except Exception:
        # Persisting the event failed — still attempt the hook below so a
        # subscriber that only cares about the side effect (Slack ping) still
        # fires, but report the write failure to the caller.
        written = False

    _run_hook(event, record)
    return written


# --- CLI — handy for manual wiring / debugging ---------------------------

def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Emit a reflect lifecycle event")
    ap.add_argument("event", choices=EVENT_TYPES)
    ap.add_argument(
        "--payload",
        default=None,
        help="JSON object string attached to the event",
    )
    args = ap.parse_args()

    payload: Optional[Mapping[str, Any]] = None
    if args.payload:
        try:
            loaded = json.loads(args.payload)
            if isinstance(loaded, Mapping):
                payload = loaded
        except json.JSONDecodeError:
            print("--payload must be a JSON object", file=sys.stderr)
            sys.exit(2)

    ok = emit(args.event, payload)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
