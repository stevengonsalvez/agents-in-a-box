#!/usr/bin/env python3
"""Codex CLI adapter for the reflect-kb plugin.

Codex 0.129+ grew first-class hook parity with Claude Code (SessionStart,
PreCompact, PostCompact, PreToolUse, PostToolUse, PermissionRequest,
UserPromptSubmit, Stop), so this adapter now wires both:

  1. ``SessionStart`` → ``session_start_recall.py`` (inject top-3
     learnings) + ``reflect-drain-bg.sh`` (drain queued reflections in
     the background; the drain script itself still shells out to
     ``claude -p`` for reflection capture — codex sessions trigger the
     drain, claude does the heavy lifting).
  2. ``PreCompact`` → ``precompact_reflect.py --auto --verbose``
     (capture learnings before context compaction).

Because codex has no plugin runtime that extracts the whole plugin tree
the way Claude's ``/plugin install`` does, this adapter physically
deploys the plugin content into ``~/.codex/skills/`` so the hook commands
in ``~/.codex/hooks.json`` point at real on-disk paths:

  * ``plugins/reflect/skills/<name>/`` → ``~/.codex/skills/<name>/``
    (recursive: SKILL.md + hooks/ + scripts/).
  * ``plugins/reflect/{hooks,scripts,assets,references}/`` →
    ``~/.codex/skills/reflect/{hooks,scripts,assets,references}/``
    (shared plugin resources land under the ``reflect`` umbrella skill,
    matching the layout produced by older bootstraps).

A ``managed_by: reflect-kb/adapters/codex`` sentinel injected into each
copied SKILL.md keeps uninstall safe against hand-written sibling files.

Usage::

    python codex_adapter.py install --dry-run
    python codex_adapter.py install
    python codex_adapter.py install --force        # overwrite hand-written siblings
    python codex_adapter.py install --no-hooks     # skip hooks.json merge
    python codex_adapter.py install --no-bg-drain  # SessionStart-recall only
    python codex_adapter.py uninstall

Sister to :mod:`claude_adapter`; the install/uninstall mechanics live on
:class:`AdapterBase` and the JSON hook merge helpers in :mod:`base`.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path
from typing import Any, Optional

# Make the shared base importable whether the script is invoked directly
# or through pytest. See claude_adapter.py for the same pattern.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from base import (  # noqa: E402
    AdapterBase,
    InstallPlan,
    PLUGIN_SKILLS,  # re-exported for backwards-compat with tests
    find_plugin_root as _shared_find_plugin_root,
    inject_managed_by as _inject_managed_by,
    merge_hook_commands,
    remove_hook_commands,
    run_cli,
)

POINTER_MANAGED_BY = "reflect-kb/adapters/codex"
HARNESS_DIR = ".codex"
HOOKS_FILE = "hooks.json"  # codex hooks live in ~/.codex/hooks.json, not settings.json

# Skill subdirectories we sync verbatim alongside SKILL.md. Each per-skill
# install copies these from ``plugins/reflect/skills/<name>/<subdir>/`` →
# ``~/.codex/skills/<name>/<subdir>/`` when they exist upstream.
SKILL_SUBDIRS: tuple[str, ...] = ("hooks", "scripts", "assets", "references")

# Plugin-level resources that aren't tied to any single skill. They land
# under the ``reflect`` umbrella skill so the deployed layout mirrors what
# older bootstraps produced (and what the hook commands in this module
# expect at runtime).
PLUGIN_ROOT_RESOURCES: tuple[str, ...] = (
    "hooks",
    "scripts",
    "assets",
    "references",
)

# Single-file plugin-root resources copied next to the reflect skill (eg.
# ``reflect.toml`` carries plugin defaults read by reflect-kb).
PLUGIN_ROOT_FILES: tuple[str, ...] = ("reflect.toml",)


# --- Hook command templates --------------------------------------------------
#
# All three render with ``home_tool_dir`` = resolved ``~/.codex`` path at
# install time. The template form lets us:
#   * substitute eagerly (so hooks.json never carries literal ``{{...}}``)
#   * still match legacy entries on uninstall via the literal form
#
# Why three? The plugin's Claude-side autowire (plugin.json) wires three
# parallel hooks: SessionStart-recall, SessionStart-drain-bg, and PreCompact-
# reflect. Codex needs the same set since there's no plugin runtime to do
# the autowire for us.

_RECALL_HOOK_TEMPLATE = (
    "uv run {home_tool_dir}/skills/recall/hooks/session_start_recall.py"
)
_PRECOMPACT_HOOK_TEMPLATE = (
    "uv run {home_tool_dir}/skills/reflect/hooks/"
    "precompact_reflect.py --auto --verbose"
)
# Detached background drain — same pattern as the plugin.json wires for
# Claude. Wrapping in ``(nohup ... &)`` makes the parent exit immediately
# while the drain keeps running; codex doesn't block on the hook.
_DRAIN_HOOK_TEMPLATE = (
    "(nohup {home_tool_dir}/skills/reflect/hooks/"
    "reflect-drain-bg.sh >/dev/null 2>&1 &) >/dev/null 2>&1"
)


def _render(template: str, codex_dir: Path) -> str:
    return template.format(home_tool_dir=str(codex_dir))


def _render_recall_hook_command(codex_dir: Path) -> str:
    return _render(_RECALL_HOOK_TEMPLATE, codex_dir)


def _render_precompact_hook_command(codex_dir: Path) -> str:
    return _render(_PRECOMPACT_HOOK_TEMPLATE, codex_dir)


def _render_drain_hook_command(codex_dir: Path) -> str:
    return _render(_DRAIN_HOOK_TEMPLATE, codex_dir)


# Legacy literals that older buggy installs may have persisted (template
# placeholder never substituted). Match them on re-install / uninstall so
# we self-heal.
_LEGACY_RECALL_HOOK_COMMAND = _RECALL_HOOK_TEMPLATE.replace(
    "{home_tool_dir}", "{{HOME_TOOL_DIR}}"
)
_LEGACY_PRECOMPACT_HOOK_COMMAND = _PRECOMPACT_HOOK_TEMPLATE.replace(
    "{home_tool_dir}", "{{HOME_TOOL_DIR}}"
)
_LEGACY_DRAIN_HOOK_COMMAND = _DRAIN_HOOK_TEMPLATE.replace(
    "{home_tool_dir}", "{{HOME_TOOL_DIR}}"
)


class CodexAdapter(AdapterBase):
    """Codex harness: full-content skill install + hooks.json hook merge."""

    POINTER_MANAGED_BY = POINTER_MANAGED_BY
    HARNESS_DIR = HARNESS_DIR
    HARNESS_LABEL = "Codex"

    # Fallback only — _pointer_body overrides to write full SKILL.md content.
    POINTER_BODY_TEMPLATE = (
        "---\n"
        "name: {name}\n"
        "description: {description}\n"
        "managed_by: {managed_by}\n"
        "source: {source}\n"
        "---\n\n"
        "Adapter could not read the upstream SKILL.md at `{source}` —\n"
        "re-run the install once the source path is accessible.\n"
    )

    def _pointer_body(self, source_skill: Path) -> str:
        """Return the full plugin SKILL.md content with ``managed_by:`` injected.

        Codex's skill loader reads file content directly (no ``source:``
        dereference). Mirrors :meth:`ClaudeAdapter._pointer_body`.
        """
        try:
            text = source_skill.read_text(encoding="utf-8")
        except OSError:
            return super()._pointer_body(source_skill)
        return _inject_managed_by(text, self.POINTER_MANAGED_BY)

    # --- CLI flags -------------------------------------------------------

    def configure_install_parser(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument(
            "--no-hooks", action="store_true",
            help="Skip merging SessionStart/PreCompact entries into hooks.json.",
        )
        parser.add_argument(
            "--no-bg-drain", action="store_true",
            help="Skip the SessionStart bg-drain hook (only wire the recall "
                 "and PreCompact hooks). The drain script shells out to "
                 "`claude -p` for reflection capture; disable on codex-only "
                 "machines.",
        )

    def configure_uninstall_parser(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument(
            "--no-hooks", action="store_true",
            help="Leave hooks.json untouched; only remove skill content.",
        )

    def install_kwargs_from_args(self, args: argparse.Namespace) -> dict[str, Any]:
        return {
            "with_hooks": not getattr(args, "no_hooks", False),
            "with_bg_drain": not getattr(args, "no_bg_drain", False),
        }

    def uninstall_kwargs_from_args(self, args: argparse.Namespace) -> dict[str, Any]:
        return {"with_hooks": not getattr(args, "no_hooks", False)}

    # --- plan augmentation + extras --------------------------------------

    def augment_plan(
        self,
        plan: InstallPlan,
        *,
        home: Path,
        with_hooks: bool = True,
        with_bg_drain: bool = True,
        **kwargs: Any,
    ) -> None:
        plan.extras["with_hooks"] = with_hooks
        plan.extras["with_bg_drain"] = with_bg_drain
        plan.extras["hooks_path"] = plan.target_harness_dir / HOOKS_FILE

        # Per-skill subdir syncs. Each entry is (src_dir, dst_dir).
        plugin_root = self.find_plugin_root()
        subdir_syncs: list[tuple[Path, Path]] = []
        for name in PLUGIN_SKILLS:
            src_skill_dir = plugin_root / "skills" / name
            if not src_skill_dir.is_dir():
                continue
            dst_skill_dir = plan.target_harness_dir / "skills" / name
            for subdir in SKILL_SUBDIRS:
                src_sub = src_skill_dir / subdir
                if src_sub.is_dir():
                    subdir_syncs.append((src_sub, dst_skill_dir / subdir))

        # Plugin-level shared resources (hooks/scripts/assets/references)
        # land under the reflect umbrella skill so the hook commands above
        # resolve correctly.
        reflect_umbrella = plan.target_harness_dir / "skills" / "reflect"
        root_resource_syncs: list[tuple[Path, Path]] = []
        for resource in PLUGIN_ROOT_RESOURCES:
            src_dir = plugin_root / resource
            if src_dir.is_dir():
                root_resource_syncs.append((src_dir, reflect_umbrella / resource))
        root_file_copies: list[tuple[Path, Path]] = []
        for filename in PLUGIN_ROOT_FILES:
            src_file = plugin_root / filename
            if src_file.is_file():
                root_file_copies.append((src_file, reflect_umbrella / filename))

        plan.extras["subdir_syncs"] = subdir_syncs
        plan.extras["root_resource_syncs"] = root_resource_syncs
        plan.extras["root_file_copies"] = root_file_copies

        describe_extra: list[str] = []
        for src, dst in subdir_syncs:
            describe_extra.append(f"sync dir: {src} → {dst}")
        for src, dst in root_resource_syncs:
            describe_extra.append(f"sync dir: {src} → {dst}")
        for src, dst in root_file_copies:
            describe_extra.append(f"copy file: {src} → {dst}")
        if with_hooks:
            hooks_path = plan.extras["hooks_path"]
            describe_extra.append(
                f"hook: add SessionStart recall entry to {hooks_path}"
            )
            if with_bg_drain:
                describe_extra.append(
                    f"hook: add SessionStart bg-drain entry to {hooks_path}"
                )
            describe_extra.append(
                f"hook: add PreCompact reflect entry to {hooks_path}"
            )
        plan.extras["describe_extra"] = describe_extra

    def execute_extra(
        self,
        plan: InstallPlan,
        *,
        with_hooks: bool = True,
        with_bg_drain: bool = True,
        **kwargs: Any,
    ) -> tuple[list[str], int]:
        actions: list[str] = []

        # 1. Sync per-skill subdirs. We do a "merge" copy: each file under
        # ``src_dir`` overwrites the corresponding file under ``dst_dir``
        # without deleting unrelated files (so user-dropped siblings
        # under e.g. ``recall/scripts/`` survive). Adapter-uninstall only
        # removes files we actually wrote, tracked via sentinel files —
        # but since per-file sentinels would be invasive, we accept that
        # uninstall leaves these dirs in place if they still contain user
        # content (the SKILL.md sentinel is the authoritative marker).
        for src, dst in plan.extras.get("subdir_syncs", []):
            self._sync_dir(src, dst)
            actions.append(f"synced {dst}")

        # 2. Sync plugin-level shared dirs into the reflect umbrella.
        for src, dst in plan.extras.get("root_resource_syncs", []):
            self._sync_dir(src, dst)
            actions.append(f"synced {dst}")

        # 3. Copy plugin-level single files.
        for src, dst in plan.extras.get("root_file_copies", []):
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
            actions.append(f"copied {dst}")

        # 4. Merge hook entries into hooks.json.
        if with_hooks:
            try:
                hook_actions = self._merge_hooks(
                    plan.extras["hooks_path"], with_bg_drain=with_bg_drain,
                )
            except RuntimeError as exc:
                print(str(exc), file=sys.stderr)
                return actions, 2
            actions.extend(hook_actions)

        return actions, 0

    def uninstall_extra(
        self, *, home: Path, with_hooks: bool = True, **kwargs: Any,
    ) -> list[str]:
        if not with_hooks:
            return []
        hooks_path = home / self.HARNESS_DIR / HOOKS_FILE
        if not hooks_path.exists():
            return []
        try:
            cfg = json.loads(hooks_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            return [
                f"hooks.json is not valid JSON; "
                f"skipped hook removal: {hooks_path}"
            ]

        codex_dir = hooks_path.parent
        removals = {
            "SessionStart": [
                _render_recall_hook_command(codex_dir),
                _render_drain_hook_command(codex_dir),
                _LEGACY_RECALL_HOOK_COMMAND,
                _LEGACY_DRAIN_HOOK_COMMAND,
            ],
            "PreCompact": [
                _render_precompact_hook_command(codex_dir),
                _LEGACY_PRECOMPACT_HOOK_COMMAND,
            ],
        }
        changed = False
        for event, cmds in removals.items():
            if remove_hook_commands(cfg, event=event, commands=cmds):
                changed = True
        if not changed:
            return []
        hooks_path.write_text(
            json.dumps(cfg, indent=2, sort_keys=False) + "\n",
            encoding="utf-8",
        )
        return [f"removed reflect hook entries from {hooks_path}"]

    # --- helpers ---------------------------------------------------------

    @staticmethod
    def _sync_dir(src: Path, dst: Path) -> None:
        """Mirror ``src`` into ``dst``, overwriting same-named files.

        Does NOT delete files under ``dst`` that aren't in ``src`` so any
        user-dropped siblings survive. ``__pycache__`` and ``.DS_Store``
        are skipped because they're build/IDE noise that would otherwise
        ride along on every install.
        """
        dst.mkdir(parents=True, exist_ok=True)
        for entry in src.iterdir():
            if entry.name in ("__pycache__", ".DS_Store"):
                continue
            target = dst / entry.name
            if entry.is_dir():
                CodexAdapter._sync_dir(entry, target)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(entry, target)

    def _merge_hooks(
        self, hooks_path: Path, *, with_bg_drain: bool,
    ) -> list[str]:
        """Merge SessionStart + PreCompact entries into ``hooks_path``.

        Re-reads any existing ``hooks.json`` content, preserves unrelated
        hook entries, sweeps out legacy unsubstituted commands, then adds
        the reflect-managed entries idempotently.
        """
        current: dict = {}
        if hooks_path.exists():
            try:
                current = json.loads(hooks_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                raise RuntimeError(
                    f"{hooks_path} exists but is not valid JSON; "
                    f"refusing to overwrite"
                )
        if not isinstance(current, dict):
            raise RuntimeError(
                f"{hooks_path} top-level must be a JSON object; "
                f"found {type(current).__name__}"
            )

        codex_dir = hooks_path.parent
        actions: list[str] = []

        session_start_cmds: list[dict] = [
            {"type": "command", "command": _render_recall_hook_command(codex_dir)},
        ]
        if with_bg_drain:
            session_start_cmds.append({
                "type": "command",
                "command": _render_drain_hook_command(codex_dir),
                "timeout": 5,
            })

        changed_ss = merge_hook_commands(
            current,
            event="SessionStart",
            commands=session_start_cmds,
            legacy_commands=[
                _LEGACY_RECALL_HOOK_COMMAND,
                _LEGACY_DRAIN_HOOK_COMMAND,
            ],
        )
        changed_pc = merge_hook_commands(
            current,
            event="PreCompact",
            commands=[{
                "type": "command",
                "command": _render_precompact_hook_command(codex_dir),
            }],
            legacy_commands=[_LEGACY_PRECOMPACT_HOOK_COMMAND],
        )

        if changed_ss or changed_pc:
            hooks_path.parent.mkdir(parents=True, exist_ok=True)
            hooks_path.write_text(
                json.dumps(current, indent=2, sort_keys=False) + "\n",
                encoding="utf-8",
            )
            if changed_ss:
                actions.append(f"merged SessionStart reflect hooks into {hooks_path}")
            if changed_pc:
                actions.append(f"merged PreCompact reflect hook into {hooks_path}")
        else:
            actions.append(f"reflect hooks already present in {hooks_path}")
        return actions


# --- backwards-compatible module-level API ------------------------------

_DEFAULT_ADAPTER = CodexAdapter(__file__)


def find_plugin_root(script_path: Path | None = None) -> Path:
    return _shared_find_plugin_root(script_path or Path(__file__))


def build_plan(
    *,
    home: Optional[Path] = None,
    plugin_root: Optional[Path] = None,
    with_hooks: bool = True,
    with_bg_drain: bool = True,
) -> InstallPlan:
    return _DEFAULT_ADAPTER.build_plan(
        home=home,
        plugin_root=plugin_root,
        with_hooks=with_hooks,
        with_bg_drain=with_bg_drain,
    )


def execute(plan: InstallPlan, *, force: bool = False) -> list[str]:
    actions, _ = _DEFAULT_ADAPTER.execute(
        plan,
        force=force,
        with_hooks=plan.extras.get("with_hooks", True),
        with_bg_drain=plan.extras.get("with_bg_drain", True),
    )
    return actions


def uninstall(
    *, home: Optional[Path] = None, with_hooks: bool = True,
) -> list[str]:
    return _DEFAULT_ADAPTER.uninstall(home=home, with_hooks=with_hooks)


def _cli() -> int:
    return run_cli(_DEFAULT_ADAPTER)


if __name__ == "__main__":
    sys.exit(_cli())
