"""Best-effort JSONL metrics writer for reflect-kb CLI operations.

Every CLI op may call ``write_metric(op, **fields)`` to append a line to
``~/.learnings/metrics.jsonl``. The file is rotated to
``metrics-<ts>.jsonl.bak`` once it passes 10 MB. Writes never raise —
observability must not break the CLI.
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path

METRICS_PATH = Path.home() / ".learnings" / "metrics.jsonl"
MAX_BYTES = 10 * 1024 * 1024  # 10 MB


def _detect_harness() -> str:
    """Best-effort harness detection from env vars."""
    if os.environ.get("CLAUDECODE"):
        return "claude"
    if os.environ.get("CODEX_CLI"):
        return "codex"
    if os.environ.get("GITHUB_COPILOT"):
        return "copilot"
    return "other"


def _rotate_if_needed(path: Path) -> None:
    try:
        if path.exists() and path.stat().st_size > MAX_BYTES:
            ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
            path.rename(path.with_name(f"metrics-{ts}.jsonl.bak"))
    except OSError:
        pass


def write_metric(op: str, **fields) -> None:
    """Append a metric event to the JSONL log. Best-effort, never raises."""
    try:
        METRICS_PATH.parent.mkdir(parents=True, exist_ok=True)
        _rotate_if_needed(METRICS_PATH)
        record = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "op": op,
            "harness": _detect_harness(),
            **fields,
        }
        with METRICS_PATH.open("a", encoding="utf-8") as f:
            f.write(json.dumps(record, default=str) + "\n")
    except Exception:
        # Observability should never break the CLI.
        pass
