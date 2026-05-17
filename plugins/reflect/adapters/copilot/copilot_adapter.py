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

Mirrors the Codex adapter; only the harness directory and copy differ.
Both delegate the install/uninstall mechanics to
:class:`AdapterBase`.

TODO(closed-loop): also wire ``~/.claude/scripts/reflect-drain-bg.sh``
into a Copilot session-init hook once Copilot grows hook parity. The
drain script is harness-agnostic — it reads the shared
``~/.reflect/pending_reflections.jsonl`` queue and shells out to
``claude -p /reflect <transcript>`` to capture learnings. Until then,
Copilot sessions rely on Claude Code session-starts to drain the queue.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from base import (  # noqa: E402
    AdapterBase,
    InstallPlan,
    PLUGIN_SKILLS,  # re-exported for backwards-compat with tests
    find_plugin_root as _shared_find_plugin_root,
    run_cli,
)

POINTER_MANAGED_BY = "reflect-kb/adapters/copilot"
HARNESS_DIR = ".copilot"


class CopilotAdapter(AdapterBase):
    """Copilot harness: pointer install only (no hook system parity)."""

    POINTER_MANAGED_BY = POINTER_MANAGED_BY
    HARNESS_DIR = HARNESS_DIR
    HARNESS_LABEL = "Copilot"

    POINTER_BODY_TEMPLATE = (
        "---\n"
        "name: {name}\n"
        "description: {description}\n"
        "managed_by: {managed_by}\n"
        "source: {source}\n"
        "---\n\n"
        "Pointer skill installed by the reflect-kb {harness_label} adapter.\n\n"
        "Copilot has no SessionStart hook system, so this skill is\n"
        "invocation-only — call `/recall`, `/reflect`, etc. manually.\n"
        "The canonical definition lives at `{source}`.\n"
    )


# --- backwards-compatible module-level API ------------------------------

_DEFAULT_ADAPTER = CopilotAdapter(__file__)


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
