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
import hashlib
import json
import shutil
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

# Ensure sibling imports work when run standalone.
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import reflect_db  # noqa: E402
from reflect_config import resolve_path  # noqa: E402

# ---------------------------------------------------------------------------
# YAML loader — prefers pyyaml, falls back to a conservative parser for the
# well-known v2 shapes only.
# ---------------------------------------------------------------------------

try:
    import yaml as _yaml  # type: ignore
    _HAVE_YAML = True
except ImportError:  # pragma: no cover - import guard
    _yaml = None
    _HAVE_YAML = False


def _load_yaml(path: Path) -> Any:
    """Load a YAML file. Returns None if missing; raises on parse error.

    When pyyaml is unavailable we fall back to a minimal parser that handles
    the known v2 structures (top-level scalars/lists/maps, no anchors, no
    multi-document streams). Anything more exotic triggers a loud failure
    with install instructions — better to stop than silently drop data.
    """
    if not path.is_file():
        return None
    text = path.read_text(encoding="utf-8")
    if _HAVE_YAML:
        return _yaml.safe_load(text)
    return _naive_yaml_parse(text, source=str(path))


def _naive_yaml_parse(text: str, *, source: str) -> Any:
    """
    Very small YAML subset parser for v2 shapes.

    Handles:
      key: value        (scalars: int, float, bool, null, quoted/unquoted str)
      key:              (followed by a nested block or list)
      - item            (list of scalars or maps)
      # comment         (ignored)

    Does NOT handle: anchors (&, *), multi-doc (---), flow style ({}, []),
    block scalars (|, >), tags (!!).  If anything unexpected is hit we raise.
    """

    def _raise(msg: str, lineno: int) -> None:
        raise RuntimeError(
            f"Cannot parse v2 YAML at {source}:{lineno} without pyyaml installed.\n"
            f"  Reason: {msg}\n"
            f"  Install pyyaml: pip install pyyaml   (or: uv pip install pyyaml)"
        )

    # Strip comments and trailing whitespace; keep blank lines for indent context.
    raw_lines = text.splitlines()
    lines: list[tuple[int, str]] = []
    for i, ln in enumerate(raw_lines, start=1):
        # Drop comment portion (YAML allows # anywhere, but v2 files only use
        # full-line comments — we do the same here).
        stripped_for_comment = ln.lstrip()
        if stripped_for_comment.startswith("#"):
            continue
        # Inline comments after a space + #.
        if " #" in ln:
            ln = ln.split(" #", 1)[0]
        if not ln.strip():
            continue
        if ln.lstrip().startswith(("&", "*", "!", "---", "...")):
            _raise(f"unsupported YAML feature in line {ln!r}", i)
        lines.append((i, ln.rstrip()))

    def _indent(s: str) -> int:
        return len(s) - len(s.lstrip(" "))

    def _coerce(val: str) -> Any:
        v = val.strip()
        if not v:
            return ""
        # Quoted string
        if (v.startswith('"') and v.endswith('"')) or (
            v.startswith("'") and v.endswith("'")
        ):
            return v[1:-1]
        # Empty flow-style collections — common in v2 files ("pending_reviews: []").
        if v == "[]":
            return []
        if v == "{}":
            return {}
        low = v.lower()
        if low in ("null", "~", ""):
            return None
        if low in ("true", "yes"):
            return True
        if low in ("false", "no"):
            return False
        # int / float
        try:
            if "." in v or "e" in low:
                return float(v)
            return int(v)
        except ValueError:
            pass
        return v  # plain string

    # Recursive descent parser.
    def _parse_block(start: int, base_indent: int) -> tuple[Any, int]:
        """Parse lines[start:] at *base_indent*. Returns (value, next_idx)."""
        if start >= len(lines):
            return None, start

        lineno, first = lines[start]
        ind = _indent(first)
        if ind < base_indent:
            return None, start

        # List?
        if first.lstrip().startswith("- "):
            return _parse_list(start, ind)
        # Map with "key:" at this indent.
        if ":" in first.lstrip():
            return _parse_map(start, ind)
        _raise(f"unexpected line {first!r}", lineno)
        return None, start  # unreachable

    def _parse_map(start: int, base_indent: int) -> tuple[dict, int]:
        out: dict[str, Any] = {}
        i = start
        while i < len(lines):
            lineno, ln = lines[i]
            ind = _indent(ln)
            if ind < base_indent:
                break
            if ind > base_indent:
                _raise(f"unexpected indentation at {ln!r}", lineno)
            body = ln.strip()
            if body.startswith("- "):
                break  # list sibling — caller decides
            if ":" not in body:
                _raise(f"expected 'key: value' got {body!r}", lineno)
            key, _, rest = body.partition(":")
            key = key.strip()
            rest = rest.strip()
            if rest == "":
                # Nested value on following lines (or empty).
                if i + 1 < len(lines) and _indent(lines[i + 1][1]) > base_indent:
                    child_indent = _indent(lines[i + 1][1])
                    value, i = _parse_block(i + 1, child_indent)
                    out[key] = value
                else:
                    out[key] = None
                    i += 1
            else:
                out[key] = _coerce(rest)
                i += 1
        return out, i

    def _parse_list(start: int, base_indent: int) -> tuple[list, int]:
        out: list[Any] = []
        i = start
        while i < len(lines):
            lineno, ln = lines[i]
            ind = _indent(ln)
            if ind < base_indent:
                break
            body = ln.strip()
            if not body.startswith("- "):
                break
            item_text = body[2:].strip()
            if item_text == "":
                # Nested block item.
                if i + 1 < len(lines) and _indent(lines[i + 1][1]) > base_indent:
                    child_indent = _indent(lines[i + 1][1])
                    value, i = _parse_block(i + 1, child_indent)
                    out.append(value)
                else:
                    out.append(None)
                    i += 1
            elif ":" in item_text:
                # Inline "- key: value" starts a map on the same line.
                # Treat the list item as a map whose first key is item_text
                # and whose other keys continue at base_indent + 2.
                first_key, _, first_rest = item_text.partition(":")
                first_key = first_key.strip()
                first_rest = first_rest.strip()
                item_map: dict[str, Any] = {}
                if first_rest:
                    item_map[first_key] = _coerce(first_rest)
                else:
                    # Nested block value for the first key.
                    if i + 1 < len(lines) and _indent(lines[i + 1][1]) > base_indent + 2:
                        child_indent = _indent(lines[i + 1][1])
                        value, i2 = _parse_block(i + 1, child_indent)
                        item_map[first_key] = value
                        i = i2 - 1  # will be incremented below
                    else:
                        item_map[first_key] = None
                i += 1
                # Continue reading sibling keys for this map item.
                while i < len(lines):
                    lineno2, ln2 = lines[i]
                    ind2 = _indent(ln2)
                    if ind2 <= base_indent:
                        break
                    body2 = ln2.strip()
                    if body2.startswith("- "):
                        break
                    if ":" not in body2:
                        _raise(f"expected 'key: value' got {body2!r}", lineno2)
                    k2, _, v2 = body2.partition(":")
                    k2 = k2.strip()
                    v2 = v2.strip()
                    if v2 == "":
                        if i + 1 < len(lines) and _indent(lines[i + 1][1]) > ind2:
                            child_indent = _indent(lines[i + 1][1])
                            value, i = _parse_block(i + 1, child_indent)
                            item_map[k2] = value
                        else:
                            item_map[k2] = None
                            i += 1
                    else:
                        item_map[k2] = _coerce(v2)
                        i += 1
                out.append(item_map)
            else:
                out.append(_coerce(item_text))
                i += 1
        return out, i

    if not lines:
        return None
    root_indent = _indent(lines[0][1])
    value, _ = _parse_block(0, root_indent)
    return value


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


