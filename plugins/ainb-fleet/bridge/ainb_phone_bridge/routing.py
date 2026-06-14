# ABOUTME: Pure target-routing logic for the phone bridge.
#
# Two concerns, both pure:
#   1. parse_target_prefix("name: hello", names) -> ("name", "hello")
#      Bare text (no recognised "name:" prefix) -> (None, text).
#   2. resolve_target(parsed_name, sessions) -> the session to relay to.
#      Conductor-first when present; degrades to any named session otherwise so
#      the bridge is useful BEFORE the separate conductor track lands.
#
# A "session" here is a lightweight dataclass the ainb client builds from
# `ainb list --format json`. Keeping these functions free of I/O makes the
# routing contract testable without a live fleet.

from __future__ import annotations

import re
from dataclasses import dataclass

# A target name is a leading "<name>:" token. Names mirror ainb workspace /
# tmux names: alphanumerics plus the separators ainb actually uses.
_PREFIX_RE = re.compile(r"^([A-Za-z0-9][A-Za-z0-9._-]*)\s*:\s*(.*)$", re.DOTALL)

# Heuristic: a session whose name starts with this is treated as a conductor and
# preferred as the default target. Matches the agent-deck "conductor-<name>"
# convention while degrading gracefully when no such session exists.
CONDUCTOR_PREFIXES = ("conductor", "conductor-")


@dataclass(frozen=True)
class TargetSession:
    """A relay target resolved from `ainb list --format json`."""

    name: str  # workspace_name (human-facing routing key)
    tmux_session: str  # tmux session name used by send-keys
    cwd: str  # worktree_path — locates the JSONL transcript
    session_id: str  # ainb session id
    is_conductor: bool = False


def parse_target_prefix(text: str, known_names: list[str]) -> tuple[str | None, str]:
    """Split an inbound message into ``(target_name, message)``.

    A leading ``"<name>:"`` selects a named session ONLY when ``<name>`` matches
    one of ``known_names`` (case-insensitive). Otherwise the whole string is the
    message and the target is ``None`` (caller falls back to the default).

    The known-names guard is what stops a normal sentence like
    ``"note: fix this"`` from being mis-routed to a session called ``note``.
    """
    if not text:
        return None, ""

    m = _PREFIX_RE.match(text)
    if not m:
        return None, text

    candidate = m.group(1)
    rest = m.group(2)
    # Match case-insensitively but return the session's canonical name so
    # downstream routing/logging is consistent regardless of how it was typed.
    for name in known_names:
        if name.lower() == candidate.lower():
            return name, rest.strip()

    # Looks like a prefix but isn't a real session — treat as plain text.
    return None, text


def _conductor_sort_key(s: TargetSession) -> tuple[int, str]:
    # Conductors first (0), then alphabetical by name for deterministic default.
    return (0 if s.is_conductor else 1, s.name.lower())


def is_conductor_name(name: str) -> bool:
    """True if a session name looks like a conductor session."""
    lo = name.lower()
    return lo == "conductor" or lo.startswith("conductor-")


def default_target(sessions: list[TargetSession]) -> TargetSession | None:
    """Pick the default relay target.

    Conductor sessions win; among equals the alphabetically-first name is
    chosen so the default is stable across restarts. Returns ``None`` when the
    fleet is empty.
    """
    if not sessions:
        return None
    return sorted(sessions, key=_conductor_sort_key)[0]


def resolve_target(parsed_name: str | None, sessions: list[TargetSession]) -> TargetSession | None:
    """Resolve a relay target from an optional name + the live session list.

    - ``parsed_name`` given: exact (case-insensitive) match on ``name``; if no
      such session is running, returns ``None`` (caller reports "no such
      session" rather than silently relaying to the wrong place).
    - ``parsed_name`` absent: the conductor-first default (degrades to any
      named session).
    """
    if parsed_name is not None:
        wanted = parsed_name.lower()
        for s in sessions:
            if s.name.lower() == wanted:
                return s
        return None
    return default_target(sessions)
