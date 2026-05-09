#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# ///
"""
Stub v2 telemetry artifacts so future investigators (LLM or human) cannot
mistake them for live state.

In v3 the source of truth moved to ``~/.reflect/reflect.db``. The legacy
yaml files (``reflect-state.yaml``, ``reflect-metrics.yaml``) and the
``episodes/`` directory are no longer written, but they sit on disk
looking authoritative — `cat`-ing them returns plausible-but-stale data
with no marker that they're dead. That's a foot-gun. This script does
two things:

  1. Drops a self-describing ``~/.reflect/README.md`` that lists what's
     authoritative vs. vestigial.
  2. Replaces each dead file with a one-line deprecation stub. Originals
     are backed up to ``~/.reflect/migrations/v2-stub-{ts}/`` first.

Idempotent: safe to re-run. If a file already starts with the deprecation
marker, it is left alone.

Usage:
    uv run scripts/stub_v2_telemetry.py            # dry run
    uv run scripts/stub_v2_telemetry.py --execute  # apply
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from datetime import datetime
from pathlib import Path

DEPRECATION_MARKER = "# DEPRECATED — telemetry moved to ~/.reflect/reflect.db (v3+)"

README = """# ~/.reflect/

reflect plugin v3+ runtime state directory.

## Authoritative

- `reflect.db`               — sqlite. **Single source of truth** for
                                metrics, learnings, proposals, events,
                                sources. Read by `reflect-status`.
- `pending_reflections.jsonl` — live queue populated by the PreCompact
                                hook, drained by `reflect-drain-bg.sh`.
- `drain.log`                 — bg drainer activity log (human-readable).
- `recall_log.jsonl`          — recall query log.
- `drain-cost.jsonl`          — per-entry drain cost telemetry.
- `recall_cache/`             — recall result cache.

## Vestigial (DEPRECATED — do not read)

- `reflect-state.yaml`        — v2 leftover; replaced by `metrics` table.
- `reflect-metrics.yaml`      — v2 leftover; replaced by `metrics` table.
- `episodes/`                 — v2 leftover; not written by v3.

If you're investigating reflect state, **start with reflect.db**:

    sqlite3 ~/.reflect/reflect.db "SELECT * FROM metrics"
    uv run ~/.claude/skills/reflect/scripts/state_manager.py status

If you find yourself `cat`-ing a yaml file in this directory and using
its contents to draw conclusions, stop. Confirm the current code reads
it (`grep -rn "filename"` against the toolkit) before trusting it.
"""

STUB_TEMPLATE = """{marker}
# Original v2 file backed up to: {backup}
# Current source of truth:       ~/.reflect/reflect.db (table: metrics)
# Read with:                     uv run state_manager.py status
"""


def get_state_dir() -> Path:
    custom = os.environ.get("REFLECT_STATE_DIR")
    return Path(custom).expanduser() if custom else Path.home() / ".reflect"


def already_stubbed(path: Path) -> bool:
    """Cheap idempotency check: a file we already stubbed begins with the marker."""
    if not path.is_file():
        return False
    try:
        with path.open("r") as f:
            first = f.readline().strip()
        return first == DEPRECATION_MARKER
    except OSError:
        return False


def stub_file(path: Path, backup_dir: Path, dry_run: bool) -> str:
    if not path.exists():
        return f"skip (missing): {path}"
    if already_stubbed(path):
        return f"skip (already stubbed): {path}"

    if dry_run:
        return f"would stub: {path} (backup -> {backup_dir / path.name})"

    backup_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, backup_dir / path.name)
    path.write_text(STUB_TEMPLATE.format(marker=DEPRECATION_MARKER, backup=backup_dir / path.name))
    return f"stubbed: {path} -> backup {backup_dir / path.name}"


def stub_episodes_dir(state_dir: Path, backup_dir: Path, dry_run: bool) -> str:
    """Drop a README inside episodes/ rather than nuking the directory.

    Some investigators will `ls` the dir before reading individual files;
    a README at the top makes the dead-state status visible immediately.
    Existing .md files are left in place — they may still be useful for
    historical context.
    """
    ep_dir = state_dir / "episodes"
    if not ep_dir.is_dir():
        return f"skip (no episodes dir): {ep_dir}"

    readme = ep_dir / "README.md"
    if readme.is_file() and already_stubbed(readme):
        return f"skip (episodes/README.md already stubbed): {readme}"

    content = (
        f"{DEPRECATION_MARKER}\n"
        f"#\n"
        f"# v3+ does not write to ~/.reflect/episodes/.\n"
        f"# Existing files are historical artifacts from v2.\n"
        f"# Episode-shaped data is now stored as rows in reflect.db (events table).\n"
    )

    if dry_run:
        return f"would write: {readme}"

    readme.write_text(content)
    return f"wrote: {readme}"


def write_readme(state_dir: Path, dry_run: bool) -> str:
    readme = state_dir / "README.md"
    if dry_run:
        return f"would write: {readme}"
    state_dir.mkdir(parents=True, exist_ok=True)
    readme.write_text(README)
    return f"wrote: {readme}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--execute",
        action="store_true",
        help="Apply changes. Without this flag, only print what would happen.",
    )
    args = parser.parse_args()
    dry_run = not args.execute

    state_dir = get_state_dir()
    if not state_dir.exists():
        print(f"reflect state dir not found: {state_dir}", file=sys.stderr)
        return 1

    ts = datetime.now().strftime("%Y%m%dT%H%M%S")
    backup_dir = state_dir / "migrations" / f"v2-stub-{ts}"

    actions = [
        write_readme(state_dir, dry_run),
        stub_file(state_dir / "reflect-state.yaml", backup_dir, dry_run),
        stub_file(state_dir / "reflect-metrics.yaml", backup_dir, dry_run),
        stub_episodes_dir(state_dir, backup_dir, dry_run),
    ]

    mode = "DRY RUN" if dry_run else "APPLIED"
    print(f"=== {mode} ===")
    for line in actions:
        print(f"  {line}")
    if dry_run:
        print("\nRe-run with --execute to apply.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
