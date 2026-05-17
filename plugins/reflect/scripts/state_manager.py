#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
State Manager for Reflect Skill — SQLite-backed thin wrapper.

Preserves the original CLI interface (init / status / on / off / pending)
and function signatures for backwards compatibility while delegating all
persistence to ``reflect_db.py``.
"""

import sys
from datetime import datetime
from pathlib import Path
from typing import Any

# Ensure sibling imports work when run standalone
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from reflect_config import get_config, resolve_path
from reflect_db import (
    init_db,
    get_metric,
    get_metrics,
    set_metric,
    increment_metric,
    add_learning as db_add_learning,
    get_pending_learnings as db_get_pending,
    update_learning_status,
    add_event,
)

# ---------------------------------------------------------------------------
# State directory (for display / legacy compatibility)
# ---------------------------------------------------------------------------


def get_state_dir() -> Path:
    """Return the state directory (now derived from config db_path)."""
    cfg = get_config()
    db_raw = cfg.get("storage", {}).get("db_path", "~/.reflect/reflect.db")
    return resolve_path(db_raw).parent


def ensure_state_dir() -> Path:
    """Create state directory if it doesn't exist."""
    state_dir = get_state_dir()
    state_dir.mkdir(parents=True, exist_ok=True)
    return state_dir


# Legacy path helpers (no longer produce YAML — kept for anything that
# checks their existence)


def get_state_file() -> Path:
    return get_state_dir() / "reflect-state.yaml"


def get_metrics_file() -> Path:
    return get_state_dir() / "reflect-metrics.yaml"


def get_learnings_file() -> Path:
    return get_state_dir() / "learnings.yaml"


# ---------------------------------------------------------------------------
# State operations — now backed by SQLite metrics table
# ---------------------------------------------------------------------------


def init_state() -> dict:
    """Initialize or load state from the database."""
    conn = init_db()

    # Seed defaults if not yet present
    if get_metric("auto_reflect", conn=conn) is None:
        set_metric("auto_reflect", False, conn=conn)
    if get_metric("last_reflection", conn=conn) is None:
        set_metric("last_reflection", "", conn=conn)

    state = {
        "auto_reflect": get_metric("auto_reflect", False, conn=conn),
        "last_reflection": get_metric("last_reflection", None, conn=conn) or None,
        "pending_low_confidence": [
            _learning_to_pending_dict(l) for l in db_get_pending(conn=conn)
        ],
    }

    db_path = resolve_path(
        get_config().get("storage", {}).get("db_path", "~/.reflect/reflect.db")
    )
    print(f"State backed by {db_path}")
    return state


def get_state() -> dict:
    """Get current state without modifying."""
    conn = init_db()
    return {
        "auto_reflect": get_metric("auto_reflect", False, conn=conn),
        "last_reflection": get_metric("last_reflection", None, conn=conn) or None,
        "pending_low_confidence": [
            _learning_to_pending_dict(l) for l in db_get_pending(conn=conn)
        ],
    }


def set_auto_reflect(enabled: bool) -> None:
    """Enable or disable auto-reflection."""
    conn = init_db()
    set_metric("auto_reflect", enabled, conn=conn)
    status = "enabled" if enabled else "disabled"
    print(f"Auto-reflection {status}")


def update_last_reflection() -> None:
    """Update last_reflection timestamp."""
    conn = init_db()
    set_metric("last_reflection", datetime.now().isoformat(), conn=conn)


def add_pending_low_confidence(signal: dict) -> None:
    """Add signal to pending review queue (stored as a pending learning)."""
    conn = init_db()
    db_add_learning(
        title=signal.get("signal", ""),
        category=signal.get("category", "Unknown"),
        confidence="LOW",
        source_tool=signal.get("source_tool", ""),
        source_path=signal.get("source_path", ""),
        conn=conn,
    )


def get_pending_reviews() -> list[dict]:
    """Get all pending low-confidence learnings."""
    conn = init_db()
    return [_learning_to_pending_dict(l) for l in db_get_pending(conn=conn)]


def clear_pending_review(index: int) -> bool:
    """Remove a pending review by index (reject it)."""
    conn = init_db()
    pending = db_get_pending(conn=conn)
    if 0 <= index < len(pending):
        update_learning_status(pending[index]["id"], "rejected", conn=conn)
        return True
    return False


def add_learning(learning: dict) -> None:
    """Add a learning to the learnings log."""
    conn = init_db()
    db_add_learning(
        title=learning.get("signal", ""),
        category=learning.get("category", "Unknown"),
        confidence=learning.get("confidence", "LOW"),
        source_tool=learning.get("source", ""),
        source_path=learning.get("target", ""),
        conn=conn,
    )


def show_status() -> None:
    """Print current state and metrics."""
    conn = init_db()
    state = get_state()
    m = get_metrics(conn=conn)

    print("\n=== Reflect Status ===\n")
    print(f"State Directory: {get_state_dir()}")
    print(f"Auto-Reflect: {'Enabled' if state.get('auto_reflect') else 'Disabled'}")
    print(f"Last Reflection: {state.get('last_reflection', 'Never') or 'Never'}")

    pending = state.get("pending_low_confidence", [])
    print(f"Pending Reviews: {len(pending)}")

    if m:
        print(f"\n=== Metrics ===\n")
        print(f"Total Sessions: {m.get('total_sessions_analyzed', 0)}")
        print(f"Signals Detected: {m.get('total_signals_detected', 0)}")
        print(f"Changes Proposed: {m.get('total_changes_proposed', 0)}")
        print(f"Changes Accepted: {m.get('total_changes_accepted', 0)}")
        print(f"Acceptance Rate: {m.get('acceptance_rate', 0)}%")


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _learning_to_pending_dict(learning: dict) -> dict:
    """Convert a DB learning row to the legacy pending-review dict shape."""
    return {
        "signal": learning.get("title", ""),
        "detected": learning.get("created_at", ""),
        "awaiting_validation": True,
        "source_quote": "",
        "category": learning.get("category", "Unknown"),
        "id": learning.get("id", ""),
    }


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------


def main():
    import argparse

    parser = argparse.ArgumentParser(description='Reflect State Manager')
    parser.add_argument('command', choices=['init', 'status', 'on', 'off', 'pending'],
                       help='Command to execute')

    args = parser.parse_args()

    if args.command == 'init':
        init_state()
    elif args.command == 'status':
        show_status()
    elif args.command == 'on':
        set_auto_reflect(True)
    elif args.command == 'off':
        set_auto_reflect(False)
    elif args.command == 'pending':
        pending = get_pending_reviews()
        if not pending:
            print("No pending low-confidence learnings.")
        else:
            print(f"\n=== Pending Reviews ({len(pending)}) ===\n")
            for i, item in enumerate(pending):
                print(f"{i+1}. {item.get('signal')}")
                print(f"   Detected: {item.get('detected')}")
                print(f"   Quote: \"{item.get('source_quote', 'N/A')}\"")
                print()


if __name__ == '__main__':
    main()