def _content_hash(payload: dict[str, Any]) -> str:
    """Stable SHA-256 over the canonical JSON of the payload."""
    blob = json.dumps(payload, sort_keys=True, default=str).encode("utf-8")
    return hashlib.sha256(blob).hexdigest()


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


def has_any_legacy_state(plan: Optional[MigrationPlan] = None) -> bool:
    plan = plan or discover()
    return any(
        f.exists for f in (
            plan.state_files + plan.metrics_files
            + plan.learnings_files + plan.reflection_files
        )
    )


# ---------------------------------------------------------------------------
# Backup
# ---------------------------------------------------------------------------


def _backup_dir() -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    cfg_root = resolve_path("~/.reflect")
    return cfg_root / "migrations" / f"v2-backup-{stamp}"


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


def _existing_learning_hashes(conn) -> set[str]:
    rows = conn.execute(
        "SELECT content_hash FROM learnings WHERE content_hash != ''"
    ).fetchall()
    return {r["content_hash"] for r in rows}


def _import_state(report: MigrationReport, execute: bool) -> None:
    path = _first_existing(STATE_CANDIDATES)
    if path is None:
        report.notes.append("state: no file found, skipping")
        return
    try:
        data = _load_yaml(path)
    except Exception as exc:  # noqa: BLE001
        report.notes.append(f"state: parse error at {path}: {exc}")
        return
    if not isinstance(data, dict):
        report.notes.append(f"state: {path} did not yield a mapping, skipping")
        return

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

    # Pending reviews are stored as pending learnings in v3.
    pending = data.get("pending_low_confidence") or data.get("pending_reviews") or []
    if isinstance(pending, list):
        seen = _existing_learning_hashes(conn) if execute else set()
        for item in pending:
            if not isinstance(item, dict):
                continue
            title = str(item.get("signal") or item.get("title") or "").strip()
            if not title:
                continue
            payload = {
                "title": title,
                "category": _map_category(item.get("category")),
                "source_quote": item.get("source_quote", ""),
                "detected": item.get("detected", ""),
                "session_id": item.get("session_id", ""),
            }
            chash = _content_hash(payload)
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
    path = _first_existing(METRICS_CANDIDATES)
    if path is None:
        report.notes.append("metrics: no file found, skipping")
        return
    try:
        data = _load_yaml(path)
    except Exception as exc:  # noqa: BLE001
        report.notes.append(f"metrics: parse error at {path}: {exc}")
        return
    if not isinstance(data, dict):
        report.notes.append(f"metrics: {path} did not yield a mapping, skipping")
        return

    conn = reflect_db.get_conn() if execute else None
    for key, value in data.items():
        if value is None:
            continue
        if execute:
            reflect_db.set_metric(str(key), value, conn=conn)
        report.metrics_imported += 1


