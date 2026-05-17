#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyyaml"]
# ///
"""
Migrate legacy v2 reflect YAML state into the v3 SQLite database.

v2 stored:
  ~/.claude/session/reflect-state.yaml     (or ~/.reflect/reflect-state.yaml)
  ~/.claude/session/reflect-metrics.yaml   (or ~/.reflect/reflect-metrics.yaml)
  ~/.claude/session/learnings.yaml         (or ~/.reflect/learnings.yaml)
  ~/.claude/reflections/*.md               (free-form reflection notes)
  ~/.claude/reflections/by-agent/*/learnings.md
  ~/.claude/reflections/by-project/*/learnings.md

v3 stores everything in ~/.reflect/reflect.db via ``reflect_db.py``.

Safety model:
  * Dry-run by default — ``--execute`` to commit.
  * Idempotent — learning dedup via SHA-256 content_hash.
  * Originals are copied (never deleted) to ~/.reflect/migrations/v2-backup-{ts}/
  * Legacy .md reflections are recorded as ``events`` rather than learnings
    because their free-form shape makes them unreliable as structured rules.

CLI:
  python3 migrate_v2.py discover     # show what would be migrated
  python3 migrate_v2.py --dry-run    # default: shows plan
  python3 migrate_v2.py --execute    # actually migrate
  python3 migrate_v2.py --json       # machine-readable output
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Optional

# Ensure sibling imports work when run standalone.
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import reflect_db  # noqa: E402
from reflect_db import (  # noqa: E402
    LEGACY_V2_PATHS,
    _now_iso,
    compute_content_hash,
    db_path,
    get_events_by_type,
    get_known_content_hashes,
    has_legacy_state,
)

try:
    import yaml  # type: ignore
except ImportError as exc:  # pragma: no cover - import guard
    raise ImportError(
        "migrate_v2 requires pyyaml. Install with: pip install pyyaml"
    ) from exc


def _load_yaml(path: Path) -> Any:
    """Load a YAML file. Returns None if missing; raises on parse error."""
    if not path.is_file():
        return None
    return yaml.safe_load(path.read_text(encoding="utf-8"))


# ---------------------------------------------------------------------------
# Candidate paths
# ---------------------------------------------------------------------------

_HOME = Path.home()

STATE_CANDIDATES = [
    _HOME / ".claude" / "session" / "reflect-state.yaml",
    _HOME / ".reflect" / "reflect-state.yaml",
]
METRICS_CANDIDATES = [
    _HOME / ".claude" / "session" / "reflect-metrics.yaml",
    _HOME / ".reflect" / "reflect-metrics.yaml",
]
LEARNINGS_CANDIDATES = [
    _HOME / ".claude" / "session" / "learnings.yaml",
    _HOME / ".reflect" / "learnings.yaml",
]
REFLECTIONS_DIR = _HOME / ".claude" / "reflections"


# ---------------------------------------------------------------------------
# Status mapping from v2 -> v3
# ---------------------------------------------------------------------------

# v2 used 'applied' as a terminal-success state; v3 splits that into
# 'indexed' (written to the index) vs 'approved' (user-accepted). We map
# legacy 'applied' to 'indexed' because pre-v3 rows that reached 'applied'
# had already been written to agent files, which is the v3 meaning of indexed.
_STATUS_MAP = {
    "applied": "indexed",
    "indexed": "indexed",
    "pending": "pending",
    "approved": "approved",
    "rejected": "rejected",
    "reverted": "reverted",
}


def _map_status(raw: Optional[str]) -> str:
    if not raw:
        return "pending"
    return _STATUS_MAP.get(str(raw).strip().lower(), "pending")


def _map_confidence(raw: Any) -> str:
    """v2 uses low/medium/high, v3 uses LOW/MEDIUM/HIGH."""
    if raw is None:
        return "LOW"
    val = str(raw).strip().upper()
    if val in ("HIGH", "MEDIUM", "LOW"):
        return val
    return "LOW"


def _map_category(raw: Any) -> str:
    if raw is None:
        return "Unknown"
    text = str(raw).strip()
    return text or "Unknown"


# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------


@dataclass
class DiscoveredFile:
    kind: str               # "state" | "metrics" | "learnings" | "reflection"
    path: Path
    exists: bool
    size: int = 0
    parse_ok: bool = False
    parse_error: Optional[str] = None
    entry_count: int = 0    # learnings entries / metrics keys / pending reviews


@dataclass
class MigrationPlan:
    state_files: list[DiscoveredFile] = field(default_factory=list)
    metrics_files: list[DiscoveredFile] = field(default_factory=list)
    learnings_files: list[DiscoveredFile] = field(default_factory=list)
    reflection_files: list[DiscoveredFile] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        def ser(items: list[DiscoveredFile]) -> list[dict[str, Any]]:
            return [
                {
                    "path": str(f.path),
                    "exists": f.exists,
                    "size": f.size,
                    "parse_ok": f.parse_ok,
                    "parse_error": f.parse_error,
                    "entry_count": f.entry_count,
                }
                for f in items
            ]
        return {
            "state": ser(self.state_files),
            "metrics": ser(self.metrics_files),
            "learnings": ser(self.learnings_files),
            "reflections": ser(self.reflection_files),
        }


def _probe_yaml(path: Path, kind: str) -> DiscoveredFile:
    df = DiscoveredFile(kind=kind, path=path, exists=path.is_file())
    if not df.exists:
        return df
    try:
        df.size = path.stat().st_size
    except OSError:
        df.size = 0
    try:
        data = _load_yaml(path)
        df.parse_ok = True
    except Exception as exc:  # noqa: BLE001
        df.parse_error = f"{type(exc).__name__}: {exc}"
        return df

    if kind == "state" and isinstance(data, dict):
        df.entry_count = len(data.get("pending_low_confidence") or data.get("pending_reviews") or [])
    elif kind == "metrics" and isinstance(data, dict):
        df.entry_count = len(data)
    elif kind == "learnings" and isinstance(data, dict):
        entries = data.get("learnings") or data.get("entries") or []
        df.entry_count = len(entries) if isinstance(entries, list) else 0
    return df


def discover() -> MigrationPlan:
    plan = MigrationPlan()
    plan.state_files = [_probe_yaml(p, "state") for p in STATE_CANDIDATES]
    plan.metrics_files = [_probe_yaml(p, "metrics") for p in METRICS_CANDIDATES]
    plan.learnings_files = [_probe_yaml(p, "learnings") for p in LEARNINGS_CANDIDATES]

    refl: list[DiscoveredFile] = []
    if REFLECTIONS_DIR.is_dir():
        patterns = [
            REFLECTIONS_DIR.glob("*.md"),
            REFLECTIONS_DIR.glob("by-agent/*/learnings.md"),
            REFLECTIONS_DIR.glob("by-project/*/learnings.md"),
        ]
        for it in patterns:
            for p in it:
                if not p.is_file():
                    continue
                refl.append(
                    DiscoveredFile(
                        kind="reflection",
                        path=p,
                        exists=True,
                        size=p.stat().st_size,
                        parse_ok=True,
                    )
                )
    plan.reflection_files = refl
    return plan


# ---------------------------------------------------------------------------
# Backup
# ---------------------------------------------------------------------------


def _backup_dir() -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return db_path().parent / "migrations" / f"v2-backup-{stamp}"


def _backup_files(plan: MigrationPlan, dest: Path) -> list[Path]:
    """Copy every existing legacy file under *dest*, preserving relative paths."""
    written: list[Path] = []
    dest.mkdir(parents=True, exist_ok=True)
    all_files = (
        plan.state_files + plan.metrics_files
        + plan.learnings_files + plan.reflection_files
    )
    for f in all_files:
        if not f.exists:
            continue
        try:
            rel = f.path.relative_to(_HOME)
        except ValueError:
            rel = Path(f.path.name)
        target = dest / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(f.path, target)
        written.append(target)
    return written


# ---------------------------------------------------------------------------
# Import routines
# ---------------------------------------------------------------------------


@dataclass
class MigrationReport:
    backup_dir: Optional[Path] = None
    metrics_imported: int = 0
    state_keys_imported: int = 0
    pending_reviews_imported: int = 0
    learnings_imported: int = 0
    learnings_skipped_duplicate: int = 0
    learnings_failed: int = 0
    reflections_imported: int = 0
    reflections_failed: int = 0
    notes: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "backup_dir": str(self.backup_dir) if self.backup_dir else None,
            "metrics_imported": self.metrics_imported,
            "state_keys_imported": self.state_keys_imported,
            "pending_reviews_imported": self.pending_reviews_imported,
            "learnings_imported": self.learnings_imported,
            "learnings_skipped_duplicate": self.learnings_skipped_duplicate,
            "learnings_failed": self.learnings_failed,
            "reflections_imported": self.reflections_imported,
            "reflections_failed": self.reflections_failed,
            "notes": self.notes,
        }


def _first_existing(paths: list[Path]) -> Optional[Path]:
    for p in paths:
        if p.is_file():
            return p
    return None


def _load_first_mapping(
    kind: str,
    candidates: Iterable[Path],
    report: MigrationReport,
) -> Optional[tuple[Path, dict]]:
    """Return (path, parsed_dict) of the first existing, successfully parsed
    file, or None. Adds a skip/error note to *report* as appropriate so the
    three importers don't repeat the same boilerplate.
    """
    path = _first_existing(list(candidates))
    if path is None:
        report.notes.append(f"{kind}: no file found, skipping")
        return None
    try:
        data = _load_yaml(path)
    except Exception as exc:  # noqa: BLE001
        report.notes.append(f"{kind}: parse error at {path}: {exc}")
        return None
    if not isinstance(data, dict):
        report.notes.append(f"{kind}: {path} did not yield a mapping, skipping")
        return None
    return path, data


def _import_state(report: MigrationReport, execute: bool) -> None:
    loaded = _load_first_mapping("state", STATE_CANDIDATES, report)
    if loaded is None:
        return
    path, data = loaded

    conn = reflect_db.get_conn() if execute else None

    def _set(key: str, value: Any) -> None:
        if value is None:
            return
        if execute:
            reflect_db.set_metric(key, value, conn=conn)
        report.state_keys_imported += 1

    if "auto_reflect" in data:
        _set("auto_reflect", bool(data.get("auto_reflect")))
    if "last_reflection" in data:
        _set("last_reflection", data.get("last_reflection") or "")
    if "reflection_count" in data:
        _set("reflection_count", data.get("reflection_count"))
    if "decay_review_interval_days" in data:
        _set("decay_review_interval_days", data.get("decay_review_interval_days"))
    if "last_decay_review" in data:
        _set("last_decay_review", data.get("last_decay_review") or "")

    pending = data.get("pending_low_confidence") or data.get("pending_reviews") or []
    if isinstance(pending, list):
        seen = get_known_content_hashes(conn=conn) if execute else set()
        for item in pending:
            if not isinstance(item, dict):
                continue
            title = str(item.get("signal") or item.get("title") or "").strip()
            if not title:
                continue
            # Hash the raw v2 record, not mapped values — keeps re-runs
            # idempotent across future changes to the mapping functions.
            chash = compute_content_hash({
                "kind": "pending_review",
                "title": title,
                "raw_category": item.get("category"),
                "source_quote": item.get("source_quote", ""),
                "detected": item.get("detected", ""),
                "session_id": item.get("session_id", ""),
                "source_path": str(path),
            })
            if chash in seen:
                report.learnings_skipped_duplicate += 1
                continue
            if execute:
                lid = reflect_db.add_learning(
                    title=title,
                    category=_map_category(item.get("category")),
                    confidence="LOW",
                    source_tool="v2-migration",
                    source_path=str(path),
                    content_hash=chash,
                    conn=conn,
                )
                reflect_db.update_learning_status(lid, "pending", conn=conn)
                seen.add(chash)
            report.pending_reviews_imported += 1


def _import_metrics(report: MigrationReport, execute: bool) -> None:
    loaded = _load_first_mapping("metrics", METRICS_CANDIDATES, report)
    if loaded is None:
        return
    _, data = loaded

    conn = reflect_db.get_conn() if execute else None
    for key, value in data.items():
        if value is None:
            continue
        if execute:
            reflect_db.set_metric(str(key), value, conn=conn)
        report.metrics_imported += 1


def _import_learnings(report: MigrationReport, execute: bool) -> None:
    loaded = _load_first_mapping("learnings", LEARNINGS_CANDIDATES, report)
    if loaded is None:
        return
    path, data = loaded

    entries = data.get("learnings") or data.get("entries") or []
    if not isinstance(entries, list):
        report.notes.append(f"learnings: {path} has no 'learnings' or 'entries' list")
        return

    conn = reflect_db.get_conn() if execute else None
    seen = get_known_content_hashes(conn=conn) if execute else set()

    for entry in entries:
        if not isinstance(entry, dict):
            report.learnings_failed += 1
            continue

        title = str(entry.get("signal") or entry.get("title") or "").strip()
        if not title:
            report.learnings_failed += 1
            continue

        # Hash the unmapped source record so re-runs remain idempotent
        # even if _map_* mapping tables change in a future version.
        chash = compute_content_hash({
            "kind": "learning",
            "legacy_id": entry.get("id", ""),
            "title": title,
            "raw_category": entry.get("category"),
            "raw_confidence": entry.get("confidence"),
            "raw_status": entry.get("status"),
            "source_quote": entry.get("source_quote", ""),
            "target_file": entry.get("target_file") or entry.get("target") or "",
            "target_section": entry.get("target_section") or entry.get("section") or "",
            "timestamp": entry.get("timestamp", ""),
            "source_path": str(path),
        })

        if chash in seen:
            report.learnings_skipped_duplicate += 1
            continue

        desired_status = _map_status(entry.get("status"))
        commit_hash = entry.get("commit_hash") or None
        revert_reason = entry.get("revert_reason")

        try:
            if execute:
                lid = reflect_db.add_learning(
                    title=title,
                    category=_map_category(entry.get("category")),
                    confidence=_map_confidence(entry.get("confidence")),
                    source_tool="v2-migration",
                    source_path=str(path),
                    content_hash=chash,
                    conn=conn,
                )
                if desired_status != "pending":
                    reflect_db.update_learning_status(
                        lid,
                        desired_status,
                        revert_reason=revert_reason,
                        commit_hash=commit_hash,
                        conn=conn,
                    )
                reflect_db.add_event(
                    "v2_learning_imported",
                    lid,
                    {
                        "legacy_id": entry.get("id", ""),
                        "source_path": str(path),
                        "original_status": entry.get("status", ""),
                    },
                    conn=conn,
                )
                seen.add(chash)
            report.learnings_imported += 1
        except Exception as exc:  # noqa: BLE001
            report.learnings_failed += 1
            report.notes.append(f"learnings: failed to import {title!r}: {exc}")


def _import_reflections(report: MigrationReport, execute: bool) -> None:
    if not REFLECTIONS_DIR.is_dir():
        report.notes.append("reflections: directory not present, skipping")
        return
    patterns = [
        REFLECTIONS_DIR.glob("*.md"),
        REFLECTIONS_DIR.glob("by-agent/*/learnings.md"),
        REFLECTIONS_DIR.glob("by-project/*/learnings.md"),
    ]
    conn = reflect_db.get_conn() if execute else None

    already_imported: set[str] = set()
    if execute:
        for ev in get_events_by_type("legacy_reflection_imported", conn=conn):
            try:
                d = json.loads(ev.get("details_json") or "{}")
            except json.JSONDecodeError:
                continue
            p = d.get("path")
            if p:
                already_imported.add(p)

    for pattern in patterns:
        for p in pattern:
            if not p.is_file():
                continue
            key = str(p.resolve())
            if key in already_imported:
                report.notes.append(f"reflections: skipping already-imported {p}")
                continue
            try:
                # 1KB is plenty for the 200-char preview; avoids loading
                # potentially-large reflection files just to truncate them.
                with p.open("rb") as f:
                    raw = f.read(1024)
            except OSError as exc:
                report.reflections_failed += 1
                report.notes.append(f"reflections: unreadable {p}: {exc}")
                continue
            preview = raw.decode("utf-8", errors="replace")[:200].replace("\n", " ").strip()
            details = {
                "path": key,
                "size": p.stat().st_size,
                "preview": preview,
                "imported_at": _now_iso(),
            }
            if execute:
                reflect_db.add_event(
                    "legacy_reflection_imported",
                    None,
                    details,
                    conn=conn,
                )
                already_imported.add(key)
            report.reflections_imported += 1


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


def migrate(execute: bool) -> MigrationReport:
    report = MigrationReport()

    if not has_legacy_state():
        report.notes.append("no legacy v2 state found — nothing to migrate")
        return report

    plan = discover()

    if execute:
        reflect_db.init_db()
        dest = _backup_dir()
        _backup_files(plan, dest)
        report.backup_dir = dest

    _import_state(report, execute)
    _import_metrics(report, execute)
    _import_learnings(report, execute)
    _import_reflections(report, execute)

    if execute:
        reflect_db.add_event(
            "v2_migration_completed",
            None,
            report.to_dict(),
        )

    return report


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _print_plan_human(plan: MigrationPlan) -> None:
    print("=== Reflect v2 -> v3 Migration Plan ===\n")

    def _print_group(title: str, items: list[DiscoveredFile]) -> None:
        print(f"[{title}]")
        any_printed = False
        for f in items:
            if not f.exists:
                continue
            any_printed = True
            status = "OK" if f.parse_ok else f"ERR ({f.parse_error})"
            print(f"  {f.path}  ({f.size} bytes, {f.entry_count} entries, {status})")
        if not any_printed:
            print("  (none found)")
        print()

    _print_group("State", plan.state_files)
    _print_group("Metrics", plan.metrics_files)
    _print_group("Learnings", plan.learnings_files)
    _print_group("Reflections (imported as events)", plan.reflection_files)


def _print_report_human(report: MigrationReport, *, execute: bool) -> None:
    mode = "EXECUTED" if execute else "DRY RUN"
    print(f"=== Migration {mode} ===\n")
    if report.backup_dir:
        print(f"Backup:                      {report.backup_dir}")
    print(f"State keys imported:         {report.state_keys_imported}")
    print(f"Metrics imported:            {report.metrics_imported}")
    print(f"Pending reviews imported:    {report.pending_reviews_imported}")
    print(f"Learnings imported:          {report.learnings_imported}")
    print(f"Learnings skipped (dup):     {report.learnings_skipped_duplicate}")
    print(f"Learnings failed:            {report.learnings_failed}")
    print(f"Reflections -> events:       {report.reflections_imported}")
    print(f"Reflections failed:          {report.reflections_failed}")
    if report.notes:
        print("\nNotes:")
        for n in report.notes:
            print(f"  - {n}")


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="Migrate legacy v2 reflect YAML state into the v3 SQLite database.",
    )
    parser.add_argument(
        "subcommand",
        nargs="?",
        default=None,
        choices=["discover"],
        help="Optional subcommand. 'discover' shows a machine-readable inventory.",
    )
    mx = parser.add_mutually_exclusive_group()
    mx.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be imported without writing (default).",
    )
    mx.add_argument(
        "--execute",
        action="store_true",
        help="Commit changes to the SQLite database.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON instead of human prose.",
    )
    args = parser.parse_args(argv)

    if args.subcommand == "discover":
        plan = discover()
        if args.json:
            print(json.dumps(plan.to_dict(), indent=2, default=str))
        else:
            _print_plan_human(plan)
        return 0

    execute = bool(args.execute)

    if not execute:
        plan = discover()
        if args.json:
            report = migrate(execute=False)
            out = {
                "mode": "dry-run",
                "plan": plan.to_dict(),
                "report": report.to_dict(),
            }
            print(json.dumps(out, indent=2, default=str))
        else:
            _print_plan_human(plan)
            report = migrate(execute=False)
            _print_report_human(report, execute=False)
            print("\nRe-run with --execute to commit.")
        return 0

    report = migrate(execute=True)
    if args.json:
        print(json.dumps({"mode": "execute", "report": report.to_dict()},
                         indent=2, default=str))
    else:
        _print_report_human(report, execute=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
