#!/usr/bin/env python3
"""Codex CLI adapter for the reflect-kb plugin (v4 §Phase 2 adapter work).

Per the v4 spec, Codex gets first-class skill *invocation* but NOT
auto-recall on session start: the harness has no hook system parity with
Claude Code, and a shell-wrapper imitation would be fragile. So this
adapter only writes pointer skill files into ``~/.codex/skills/<name>/``;
users invoke ``/recall`` and friends manually.

Usage::

    python codex_adapter.py install --dry-run
    python codex_adapter.py install
    python codex_adapter.py uninstall

The pointer files reference the canonical ``SKILL.md`` under the reflect
plugin source so upstream edits propagate without reinstalling. A
``managed_by: reflect-kb/adapters/codex`` sentinel keeps uninstall safe
against hand-written sibling files.

Most of the install/uninstall mechanics live on :class:`AdapterBase` —
this module just supplies the harness-specific constants and pointer
body. Compare with ``claude_adapter.py`` which adds a SessionStart hook
merge step on top of the same base.

TODO(closed-loop): also wire ``~/.claude/scripts/reflect-drain-bg.sh``
into a Codex session-init hook once Codex grows hook parity. The drain
script is harness-agnostic — it reads the shared
``~/.reflect/pending_reflections.jsonl`` queue and shells out to
``claude -p /reflect <transcript>`` to capture learnings. Until Codex
gets a session-start hook (or we add a wrapper that runs the drain on
``codex`` invocation), Codex sessions piggy-back on Claude Code sessions
opening to drain the queue.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Optional

# Make the shared base importable whether the script is invoked directly
# or through pytest. See claude_adapter.py for the same pattern.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from base import (  # noqa: E402
    AdapterBase,
    InstallPlan,
    PLUGIN_SKILLS,  # re-exported for backwards-compat with tests
    find_plugin_root as _shared_find_plugin_root,
    run_cli,
)

POINTER_MANAGED_BY = "reflect-kb/adapters/codex"
HARNESS_DIR = ".codex"


class CodexAdapter(AdapterBase):
    """Codex harness: pointer install only (no hook system to wire into)."""

    POINTER_MANAGED_BY = POINTER_MANAGED_BY
    HARNESS_DIR = HARNESS_DIR
    HARNESS_LABEL = "Codex"

    POINTER_BODY_TEMPLATE = (
        "---\n"
        "name: {name}\n"
        "description: {description}\n"
        "managed_by: {managed_by}\n"
        "source: {source}\n"
        "---\n\n"
        "Pointer skill installed by the reflect-kb {harness_label} adapter.\n\n"
        "Codex has no SessionStart hook parity with Claude Code, so this\n"
        "skill is invocation-only — call `/recall`, `/reflect`, etc.\n"
        "manually. The canonical definition lives at `{source}`.\n"
    )


# --- backwards-compatible module-level API ------------------------------

_DEFAULT_ADAPTER = CodexAdapter(__file__)


def find_plugin_root(script_path: Path | None = None) -> Path:
    return _shared_find_plugin_root(script_path or Path(__file__))


def build_plan(
    *, home: Optional[Path] = None, plugin_root: Optional[Path] = None,
) -> InstallPlan:
    return _DEFAULT_ADAPTER.build_plan(home=home, plugin_root=plugin_root)


def execute(plan: InstallPlan, *, force: bool = False) -> list[str]:
    actions, _ = _DEFAULT_ADAPTER.execute(plan, force=force)
    return actions


def uninstall(*, home: Optional[Path] = None) -> list[str]:
    return _DEFAULT_ADAPTER.uninstall(home=home)


def _cli() -> int:
    return run_cli(_DEFAULT_ADAPTER)


if __name__ == "__main__":
    sys.exit(_cli())