def _import_learnings(report: MigrationReport, execute: bool) -> None:
    path = _first_existing(LEARNINGS_CANDIDATES)
    if path is None:
        report.notes.append("learnings: no file found, skipping")
        return
    try:
        data = _load_yaml(path)
    except Exception as exc:  # noqa: BLE001
        report.notes.append(f"learnings: parse error at {path}: {exc}")
        return
    if not isinstance(data, dict):
        report.notes.append(f"learnings: {path} did not yield a mapping, skipping")
        return

    entries = data.get("learnings") or data.get("entries") or []
    if not isinstance(entries, list):
        report.notes.append(f"learnings: {path} has no 'learnings' or 'entries' list")
        return

    conn = reflect_db.get_conn() if execute else None
    seen = _existing_learning_hashes(conn) if execute else set()

    for entry in entries:
        if not isinstance(entry, dict):
            report.learnings_failed += 1
            continue

        title = str(entry.get("signal") or entry.get("title") or "").strip()
        if not title:
            report.learnings_failed += 1
            continue

        payload = {
            "legacy_id": entry.get("id", ""),
            "title": title,
            "category": _map_category(entry.get("category")),
            "confidence": _map_confidence(entry.get("confidence")),
            "source_quote": entry.get("source_quote", ""),
            "target_file": entry.get("target_file") or entry.get("target") or "",
            "target_section": entry.get("target_section") or entry.get("section") or "",
            "timestamp": entry.get("timestamp", ""),
        }
        chash = _content_hash(payload)

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
                # Preserve the v2 id as an event detail for forensics.
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

    # Dedup: pre-load any paths already recorded as legacy_reflection_imported.
    already_imported: set[str] = set()
    if execute:
        rows = conn.execute(
            "SELECT details_json FROM events WHERE type = 'legacy_reflection_imported'"
        ).fetchall()
        for r in rows:
            try:
                d = json.loads(r["details_json"] or "{}")
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
                text = p.read_text(encoding="utf-8", errors="replace")
            except OSError as exc:
                report.reflections_failed += 1
                report.notes.append(f"reflections: unreadable {p}: {exc}")
                continue
            preview = text[:200].replace("\n", " ").strip()
            details = {
                "path": key,
                "size": p.stat().st_size,
                "preview": preview,
                "imported_at": datetime.now(timezone.utc).isoformat(),
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
    plan = discover()
    report = MigrationReport()

    if not has_any_legacy_state(plan):
        report.notes.append("no legacy v2 state found — nothing to migrate")
        return report

    if execute:
        # Eagerly init DB so subsequent reads see the schema.
        reflect_db.init_db()
        # Back up originals to ~/.reflect/migrations/v2-backup-{ts}/.
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

    if not _HAVE_YAML:
        msg = (
            "WARNING: pyyaml not installed. Falling back to a minimal YAML parser "
            "that only supports the known v2 shapes. Install pyyaml for robustness:\n"
            "  pip install pyyaml   (or: uv pip install pyyaml)"
        )
        if args.json:
            sys.stderr.write(msg + "\n")
        else:
            print(msg + "\n")

    if not execute:
        plan = discover()
        if args.json:
            # For dry-run JSON, combine plan + simulated report.
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
