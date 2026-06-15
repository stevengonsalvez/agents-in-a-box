#!/usr/bin/env python3
"""GitHub Copilot adapter for the reflect-kb plugin.

GitHub Copilot CLI grew a native hook system (GA Feb 2026, 13 events), so
this adapter now wires full hook parity with the Claude/Codex adapters —
but in **Copilot's own config format**, which differs from Codex/Claude on
every axis that matters:

  ===================  ==============================  =========================
  axis                 claude / codex                  copilot
  ===================  ==============================  =========================
  config location      one shared settings/hooks.json  drop-in dir, one file/owner
  event-name case      ``SessionStart`` (PascalCase)   ``sessionStart`` (camelCase)
  entry nesting        ``{matcher, hooks:[{...}]}``     flat ``[{type,command}]`` + ``version:1``
  timeout field        ``timeout``                      ``timeoutSec``
  stdin keys           snake_case                       camelCase
  ===================  ==============================  =========================

Because Copilot's drop-in dir (``~/.copilot/hooks/``) combines every
``*.json`` it finds at load, we own exactly **one** file there —
``reflect.json`` — and write/delete it whole. There is no merge-into-a-
shared-file machinery (which is why this adapter does NOT reuse
``base.merge_hook_commands`` — that helper produces the Claude-shaped
two-level JSON). On uninstall we simply delete ``reflect.json`` (+ our
managed skills); foreign ``*.json`` siblings in the dir are never touched.

Like Codex, Copilot reads ``SKILL.md`` content directly and has no plugin
runtime that extracts the whole plugin tree the way Claude's
``/plugin install`` does — so this adapter physically deploys the plugin
content into ``~/.copilot/skills/`` and the hook commands in
``reflect.json`` point at those real on-disk paths:

  * ``plugins/reflect/skills/<name>/`` → ``~/.copilot/skills/<name>/``
    (recursive: SKILL.md + hooks/ + scripts/).
  * ``plugins/reflect/{hooks,scripts,assets,references}/`` →
    ``~/.copilot/skills/reflect/{hooks,scripts,assets,references}/``.

A ``managed_by: reflect-kb/adapters/copilot`` sentinel injected into each
copied SKILL.md keeps uninstall safe against hand-written sibling files.

The hook commands set ``REFLECT_HARNESS=copilot`` so the recall hooks emit
Copilot's ``additionalContext`` envelope (vs the Claude
``hookSpecificOutput`` wrapper) and so the cross-harness stdin readers in
``scripts/hook_input.py`` pick up camelCase keys.

Usage::

    python copilot_adapter.py install --dry-run
    python copilot_adapter.py install
    python copilot_adapter.py install --force        # overwrite hand-written siblings
    python copilot_adapter.py install --no-hooks     # skip writing reflect.json
    python copilot_adapter.py install --no-bg-drain  # sessionStart recall only (no drain)
    python copilot_adapter.py uninstall

Sister to :mod:`codex_adapter`; the install/uninstall pointer mechanics
live on :class:`AdapterBase`.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path
from typing import Any, Optional

# Make the shared base importable whether the script is invoked directly
# or through pytest. See codex_adapter.py for the same pattern.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from base import (  # noqa: E402
    AdapterBase,
    InstallPlan,
    PLUGIN_SKILLS,  # re-exported for backwards-compat with tests
    find_plugin_root as _shared_find_plugin_root,
    inject_managed_by as _inject_managed_by,
    run_cli,
)

POINTER_MANAGED_BY = "reflect-kb/adapters/copilot"
HARNESS_DIR = ".copilot"

# Copilot's drop-in hook dir combines all ``*.json`` at load; we own this
# single file under it so install writes it whole and uninstall deletes it.
HOOKS_DIR = "hooks"
HOOKS_FILE = "reflect.json"

# Copilot drop-in schema version (top-level ``version: 1``).
COPILOT_HOOKS_SCHEMA_VERSION = 1

# Skill subdirectories synced verbatim alongside SKILL.md (same as codex).
SKILL_SUBDIRS: tuple[str, ...] = ("hooks", "scripts", "assets", "references")

# Plugin-level resources not tied to a single skill; land under the
# ``reflect`` umbrella skill so the hook commands resolve at runtime.
PLUGIN_ROOT_RESOURCES: tuple[str, ...] = ("hooks", "scripts", "assets", "references")

# Single-file plugin-root resources copied next to the reflect skill.
PLUGIN_ROOT_FILES: tuple[str, ...] = ("reflect.toml",)


# --- Hook command templates --------------------------------------------------
#
# Each renders with ``home_tool_dir`` = resolved ``~/.copilot`` path at
# install time (eager substitution like codex's ``_render``, so the
# persisted reflect.json never carries literal ``{{...}}``).
#
# ``REFLECT_HARNESS=copilot`` is prefixed on the ``uv run`` commands so the
# recall hooks emit Copilot's additionalContext envelope and the
# cross-harness stdin readers pick camelCase keys. It is a POSIX
# simple-command env prefix (repo is Unix-only); it is intentionally NOT
# applied to the drain command, which is a ``(subshell ...)`` where a
# ``VAR=val`` prefix is invalid shell syntax (and the drain script doesn't
# read REFLECT_HARNESS anyway).

_HARNESS_ENV = "REFLECT_HARNESS=copilot "

_RECALL_HOOK_TEMPLATE = (
    _HARNESS_ENV
    + "uv run {home_tool_dir}/skills/recall/hooks/session_start_recall.py"
)
_PRECOMPACT_HOOK_TEMPLATE = (
    _HARNESS_ENV
    + "uv run {home_tool_dir}/skills/reflect/hooks/"
    "precompact_reflect.py --auto --verbose"
)
# Detached background drain — same ``(nohup ... &)`` pattern as codex so
# the parent exits immediately while the drain keeps running. NO env prefix
# (subshell — see note above).
_DRAIN_HOOK_TEMPLATE = (
    "(nohup {home_tool_dir}/skills/reflect/hooks/"
    "reflect-drain-bg.sh >/dev/null 2>&1 &) >/dev/null 2>&1"
)
_USER_PROMPT_RECALL_TEMPLATE = (
    _HARNESS_ENV
    + "uv run {home_tool_dir}/skills/recall/hooks/user_prompt_submit_recall.py"
)
_POSTTOOLUSE_MINILEARNING_TEMPLATE = (
    _HARNESS_ENV
    + "uv run {home_tool_dir}/skills/reflect/hooks/posttooluse_minilearning.py"
)
_STOP_REFLECT_TEMPLATE = (
    _HARNESS_ENV
    + "uv run {home_tool_dir}/skills/reflect/hooks/stop_reflect.py"
)

# Timeout (seconds) on the detached drain so Copilot never blocks long on
# session start; mirrors the codex ``timeout: 5`` on its drain entry.
_DRAIN_TIMEOUT_SEC = 5


def _render(template: str, copilot_dir: Path) -> str:
    return template.format(home_tool_dir=str(copilot_dir))


def _render_recall_hook_command(copilot_dir: Path) -> str:
    return _render(_RECALL_HOOK_TEMPLATE, copilot_dir)


def _render_precompact_hook_command(copilot_dir: Path) -> str:
    return _render(_PRECOMPACT_HOOK_TEMPLATE, copilot_dir)


def _render_drain_hook_command(copilot_dir: Path) -> str:
    return _render(_DRAIN_HOOK_TEMPLATE, copilot_dir)


def _render_user_prompt_recall_command(copilot_dir: Path) -> str:
    return _render(_USER_PROMPT_RECALL_TEMPLATE, copilot_dir)


def _render_posttooluse_minilearning_command(copilot_dir: Path) -> str:
    return _render(_POSTTOOLUSE_MINILEARNING_TEMPLATE, copilot_dir)


def _render_stop_reflect_command(copilot_dir: Path) -> str:
    return _render(_STOP_REFLECT_TEMPLATE, copilot_dir)


def _command_entry(command: str, *, timeout_sec: Optional[int] = None) -> dict:
    """Build one copilot-native hook entry.

    Copilot entries are **flat**: ``{"type":"command","command":"...",
    "timeoutSec":<n?>}`` — NOT the Claude/Codex two-level
    ``{"matcher":..., "hooks":[...]}`` shape. We use the ``command`` field
    (Copilot's cross-platform option) and ``timeoutSec`` (not ``timeout``).
    """
    entry: dict[str, Any] = {"type": "command", "command": command}
    if timeout_sec is not None:
        entry["timeoutSec"] = timeout_sec
    return entry


def _build_hooks_config(copilot_dir: Path, *, with_bg_drain: bool) -> dict:
    """Construct the whole copilot-native ``reflect.json`` document.

    Event mapping (claude/codex → copilot):

      * SessionStart recall + (optional) bg-drain → ``sessionStart``
      * PreCompact reflect                        → ``preCompact``
      * PostToolUse mini-learning                 → ``postToolUse``
      * Stop reflect-enqueue                      → ``agentStop``
      * UserPromptSubmit recall                   → ``userPromptSubmitted``
    """
    session_start = [_command_entry(_render_recall_hook_command(copilot_dir))]
    if with_bg_drain:
        session_start.append(
            _command_entry(
                _render_drain_hook_command(copilot_dir),
                timeout_sec=_DRAIN_TIMEOUT_SEC,
            )
        )
    return {
        "version": COPILOT_HOOKS_SCHEMA_VERSION,
        "hooks": {
            "sessionStart": session_start,
            "preCompact": [
                _command_entry(_render_precompact_hook_command(copilot_dir))
            ],
            "postToolUse": [
                _command_entry(
                    _render_posttooluse_minilearning_command(copilot_dir)
                )
            ],
            "agentStop": [
                _command_entry(_render_stop_reflect_command(copilot_dir))
            ],
            "userPromptSubmitted": [
                _command_entry(
                    _render_user_prompt_recall_command(copilot_dir)
                )
            ],
        },
    }


class CopilotAdapter(AdapterBase):
    """Copilot harness: full-content skill install + native drop-in hooks."""

    POINTER_MANAGED_BY = POINTER_MANAGED_BY
    HARNESS_DIR = HARNESS_DIR
    HARNESS_LABEL = "Copilot"

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

        Copilot's skill loader reads file content directly (no ``source:``
        dereference). Mirrors :meth:`CodexAdapter._pointer_body`.
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
            help="Skip writing the ~/.copilot/hooks/reflect.json drop-in.",
        )
        parser.add_argument(
            "--no-bg-drain", action="store_true",
            help="Skip the sessionStart bg-drain hook (only wire recall + "
                 "the capture hooks). The drain script shells out to "
                 "`claude -p` for reflection capture; disable on machines "
                 "without claude.",
        )

    def configure_uninstall_parser(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument(
            "--no-hooks", action="store_true",
            help="Leave reflect.json untouched; only remove skill content.",
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
        plan.extras["hooks_path"] = (
            plan.target_harness_dir / HOOKS_DIR / HOOKS_FILE
        )

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

        # Plugin-level shared resources land under the reflect umbrella
        # skill so the hook commands above resolve correctly.
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
                f"hook: write copilot-native drop-in {hooks_path} "
                f"(version:{COPILOT_HOOKS_SCHEMA_VERSION}, flat arrays, "
                f"camelCase events)"
            )
            describe_extra.append("  - sessionStart: recall" + (
                " + bg-drain" if with_bg_drain else ""))
            describe_extra.append("  - preCompact: reflect")
            describe_extra.append("  - postToolUse: mini-learning")
            describe_extra.append("  - agentStop: reflect-enqueue")
            describe_extra.append("  - userPromptSubmitted: recall")
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

        # 1. Sync per-skill subdirs (merge copy — never deletes siblings).
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

        # 4. Write the copilot-native reflect.json drop-in (whole file).
        if with_hooks:
            try:
                hook_actions = self._write_hooks(
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
        hooks_path = home / self.HARNESS_DIR / HOOKS_DIR / HOOKS_FILE
        if not hooks_path.exists():
            return []
        # We own the whole file — delete it unconditionally (corrupt or not).
        # Foreign ``*.json`` siblings in the drop-in dir are untouched.
        try:
            hooks_path.unlink()
        except OSError as exc:
            return [f"failed to remove {hooks_path}: {exc}"]
        actions = [f"removed reflect drop-in {hooks_path}"]
        # Best-effort: prune the hooks dir if no foreign siblings remain.
        try:
            hooks_path.parent.rmdir()
        except OSError:
            pass
        return actions

    # --- helpers ---------------------------------------------------------

    @staticmethod
    def _sync_dir(src: Path, dst: Path) -> None:
        """Mirror ``src`` into ``dst``, overwriting same-named files.

        Does NOT delete files under ``dst`` that aren't in ``src`` so any
        user-dropped siblings survive. ``__pycache__`` and ``.DS_Store``
        are skipped because they're build/IDE noise. Mirrors
        :meth:`CodexAdapter._sync_dir`.
        """
        dst.mkdir(parents=True, exist_ok=True)
        for entry in src.iterdir():
            if entry.name in ("__pycache__", ".DS_Store"):
                continue
            target = dst / entry.name
            if entry.is_dir():
                CopilotAdapter._sync_dir(entry, target)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(entry, target)

    def _write_hooks(
        self, hooks_path: Path, *, with_bg_drain: bool,
    ) -> list[str]:
        """Write the whole copilot-native ``reflect.json`` drop-in.

        We own this file exclusively (the drop-in dir combines all
        ``*.json`` at load), so there is no merge step — we replace it
        whole. The only guard: if a ``reflect.json`` already exists and is
        corrupt JSON we refuse to clobber it (consistent with the codex
        adapter's corrupt-file behaviour), so a user can inspect what's
        there before re-installing. A well-formed existing file is simply
        overwritten with the freshly-rendered config (idempotent).
        """
        if hooks_path.exists():
            try:
                json.loads(hooks_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                raise RuntimeError(
                    f"{hooks_path} exists but is not valid JSON; "
                    f"refusing to overwrite"
                )

        copilot_dir = hooks_path.parent.parent  # ~/.copilot/hooks/.. → ~/.copilot
        config = _build_hooks_config(copilot_dir, with_bg_drain=with_bg_drain)
        hooks_path.parent.mkdir(parents=True, exist_ok=True)
        hooks_path.write_text(
            json.dumps(config, indent=2, sort_keys=False) + "\n",
            encoding="utf-8",
        )
        return [f"wrote copilot-native reflect hooks drop-in to {hooks_path}"]


# --- backwards-compatible module-level API ------------------------------

_DEFAULT_ADAPTER = CopilotAdapter(__file__)


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
