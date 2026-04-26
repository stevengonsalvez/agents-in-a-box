#!/usr/bin/env python3
"""Claude Code adapter for the reflect-kb plugin (v4 §Phase 2 adapter work).

This is a thin wrapper that plugs reflect-kb into the Claude Code `.claude`
layout on a user's machine. It:

  1. Writes pointer skill files into ``~/.claude/skills/<skill_name>/`` so
     Claude's skill discovery picks them up. The pointer references the
     canonical SKILL.md under the reflect-kb plugin, rather than copying the
     whole tree — updates to the upstream skill propagate automatically.
  2. Merges a SessionStart hook snippet into ``~/.claude/settings.json`` so
     the recall skill fires on session start.

Both steps support ``--dry-run`` (report intent, no filesystem changes) and
are idempotent.

Usage:

    python claude_adapter.py install --dry-run
    python claude_adapter.py install
    python claude_adapter.py uninstall

The adapter detects the plugin's skills dir relative to its own path, so it
works whether invoked from the toolkit checkout or a pipx-installed copy.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Optional

# Skills exposed by the reflect plugin that the adapter installs. Each entry
# is a directory name under ``toolkit/packages/plugins/reflect/skills/``.
PLUGIN_SKILLS: tuple[str, ...] = (
    "reflect",
    "recall",
    "reflect-status",
    "consolidate",
    "ingest",
)

# Template for the SessionStart hook command. ``{{HOME_TOOL_DIR}}`` gets
# substituted at runtime with the resolved Claude home (e.g. ``~/.claude``)
# rather than at toolkit-bootstrap time, so the adapter is safe to invoke
# directly (e.g. ``python claude_adapter.py install``) without going through
# bootstrap.js's template-substitution pass.
SESSION_START_HOOK_COMMAND_TEMPLATE = (
    "uv run {home_tool_dir}/skills/recall/hooks/session_start_recall.py"
)


def _render_session_start_hook_command(claude_dir: Path) -> str:
    """Substitute the resolved Claude home into the hook command template.

    ``claude_dir`` is the *target* Claude directory (typically ``~/.claude``
    or, in tests, ``<tmp>/.claude``). We render the absolute path so that
    even when the adapter runs against a non-standard HOME the resulting
    settings.json is self-consistent.
    """
    return SESSION_START_HOOK_COMMAND_TEMPLATE.format(home_tool_dir=str(claude_dir))


# Sentinel written into the pointer file's "managed_by" field so subsequent
# runs (or uninstall) can tell the file belongs to us and is safe to replace.
POINTER_MANAGED_BY = "reflect-kb/adapters/claude"


@dataclass
class InstallPlan:
    """What the adapter intends to do on this run."""

    source_skills_dir: Path
    target_claude_dir: Path
    pointers: list[tuple[Path, Path]] = field(default_factory=list)
    add_session_start_hook: bool = False
    settings_path: Path = field(default_factory=Path)

    def describe(self) -> list[str]:
        lines: list[str] = []
        for src, dst in self.pointers:
            lines.append(f"pointer: {src} → {dst}")
        if self.add_session_start_hook:
            lines.append(
                f"hook: add SessionStart recall entry to {self.settings_path}"
            )
        return lines


def find_plugin_root(script_path: Path | None = None) -> Path:
    """Walk up from this script to the plugin root (``.../reflect/``).

    Layout::

        toolkit/packages/plugins/reflect/        ← return this
        └── adapters/claude/claude_adapter.py    ← __file__
    """
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
    *,
    home: Optional[Path] = None,
    plugin_root: Optional[Path] = None,
    with_hooks: bool = True,
) -> InstallPlan:
    """Compute (but do not execute) the work the adapter would do."""
    resolved_home = _resolve_home(home)
    root = plugin_root or find_plugin_root()
    src_skills = root / "skills"
    claude_dir = resolved_home / ".claude"

    pointers: list[tuple[Path, Path]] = []
    for name in PLUGIN_SKILLS:
        src_skill = src_skills / name / "SKILL.md"
        if not src_skill.exists():
            # Not every skill is on every version; skip silently for
            # forward-compatibility with the plugin adding/removing skills.
            continue
        dst_skill = claude_dir / "skills" / name / "SKILL.md"
        pointers.append((src_skill, dst_skill))

    return InstallPlan(
        source_skills_dir=src_skills,
        target_claude_dir=claude_dir,
        pointers=pointers,
        add_session_start_hook=with_hooks,
        settings_path=claude_dir / "settings.json",
    )


def _pointer_body(source_skill: Path) -> str:
    """A minimal SKILL.md pointer that Claude Code will load.

    The frontmatter has to satisfy Claude's skill discovery (name, description)
    so we surface both. The body points at the canonical file.
    """
    # Read upstream frontmatter to preserve name/description rather than
    # fabricating one. If upstream is unreadable, fall back to a name derived
    # from the directory.
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
                    # Single-line description only; multi-line descriptions
                    # keep the default.
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
        f"Pointer skill installed by the reflect-kb Claude adapter.\n\n"
        f"Canonical skill definition lives at `{source_skill}`. The adapter\n"
        f"writes this pointer so Claude Code's skill discovery finds the\n"
        f"reflect skill set via its standard `~/.claude/skills/` scan.\n"
    )


def _write_pointer(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(_pointer_body(src), encoding="utf-8")


def _merge_session_start_hook(settings_path: Path) -> bool:
    """Add the SessionStart recall hook to ``settings.json``. Returns True iff
    the file was changed (used for idempotency reporting).
    """
    current: dict = {}
    if settings_path.exists():
        try:
            current = json.loads(settings_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            # Don't clobber a file we can't parse. Caller is expected to
            # handle this by either fixing the JSON manually or deleting it.
            raise RuntimeError(
                f"{settings_path} exists but is not valid JSON; refusing to overwrite"
            )

    hooks = current.setdefault("hooks", {})
    session_start = hooks.setdefault("SessionStart", [])

    wanted_command = _render_session_start_hook_command(settings_path.parent)
    # Idempotency: skip if any hook command already matches our sentinel.
    # We also strip out any *legacy* unsubstituted entries left behind by an
    # earlier buggy install — see the v3.2 review fix.
    legacy_command = SESSION_START_HOOK_COMMAND_TEMPLATE.replace(
        "{home_tool_dir}", "{{HOME_TOOL_DIR}}"
    )
    cleaned_any = False
    for entry in session_start:
        original = entry.get("hooks", [])
        kept = [
            hook for hook in original
            if hook.get("command") != legacy_command
        ]
        if kept != original:
            entry["hooks"] = kept
            cleaned_any = True
    # Drop entries that lost all of their hooks during cleanup.
    session_start[:] = [e for e in session_start if e.get("hooks")]

    for entry in session_start:
        for hook in entry.get("hooks", []):
            if hook.get("command") == wanted_command:
                # Existing matching entry — only persist if we cleaned legacy.
                if cleaned_any:
                    settings_path.parent.mkdir(parents=True, exist_ok=True)
                    settings_path.write_text(
                        json.dumps(current, indent=2, sort_keys=False) + "\n",
                        encoding="utf-8",
                    )
                return cleaned_any

    session_start.append({
        "matcher": "",
        "hooks": [{"type": "command", "command": wanted_command}],
    })

    settings_path.parent.mkdir(parents=True, exist_ok=True)
    settings_path.write_text(
        json.dumps(current, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    return True


def execute(plan: InstallPlan) -> list[str]:
    """Apply an :class:`InstallPlan`. Returns a list of human-readable actions
    performed (skipped idempotent no-ops are still listed for transparency).
    """
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

    if plan.add_session_start_hook:
        changed = _merge_session_start_hook(plan.settings_path)
        if changed:
            actions.append(f"added SessionStart hook to {plan.settings_path}")
        else:
            actions.append(f"SessionStart hook already present in {plan.settings_path}")

    return actions


def uninstall(
    *, home: Optional[Path] = None, with_hooks: bool = True,
) -> list[str]:
    """Remove pointer files and our SessionStart hook entry. Idempotent."""
    actions: list[str] = []
    resolved_home = _resolve_home(home)
    skills_dir = resolved_home / ".claude" / "skills"
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
        # Best-effort directory cleanup — don't touch non-empty dirs.
        try:
            pointer.parent.rmdir()
        except OSError:
            pass
        actions.append(f"removed pointer {pointer}")

    if with_hooks:
        settings_path = resolved_home / ".claude" / "settings.json"
        if settings_path.exists():
            try:
                cfg = json.loads(settings_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                return actions + [
                    f"settings.json is not valid JSON; skipped hook removal: {settings_path}"
                ]
            changed = False
            hooks = cfg.get("hooks", {})
            ss = hooks.get("SessionStart", [])
            # Match both the rendered command (current installs) and the
            # legacy unsubstituted template (broken-by-bootstrap installs).
            wanted_command = _render_session_start_hook_command(
                settings_path.parent
            )
            legacy_command = SESSION_START_HOOK_COMMAND_TEMPLATE.replace(
                "{home_tool_dir}", "{{HOME_TOOL_DIR}}"
            )
            removable = {wanted_command, legacy_command}
            filtered: list = []
            for entry in ss:
                kept_hooks = [
                    h for h in entry.get("hooks", [])
                    if h.get("command") not in removable
                ]
                if kept_hooks != entry.get("hooks", []):
                    changed = True
                if kept_hooks:
                    new_entry = dict(entry)
                    new_entry["hooks"] = kept_hooks
                    filtered.append(new_entry)
            if changed:
                if filtered:
                    hooks["SessionStart"] = filtered
                else:
                    hooks.pop("SessionStart", None)
                if not hooks:
                    cfg.pop("hooks", None)
                settings_path.write_text(
                    json.dumps(cfg, indent=2, sort_keys=False) + "\n",
                    encoding="utf-8",
                )
                actions.append(f"removed SessionStart hook from {settings_path}")

    return actions


def _cli() -> int:
    parser = argparse.ArgumentParser(
        prog="claude-adapter",
        description="Install the reflect-kb skill set into ~/.claude/.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    install = sub.add_parser("install", help="Write pointer skills + hook entries.")
    install.add_argument("--dry-run", action="store_true", help="Report without changing files.")
    install.add_argument("--no-hooks", action="store_true", help="Skip settings.json hook merge.")
    install.add_argument("--home", type=Path, default=None, help="Override HOME (testing).")

    uninst = sub.add_parser("uninstall", help="Remove adapter-managed pointers + hook.")
    uninst.add_argument("--home", type=Path, default=None)
    uninst.add_argument("--no-hooks", action="store_true")

    args = parser.parse_args()

    if args.command == "install":
        plan = build_plan(home=args.home, with_hooks=not args.no_hooks)
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
        for line in uninstall(home=args.home, with_hooks=not args.no_hooks):
            print(line)
        return 0

    parser.error(f"unknown command: {args.command}")
    return 2


if __name__ == "__main__":
    sys.exit(_cli())
