"""Shared silent-fail mechanics for reflect hooks.

All reflect hooks (recall, reflect, drain, mini-learning) MUST exit 0
cleanly on any uncaught exception — see ``feedback_reflect_hooks_silent_fail``
in MEMORY.md. This module centralises the breadcrumb writer, credential
scrubber, and forensics log so hooks don't duplicate the same 30 lines.

Three primitives:

  * :func:`write_last_event` — atomic breadcrumb at
    ``$REFLECT_STATE_DIR/last-event.json`` (default ``~/.reflect/``). The
    status line reads this to render ⚠ recall failed fragments. Always
    safe to call; swallows its own errors.

  * :func:`forensics_log` — append-only developer log at
    ``$REFLECT_STATE_DIR/logs/reflect_<hook>.log``. NOT user-visible —
    debuggability for incident review only.

  * :func:`scrub_secrets` — best-effort credential masking applied to any
    text persisted via the above two. Not a security boundary; just
    keeps casual exception messages from leaking obvious tokens.

All three resolve ``REFLECT_STATE_DIR`` at call time (not import time) so
tests and runtime callers can change the env mid-process without
restarting.
"""

from __future__ import annotations

import json
import os
import re
import time
from pathlib import Path
from typing import Literal


# --- Paths (resolved at call-time, NOT import-time) -----------------------

def state_dir() -> Path:
    """Resolve ``REFLECT_STATE_DIR`` (or ``~/.reflect``) at call time.

    Per-call resolution lets test code or runtime callers change the env
    after this module has been imported — previously the path was bound
    at import time and was effectively immutable in-process.
    """
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def last_event_path() -> Path:
    return state_dir() / "last-event.json"


def logs_dir() -> Path:
    return state_dir() / "logs"


# --- Secret scrubbing ----------------------------------------------------

# Order matters: more specific patterns first so we don't double-mask.
# Each entry is (regex, group_index_to_mask). group_index=0 masks the whole
# match; >0 masks only the captured credential while preserving the prefix.
_SECRET_PATTERNS: list[tuple[re.Pattern[str], int]] = [
    # "Authorization: Bearer <token>" / "Authorization: <token>"
    (re.compile(r"(?i)Authorization\s*:\s*(?:Bearer\s+)?([A-Za-z0-9._\-/+]{8,})"), 1),
    # "X-Api-Key: <token>" / "X-Auth-Token: <token>" / similar headers
    (re.compile(r"(?i)X-(?:Api-)?(?:Key|Auth|Token)[^:]*:\s*([A-Za-z0-9._\-/+]{8,})"), 1),
    # "Bearer <token>" anywhere in text (e.g. exception "401 Bearer xxx").
    (re.compile(r"(?i)\bBearer\s+([A-Za-z0-9._\-/+]{8,})"), 1),
    # key=value style in URLs / config / env: "token=...", "api_key=...", "password=..."
    (re.compile(r"(?i)\b(?:token|api[_-]?key|password|passwd|secret|auth)\s*[:=]\s*['\"]?([A-Za-z0-9._\-/+]{8,})"), 1),
    # Provider-specific key shapes (whole match masked).
    (re.compile(r"sk-[A-Za-z0-9_\-]{20,}"), 0),    # OpenAI-style
    (re.compile(r"sk-ant-[A-Za-z0-9_\-]{20,}"), 0),  # Anthropic-style
    (re.compile(r"ghp_[A-Za-z0-9]{20,}"), 0),       # GitHub PAT
    (re.compile(r"ghs_[A-Za-z0-9]{20,}"), 0),       # GitHub app-token
    (re.compile(r"xox[abp]-[A-Za-z0-9\-]{20,}"), 0),  # Slack
    (re.compile(r"AKIA[0-9A-Z]{16}"), 0),           # AWS access key ID
]


def scrub_secrets(text: str) -> str:
    """Mask common credential patterns. Best-effort; not a security boundary.

    Returns ``text`` with any matched credentials replaced by
    ``***REDACTED***``. Patterns covered: Authorization/X-API headers,
    ``token=``/``api_key=``/``password=``/``secret=`` style values,
    and well-known provider key prefixes (sk-, ghp_, xox*, AKIA*).
    """
    if not text:
        return text
    result = text
    for pattern, group in _SECRET_PATTERNS:
        if group == 0:
            result = pattern.sub("***REDACTED***", result)
        else:
            def _mask(m: re.Match[str]) -> str:
                whole = m.group(0)
                secret = m.group(group)
                return whole.replace(secret, "***REDACTED***")
            result = pattern.sub(_mask, result)
    return result


# --- Atomic breadcrumb writer --------------------------------------------

def write_last_event(
    *,
    hook_name: str,
    event: Literal["ok", "error"],
    kind: str,
    detail: str,
) -> None:
    """Best-effort breadcrumb for the status line. Never raises.

    Writes ``last-event.json`` atomically (write to ``.tmp`` then
    ``replace``) so a concurrent status-line reader never sees a
    half-written file.

    ``event="ok"`` is reserved for the upcoming status-line counter view
    (status line shows "🧠 N recalled · M queued"). The silent-fail
    wrapper itself only ever writes ``event="error"``.
    """
    try:
        path = last_event_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "event": event,
            "hook": hook_name,
            "kind": kind,
            "detail": scrub_secrets(detail)[:500],
            "ts": time.time(),
        }
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload), encoding="utf-8")
        tmp.replace(path)
    except Exception:
        # If even the breadcrumb fails, we still MUST NOT surface
        # anything. Status line goes without a fresh event; that's fine.
        pass


# --- Forensics log -------------------------------------------------------

def forensics_log(hook_name: str, message: str) -> None:
    """Append ``message`` to ``$REFLECT_STATE_DIR/logs/<hook_name>.log``.

    Harness-agnostic — previously `precompact_reflect.py` hardcoded
    ``~/.claude/logs/reflect_precompact.log`` which sent codex-fired log
    lines into a Claude-flavoured directory. This helper picks the
    unified reflect state dir so logs from both harnesses land in the
    same place. The ``reflect_`` prefix is dropped (logs already live
    under ``~/.reflect/logs/`` — the prefix was doubly redundant).

    Never raises. Not user-visible — developer debugging only.
    """
    try:
        logs = logs_dir()
        logs.mkdir(parents=True, exist_ok=True)
        log_path = logs / f"{hook_name}.log"
        ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(f"[{ts}] {scrub_secrets(message)}\n")
    except Exception:
        pass
