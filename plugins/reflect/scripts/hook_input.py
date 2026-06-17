"""Cross-harness hook stdin readers for reflect hooks.

Claude Code and Codex CLI send hook payloads with **snake_case** keys
(``session_id``, ``transcript_path``, ``tool_name``, ``tool_response``).
GitHub Copilot CLI sends the same data with **camelCase** keys
(``sessionId``, ``transcriptPath``, ``toolName``, ``toolResult``). Rather
than scatter ``data.get("session_id") or data.get("sessionId")`` across six
hook scripts, this module centralises the lookup.

Two design rules, both load-bearing:

  1. **First-present-wins, not falsy-fallback.** We check ``key in data``
     in priority order and return the first key that is *present*, even
     when its value is falsy (empty dict / empty string / ``0``). A bare
     ``a or b`` would wrongly fall through on a meaningful empty
     ``tool_response`` (``{}`` is a legitimate "tool returned nothing"
     payload). Presence ordering keeps the snake_case branch winning for
     claude/codex whenever the snake_case key exists — so their behaviour
     is byte-identical to before (the camelCase alias is never consulted).

  2. **Snake_case is always tried first.** The camelCase alias only gets a
     look-in when the canonical snake_case key is genuinely absent — i.e.
     on Copilot. This guarantees we never change what claude/codex see.

The helpers are intentionally tiny and dependency-free so each hook can
import them the same way it imports :mod:`silent_fail` — with a
``try/except ImportError`` inline fallback. That fallback matters because
in *deployed* form the two recall hooks (``skills/recall/hooks/*.py``)
resolve their ``scripts/`` dir to a path that does not exist (a
pre-existing layout quirk shared with ``silent_fail``), so the import
no-ops there and the inline copy takes over. In *source* form (where the
unit tests run) the import resolves and exercises this module directly.
"""

from __future__ import annotations

from typing import Any, Mapping, Sequence


def _first_present(data: Mapping[str, Any], keys: Sequence[str], default: Any) -> Any:
    """Return ``data[key]`` for the first ``key`` present in ``data``.

    Presence (``key in data``) — *not* truthiness — decides the winner, so
    a meaningful falsy value (``{}``, ``""``, ``0``) under an earlier key
    is honoured instead of skipped. Returns ``default`` when none match.
    """
    if not isinstance(data, Mapping):
        return default
    for key in keys:
        if key in data:
            return data[key]
    return default


def get_session_id(data: Mapping[str, Any], default: str = "") -> str:
    """Session id: ``session_id`` (claude/codex) → ``sessionId`` (copilot)."""
    return _first_present(data, ("session_id", "sessionId"), default)


def get_transcript_path(data: Mapping[str, Any], default: str = "") -> str:
    """Transcript path: ``transcript_path`` → ``transcriptPath``."""
    return _first_present(data, ("transcript_path", "transcriptPath"), default)


def get_tool_name(data: Mapping[str, Any], default: str = "") -> str:
    """Tool name across harnesses.

    Claude/codex use ``tool`` or ``tool_name``; Copilot uses ``toolName``.
    We keep the legacy ``tool`` → ``tool_name`` order (matching the prior
    inline read in ``posttooluse_minilearning.py``) and only fall through
    to ``toolName`` when neither snake_case key is present.
    """
    return _first_present(data, ("tool", "tool_name", "toolName"), default)


def get_tool_response(data: Mapping[str, Any], default: Any = None) -> Any:
    """Tool response/result payload.

    Claude/codex use ``tool_response`` (or the older ``response``); Copilot
    uses ``toolResult``. Presence-first matters most here — an empty
    ``{}`` result is a real value, not a reason to fall through.
    """
    if default is None:
        default = {}
    return _first_present(
        data, ("tool_response", "response", "toolResult"), default
    )


def get_cwd(data: Mapping[str, Any], default: str = "") -> str:
    """Working directory: ``cwd`` is shared across all three harnesses."""
    return _first_present(data, ("cwd",), default)
