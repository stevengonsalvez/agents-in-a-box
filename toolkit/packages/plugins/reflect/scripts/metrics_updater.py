#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Metrics Updater for Reflect Skill — SQLite edition.

Tracks and aggregates reflection metrics including:
- Sessions analyzed
- Signals detected by confidence level
- Changes proposed vs accepted
- Most frequently updated agents

Backed by the ``reflect_db`` SQLite store.  The CLI interface is unchanged
from the YAML-based version for backwards compatibility.

Usage:
    python metrics_updater.py --accepted 3 --rejected 1 --confidence high:2,medium:1
    python metrics_updater.py --show
    python metrics_updater.py --reset
    python metrics_updater.py --action record --accepted 2
    python metrics_updater.py --action get --key total_sessions_analyzed
    python metrics_updater.py --action summary
"""

import json
import sys
from pathlib import Path
from typing import Optional

# Ensure sibling imports work when run standalone
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from reflect_db import (
    init_db,
    get_metric,
    get_metrics,
    increment_metric,
    set_metric,
)

# ---------------------------------------------------------------------------
# Metric keys (mirror the old YAML structure)
# ---------------------------------------------------------------------------

_METRIC_KEYS = [
    "last_reflection",
    "total_sessions_analyzed",
    "total_signals_detected",
    "total_changes_proposed",
    "total_changes_accepted",
    "acceptance_rate",
    "confidence_high",
    "confidence_medium",
    "confidence_low",
    "skills_created",
    "knowledge_notes_created",
    "sidecars_generated",
    "estimated_time_saved",
    "most_updated_agents",
]


def _ensure_defaults() -> None:
    """Seed default metric rows if the database is fresh."""
    conn = init_db()
    for key in _METRIC_KEYS:
        if get_metric(key, conn=conn) is None:
            default = {} if key == "most_updated_agents" else 0
            if key == "last_reflection":
                default = ""
            elif key == "estimated_time_saved":
                default = "~0 hours"
            set_metric(key, default, conn=conn)


# ---------------------------------------------------------------------------
# Core update logic
# ---------------------------------------------------------------------------


def update_metrics(
    accepted: int = 0,
    rejected: int = 0,
    high: int = 0,
    medium: int = 0,
    low: int = 0,
    agents: Optional[list[str]] = None,
    skills: int = 0,
    knowledge_notes: int = 0,
    sidecars: int = 0,
) -> dict:
    """
    Update metrics with new reflection results.

    Returns a dict of all current metric values.
    """
    from datetime import datetime

    conn = init_db()
    _ensure_defaults()

    # Timestamp
    set_metric("last_reflection", datetime.now().isoformat(), conn=conn)

    # Session
    increment_metric("total_sessions_analyzed", 1, conn=conn)

    # Signals
    total_signals = high + medium + low
    if total_signals > 0:
        increment_metric("total_signals_detected", total_signals, conn=conn)

    # Confidence breakdown
    if high:
        increment_metric("confidence_high", high, conn=conn)
    if medium:
        increment_metric("confidence_medium", medium, conn=conn)
    if low:
        increment_metric("confidence_low", low, conn=conn)

    # Changes
    proposed = accepted + rejected
    if proposed:
        increment_metric("total_changes_proposed", proposed, conn=conn)
    if accepted:
        increment_metric("total_changes_accepted", accepted, conn=conn)

    # Acceptance rate
    total_proposed = get_metric("total_changes_proposed", 0, conn=conn)
    total_accepted = get_metric("total_changes_accepted", 0, conn=conn)
    rate = round((total_accepted / total_proposed) * 100) if total_proposed else 0
    set_metric("acceptance_rate", rate, conn=conn)

    # Agent update counts
    if agents:
        agent_counts = get_metric("most_updated_agents", {}, conn=conn)
        if not isinstance(agent_counts, dict):
            agent_counts = {}
        for agent in agents:
            agent_counts[agent] = agent_counts.get(agent, 0) + 1
        # Keep top 10
        sorted_agents = dict(sorted(agent_counts.items(), key=lambda x: -x[1])[:10])
        set_metric("most_updated_agents", sorted_agents, conn=conn)

    # Artefact counts
    if skills:
        increment_metric("skills_created", skills, conn=conn)
    if knowledge_notes:
        increment_metric("knowledge_notes_created", knowledge_notes, conn=conn)
    if sidecars:
        increment_metric("sidecars_generated", sidecars, conn=conn)

    # Estimated time saved (5 min per accepted learning)
    all_accepted = get_metric("total_changes_accepted", 0, conn=conn)
    hours_saved = round(int(all_accepted) * 5 / 60, 1)
    set_metric("estimated_time_saved", f"~{hours_saved} hours", conn=conn)

    return get_metrics(conn=conn)


# ---------------------------------------------------------------------------
# Display
# ---------------------------------------------------------------------------


def show_metrics() -> None:
    """Display current metrics."""
    conn = init_db()
    _ensure_defaults()
    m = get_metrics(conn=conn)

    print("\n=== Reflect Metrics ===\n")
    print(f"Last Reflection: {m.get('last_reflection', 'Never') or 'Never'}")
    print(f"Sessions Analyzed: {m.get('total_sessions_analyzed', 0)}")
    print(f"Total Signals: {m.get('total_signals_detected', 0)}")
    print(f"Changes Proposed: {m.get('total_changes_proposed', 0)}")
    print(f"Changes Accepted: {m.get('total_changes_accepted', 0)}")
    print(f"Acceptance Rate: {m.get('acceptance_rate', 0)}%")
    print(f"Skills Created: {m.get('skills_created', 0)}")
    print(f"Knowledge Notes: {m.get('knowledge_notes_created', 0)}")
    print(f"Sidecars Generated: {m.get('sidecars_generated', 0)}")
    print(f"Estimated Time Saved: {m.get('estimated_time_saved', '~0 hours')}")

    print(f"\nConfidence Breakdown:")
    print(f"  High: {m.get('confidence_high', 0)}")
    print(f"  Medium: {m.get('confidence_medium', 0)}")
    print(f"  Low: {m.get('confidence_low', 0)}")

    agents = m.get("most_updated_agents", {})
    if isinstance(agents, dict) and agents:
        print(f"\nMost Updated Agents:")
        for agent, count in list(agents.items())[:5]:
            print(f"  {agent}: {count}")


def reset_metrics() -> None:
    """Reset all metrics to defaults."""
    conn = init_db()
    for key in _METRIC_KEYS:
        default = {} if key == "most_updated_agents" else 0
        if key == "last_reflection":
            default = ""
        elif key == "estimated_time_saved":
            default = "~0 hours"
        set_metric(key, default, conn=conn)
    print("Metrics reset to defaults.")


# ---------------------------------------------------------------------------
# Confidence string parser
# ---------------------------------------------------------------------------


def parse_confidence(conf_str: str) -> tuple[int, int, int]:
    """Parse confidence string like 'high:2,medium:1,low:3'."""
    high = medium = low = 0
    for part in conf_str.split(","):
        if ":" in part:
            level, count = part.split(":")
            count_int = int(count)
            level_lower = level.strip().lower()
            if level_lower == "high":
                high = count_int
            elif level_lower == "medium":
                medium = count_int
            elif level_lower == "low":
                low = count_int
    return high, medium, low


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Update reflection metrics")

    # Legacy positional --action interface
    parser.add_argument("--action", type=str, choices=["record", "get", "summary"],
                        help="Action mode (record / get / summary)")
    parser.add_argument("--key", type=str, help="Metric key for --action get")

    # Direct flags
    parser.add_argument("--accepted", type=int, default=0,
                        help="Number of accepted changes")
    parser.add_argument("--rejected", type=int, default=0,
                        help="Number of rejected changes")
    parser.add_argument("--confidence", type=str,
                        help="Confidence breakdown (e.g., high:2,medium:1)")
    parser.add_argument("--agents", type=str,
                        help="Comma-separated list of updated agents")
    parser.add_argument("--skills", type=int, default=0,
                        help="Number of skills created")
    parser.add_argument("--knowledge-notes", type=int, default=0,
                        help="Number of knowledge notes created")
    parser.add_argument("--sidecars", type=int, default=0,
                        help="Number of sidecars generated")
    parser.add_argument("--show", action="store_true", help="Show current metrics")
    parser.add_argument("--reset", action="store_true", help="Reset all metrics")
    parser.add_argument("--json", action="store_true", help="Output as JSON")

    args = parser.parse_args()

    # Handle --action interface for backwards compat
    if args.action == "summary" or args.show:
        if args.json:
            conn = init_db()
            _ensure_defaults()
            print(json.dumps(get_metrics(conn=conn), indent=2, default=str))
        else:
            show_metrics()
        return

    if args.action == "get":
        if not args.key:
            print("ERROR: --key required with --action get", file=sys.stderr)
            sys.exit(1)
        conn = init_db()
        _ensure_defaults()
        val = get_metric(args.key, conn=conn)
        if args.json:
            print(json.dumps({"key": args.key, "value": val}, default=str))
        else:
            print(f"{args.key}: {val}")
        return

    if args.reset:
        reset_metrics()
        return

    # Default: record mode
    high = medium = low = 0
    if args.confidence:
        high, medium, low = parse_confidence(args.confidence)

    agents = None
    if args.agents:
        agents = [a.strip() for a in args.agents.split(",")]

    metrics = update_metrics(
        accepted=args.accepted,
        rejected=args.rejected,
        high=high,
        medium=medium,
        low=low,
        agents=agents,
        skills=args.skills,
        knowledge_notes=args.knowledge_notes,
        sidecars=args.sidecars,
    )

    if args.json:
        print(json.dumps(metrics, indent=2, default=str))
    else:
        print(f"Metrics updated. Acceptance rate: {metrics.get('acceptance_rate', 0)}%")


if __name__ == "__main__":
    main()
