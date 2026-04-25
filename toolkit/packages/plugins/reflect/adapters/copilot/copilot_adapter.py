#!/usr/bin/env python3
"""GitHub Copilot adapter for the reflect-kb plugin (v4 §Phase 2).

Per the v4 spec, Copilot — like Codex — gets first-class skill *invocation*
but no auto-recall on session start (no hook system to wire into). The
adapter writes pointer skill files into ``~/.copilot/skills/<name>/``;
users invoke ``/recall``, ``/reflect`` etc. manually.

Usage::

    python copilot_adapter.py install --dry-run
    python copilot_adapter.py install
    python copilot_adapter.py uninstall

The pointer files reference the canonical ``SKILL.md`` under the reflect
plugin source so upstream edits propagate without reinstalling. A
``managed_by: reflect-kb/adapters/copilot`` sentinel keeps uninstall safe
against hand-written sibling files.
"""

from __future__ import annotations

import argparse
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

PLUGIN_SKILLS: tuple[str, ...] = (
    "reflect",
    "recall",
    "reflect-status",
    "consolidate",
    "ingest",
)

POINTER_MANAGED_BY = "reflect-kb/adapters/copilot"
HARNESS_DIR = ".copilot"


@dataclass
class InstallPlan:
    source_skills_dir: Path
    target_harness_dir: Path
    pointers: list[tuple[Path, Path]] = field(default_factory=list)

    def describe(self) -> list[str]:
        return [f"pointer: {src} → {dst}" for src, dst in self.pointers]


def find_plugin_root(script_path: Path | None = None) -> Path:
    """Walk up from this script to ``.../reflect/`` (the plugin root)."""
    here = (script_path or Path(__file__)).resolve()
    for parent in here.parents:
        if (parent / "skills").is_dir() and (parent / "adapters").is_dir():
            return parent
    raise RuntimeError(
        f"could not find reflect plugin root walking up from {here!r}; "
        "expected a parent containing both skills/ and adapters/"
    )


def _resolve_home(home: Optional[Path]) -> Path:
    if home is not None:
        return home
    env = os.environ.get("HOME")
    return Path(env) if env else Path.home()


def build_plan(
    *, home: Optional[Path] = None, plugin_root: Optional[Path] = None,
) -> InstallPlan:
    resolved_home = _resolve_home(home)
    root = plugin_root or find_plugin_root()
    src_skills = root / "skills"
    harness_dir = resolved_home / HARNESS_DIR

    pointers: list[tuple[Path, Path]] = []
    for name in PLUGIN_SKILLS:
        src_skill = src_skills / name / "SKILL.md"
        if not src_skill.exists():
            continue
        dst_skill = harness_dir / "skills" / name / "SKILL.md"
        pointers.append((src_skill, dst_skill))

    return InstallPlan(
        source_skills_dir=src_skills,
        target_harness_dir=harness_dir,
        pointers=pointers,
    )


def _pointer_body(source_skill: Path) -> str:
    name = source_skill.parent.name
    description = "Installed by reflect-kb adapter; see source for details."
    try:
        text = source_skill.read_text(encoding="utf-8")
        if text.startswith("---"):
            _, fm_block, _ = text.split("---", 2)
            for line in fm_block.splitlines():
                line = line.strip()
                if line.startswith("name:"):
                    name = line.split(":", 1)[1].strip().strip("\"'")
                elif line.startswith("description:") and ":" in line:
                    rest = line.split(":", 1)[1].strip().strip("\"'")
                    if rest and not rest.startswith("|"):
                        description = rest
    except OSError:
        pass

    return (
        "---\n"
        f"name: {name}\n"
        f"description: {description}\n"
        f"managed_by: {POINTER_MANAGED_BY}\n"
        f"source: {source_skill}\n"
        "---\n\n"
        f"Pointer skill installed by the reflect-kb Copilot adapter.\n\n"
        f"Copilot has no SessionStart hook system, so this skill is\n"
        f"invocation-only — call `/recall`, `/reflect`, etc. manually.\n"
        f"The canonical definition lives at `{source_skill}`.\n"
    )


def _write_pointer(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(_pointer_body(src), encoding="utf-8")


def execute(plan: InstallPlan) -> list[str]:
    actions: list[str] = []
    for src, dst in plan.pointers:
        existing = None
        if dst.exists():
            try:
                existing = dst.read_text(encoding="utf-8")
            except OSError:
                existing = None
        _write_pointer(src, dst)
        if existing is not None and POINTER_MANAGED_BY not in existing:
            actions.append(f"replaced non-pointer file at {dst}")
        else:
            actions.append(f"wrote pointer {dst}")
    return actions


def uninstall(*, home: Optional[Path] = None) -> list[str]:
    actions: list[str] = []
    resolved_home = _resolve_home(home)
    skills_dir = resolved_home / HARNESS_DIR / "skills"
    for name in PLUGIN_SKILLS:
        pointer = skills_dir / name / "SKILL.md"
        if not pointer.exists():
            continue
        try:
            content = pointer.read_text(encoding="utf-8")
        except OSError:
            continue
        if POINTER_MANAGED_BY not in content:
            actions.append(f"left foreign file untouched: {pointer}")
            continue
        pointer.unlink()
        try:
            pointer.parent.rmdir()
        except OSError:
            pass
        actions.append(f"removed pointer {pointer}")
    return actions


def _cli() -> int:
    parser = argparse.ArgumentParser(
        prog="copilot-adapter",
        description="Install the reflect-kb skill set into ~/.copilot/.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    install = sub.add_parser("install", help="Write pointer skills into ~/.copilot/.")
    install.add_argument("--dry-run", action="store_true")
    install.add_argument("--home", type=Path, default=None)

    uninst = sub.add_parser("uninstall", help="Remove adapter-managed pointers.")
    uninst.add_argument("--home", type=Path, default=None)

    args = parser.parse_args()

    if args.command == "install":
        plan = build_plan(home=args.home)
        if args.dry_run:
            print("[dry-run] would perform the following actions:")
            for line in plan.describe():
                print(f"  {line}")
            if not plan.describe():
                print("  (nothing to do)")
            return 0
        for line in execute(plan):
            print(line)
        return 0

    if args.command == "uninstall":
        for line in uninstall(home=args.home):
            print(line)
        return 0

    parser.error(f"unknown command: {args.command}")
    return 2


if __name__ == "__main__":
    sys.exit(_cli())
