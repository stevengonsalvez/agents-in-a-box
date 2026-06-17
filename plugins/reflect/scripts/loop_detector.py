#!/usr/bin/env python3
# ABOUTME: Agent tool-loop detection (port SG5, pattern from ByteRover's overlap locks).
# ABOUTME: Sliding window of (tool, arg-hash) per session; flags repeats and A-B oscillation.
"""Tool-loop detector.

Port SG5. A stuck agent repeats the same tool call (or ping-pongs between
two) — and the user's *next prompt* is almost always a correction, which is
the highest-signal learning in the session. This module gives the PostToolUse
hook a cheap, deterministic "the agent is looping" signal so it can arm the
existing mini-learning watcher with ``reason="loop"``.

Detection rules (per session, sliding window of the last ``WINDOW`` calls):

* **repeat**      — the same ``(tool, arg_hash)`` appears ``REPEAT_N`` times
                    consecutively.
* **oscillation** — the tail alternates A,B,A,B for ``OSC_CYCLES`` full
                    cycles (4 calls), where A != B.

State lives at ``$REFLECT_STATE_DIR/loops/<session_id>.json`` (same layout as
the armed/ dir). Entries are pruned past the window; files are tiny.

Everything here is stdlib-only and silent-fail shaped: any error returns
``None`` (no detection) rather than raising into the hook.
"""
from __future__ import annotations

import hashlib
import json
import os
import time
from pathlib import Path
from typing import Optional

__all__ = ["record_call", "LoopHit", "WINDOW", "REPEAT_N", "OSC_CYCLES"]

WINDOW = 10        # sliding window size (calls)
REPEAT_N = 3       # identical consecutive calls => loop
OSC_CYCLES = 2     # A,B repeated this many times (== 4 calls) => oscillation
_STATE_TTL_S = 6 * 3600  # stale session state expires after 6h


class LoopHit(dict):
    """Detection result: {"kind": "repeat"|"oscillation", "tool": ..., "count": ...}."""


def _state_dir() -> Path:
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / "loops"


def _state_path(session_id: str) -> Path:
    safe = "".join(c if c.isalnum() or c in "-_" else "_" for c in session_id)[:64]
    return _state_dir() / f"{safe}.json"


def _arg_hash(tool_input) -> str:
    try:
        canonical = json.dumps(tool_input, sort_keys=True, default=str)
    except (TypeError, ValueError):
        canonical = str(tool_input)
    return hashlib.sha1(canonical.encode("utf-8", errors="replace")).hexdigest()[:12]


def _load(path: Path) -> list[dict]:
    try:
        data = json.loads(path.read_text())
        if not isinstance(data, dict):
            return []
        if time.time() - float(data.get("updated", 0)) > _STATE_TTL_S:
            return []
        calls = data.get("calls", [])
        return calls if isinstance(calls, list) else []
    except (OSError, json.JSONDecodeError, ValueError):
        return []


def _save(path: Path, calls: list[dict]) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps({"updated": time.time(), "calls": calls[-WINDOW:]}))
        tmp.replace(path)
    except OSError:
        pass


def _detect(calls: list[dict]) -> Optional[LoopHit]:
    keys = [(c.get("tool", ""), c.get("h", "")) for c in calls]
    if len(keys) >= REPEAT_N:
        tail = keys[-REPEAT_N:]
        if len(set(tail)) == 1:
            return LoopHit(kind="repeat", tool=tail[0][0], count=REPEAT_N)
    need = OSC_CYCLES * 2
    if len(keys) >= need:
        tail = keys[-need:]
        a, b = tail[0], tail[1]
        if a != b and all(tail[i] == (a if i % 2 == 0 else b) for i in range(need)):
            return LoopHit(kind="oscillation", tool=f"{a[0]}<->{b[0]}", count=OSC_CYCLES)
    return None


def record_call(session_id: str, tool_name: str, tool_input) -> Optional[LoopHit]:
    """Record one tool call; return a LoopHit when the window shows a loop.

    On detection the window is RESET so one stuck stretch arms once, not on
    every subsequent call.
    """
    if not session_id or not tool_name:
        return None
    try:
        path = _state_path(session_id)
        calls = _load(path)
        calls.append({"tool": tool_name, "h": _arg_hash(tool_input), "ts": time.time()})
        hit = _detect(calls)
        _save(path, [] if hit else calls)
        return hit
    except Exception:  # noqa: BLE001 — hook-adjacent: never raise
        return None
