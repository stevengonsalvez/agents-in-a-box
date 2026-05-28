#!/usr/bin/env python3
"""Claude Code adapter for the reflect-kb plugin (v4 §Phase 2 adapter work).

This is a thin wrapper that plugs reflect-kb into the Claude Code `.claude`
layout on a user's machine. It:

  1. Writes the full plugin SKILL.md content into
     ``~/.claude/skills/<skill_name>/SKILL.md`` so Claude's skill discovery
     picks them up with real workflow guidance. (Earlier versions wrote a
     pointer stub that referenced the source path — Claude Code's skill
     loader does NOT dereference that ``source:`` field, so the pointer
     pattern silently loaded 12 lines of "see source" and agents improvised
     everything else. The adapter now copies content directly; a
     ``managed_by:`` sentinel in frontmatter still lets uninstall recognise
     adapter-written files.) Re-run ``install --force`` after each
     ``/plugin update`` to mirror the refreshed plugin content.
  2. Merges a SessionStart hook snippet into ``~/.claude/settings.json`` so
     the recall skill fires on session start.

Both steps support ``--dry-run`` (report intent, no filesystem changes) and
are idempotent. The Claude harness is the only one with hook parity; Codex
and Copilot still use the inherited pointer-stub pattern (their own loaders
may or may not dereference ``source:`` — see their respective adapters).

Usage::

    python claude_adapter.py install --dry-run
    python claude_adapter.py install
    python claude_adapter.py install --force        # overwrite hand-written siblings
    python claude_adapter.py install --no-hooks     # skip settings.json merge
    python claude_adapter.py uninstall

Most of the install/uninstall mechanics live on :class:`AdapterBase`. The
Claude-specific pieces are the SessionStart hook injection and the
``_pointer_body`` override that writes full content instead of a stub.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Optional

# Make the shared base importable whether the script is invoked directly
# (``python claude_adapter.py install``) or through pytest. We deliberately
# avoid turning ``adapters/`` into a proper package because the per-harness
# scripts already work as standalone executables.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from base import (  # noqa: E402
    AdapterBase,
    InstallPlan,
    PLUGIN_SKILLS,  # re-exported for backwards-compat with tests
    _resolve_home,
    find_plugin_root as _shared_find_plugin_root,
    inject_managed_by as _inject_managed_by,
    parse_skill_frontmatter,
    run_cli,
)

# Sentinel written into the pointer file's ``managed_by:`` field so subsequent
# runs (or uninstall) can tell the file belongs to us and is safe to replace.
POINTER_MANAGED_BY = "reflect-kb/adapters/claude"

# Template for the SessionStart hook command. ``{home_tool_dir}`` gets
# substituted at runtime with the resolved Claude home (e.g. ``~/.claude``)
# rather than at toolkit-bootstrap time, so the adapter is safe to invoke
# directly (e.g. ``python claude_adapter.py install``) without going
# through bootstrap.js's template-substitution pass.
SESSION_START_HOOK_COMMAND_TEMPLATE = (
    "uv run {home_tool_dir}/skills/recall/hooks/session_start_recall.py"
)


def _render_session_start_hook_command(claude_dir: Path) -> str:
    """Substitute the resolved Claude home into the hook command template."""
    return SESSION_START_HOOK_COMMAND_TEMPLATE.format(home_tool_dir=str(claude_dir))


# Legacy literal that older buggy installs persisted into settings.json.
# Kept as a constant so we can self-heal on the next install/uninstall.
_LEGACY_SESSION_START_HOOK_COMMAND = SESSION_START_HOOK_COMMAND_TEMPLATE.replace(
    "{home_tool_dir}", "{{HOME_TOOL_DIR}}"
)


class ClaudeAdapter(AdapterBase):
    """Claude harness: full-content skill install + SessionStart hook merge.

    Unlike the base adapter (which writes a pointer-stub SKILL.md and relies
    on the harness to dereference ``source:``), this adapter copies the
    plugin's SKILL.md content verbatim into ``~/.claude/skills/<name>/`` —
    Claude Code's skill loader reads the file content directly and does not
    follow the ``source:`` field. Frontmatter is mutated to set
    ``managed_by:`` so uninstall still recognises adapter-written files.
    """

    POINTER_MANAGED_BY = POINTER_MANAGED_BY
    HARNESS_DIR = ".claude"
    HARNESS_LABEL = "Claude"

    # Retained as a fallback for the rare case where the source SKILL.md is
    # unreadable; normal operation overrides this via ``_pointer_body``.
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

        Overrides :meth:`AdapterBase._pointer_body` which produces a
        pointer-stub. See the module docstring for the rationale.
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
            help="Skip merging the SessionStart hook into settings.json.",
        )

    def configure_uninstall_parser(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument(
            "--no-hooks", action="store_true",
            help="Leave settings.json untouched; only remove pointer files.",
        )

    def install_kwargs_from_args(self, args: argparse.Namespace) -> dict[str, Any]:
        return {"with_hooks": not getattr(args, "no_hooks", False)}

    def uninstall_kwargs_from_args(self, args: argparse.Namespace) -> dict[str, Any]:
        return {"with_hooks": not getattr(args, "no_hooks", False)}

    # --- plan augmentation + extras --------------------------------------

    def augment_plan(
        self, plan: InstallPlan, *, home: Path, with_hooks: bool = True, **kwargs: Any,
    ) -> None:
        plan.extras["with_hooks"] = with_hooks
        plan.extras["settings_path"] = plan.target_harness_dir / "settings.json"
        if with_hooks:
            plan.extras["describe_extra"] = [
                f"hook: add SessionStart recall entry to {plan.extras['settings_path']}",
            ]

    def execute_extra(
        self, plan: InstallPlan, *, with_hooks: bool = True, **kwargs: Any,
    ) -> tuple[list[str], int]:
        if not with_hooks:
            return [], 0
        settings_path: Path = plan.extras["settings_path"]

        # Detect whether Claude Code's plugin runtime has already installed
        # reflect via ``/plugin install reflect@agents-in-a-box``. When it
        # has, the plugin's ``plugin.json`` autowires SessionStart →
        # ``${CLAUDE_PLUGIN_ROOT}/skills/recall/hooks/session_start_recall.py``
        # from the cache at every SessionStart. Adding our own entry to
        # settings.json would point at ``~/.claude/skills/recall/hooks/``
        # (the toolkit-deployed location) and fire a SECOND copy on every
        # SessionStart — typically a stale version of the same script. So
        # if the plugin runtime owns this, the adapter steps aside (and
        # also sweeps out any legacy duplicate we wrote on a prior run).
        plugin_runtime_owns_it = self._plugin_runtime_owns_reflect(plan.target_harness_dir)
        if plugin_runtime_owns_it:
            try:
                removed = self._remove_legacy_session_start_hook(settings_path)
            except RuntimeError as exc:
                print(str(exc), file=sys.stderr)
                return [], 2
            msg = (
                f"plugin runtime owns reflect hooks; skipped settings.json merge"
                f" ({settings_path})"
            )
            if removed:
                msg += " — cleaned up legacy duplicate left by older installs"
            return [msg], 0

        try:
            changed = self._merge_session_start_hook(settings_path)
        except RuntimeError as exc:
            # Settings parse failure is fatal: refuse to silently continue,
            # otherwise we leave the user with broken JSON they can't trace.
            print(str(exc), file=sys.stderr)
            return [], 2
        if changed:
            return [f"added SessionStart hook to {settings_path}"], 0
        return [f"SessionStart hook already present in {settings_path}"], 0

    def _plugin_runtime_owns_reflect(self, claude_dir: Path) -> bool:
        """Return True iff Claude Code's plugin runtime has installed the
        reflect plugin (any version, any scope).

        Heuristic: read ``~/.claude/plugins/installed_plugins.json`` and
        look for any key matching ``reflect@*``. False on parse error /
        missing file — safer to wire the hook than to skip and silently
        leave the user with no recall.
        """
        installed_path = claude_dir / "plugins" / "installed_plugins.json"
        try:
            data = json.loads(installed_path.read_text(encoding="utf-8"))
        except (FileNotFoundError, OSError, json.JSONDecodeError):
            return False
        plugins = data.get("plugins", {})
        if not isinstance(plugins, dict):
            return False
        return any(
            isinstance(name, str) and name.split("@", 1)[0] == "reflect"
            for name in plugins.keys()
        )

    def _remove_legacy_session_start_hook(self, settings_path: Path) -> bool:
        """Remove any adapter-written SessionStart entry the user may have
        from a pre-plugin-runtime install. Returns True iff anything was
        removed. Idempotent."""
        if not settings_path.exists():
            return False
        try:
            cfg = json.loads(settings_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            raise RuntimeError(
                f"{settings_path} exists but is not valid JSON; refusing to overwrite"
            )
        wanted = _render_session_start_hook_command(settings_path.parent)
        removable = {wanted, _LEGACY_SESSION_START_HOOK_COMMAND}
        ss = cfg.get("hooks", {}).get("SessionStart", [])
        new_ss: list = []
        changed = False
        for entry in ss:
            kept = [h for h in entry.get("hooks", []) if h.get("command") not in removable]
            if kept != entry.get("hooks", []):
                changed = True
            if kept:
                new_entry = dict(entry)
                new_entry["hooks"] = kept
                new_ss.append(new_entry)
        if not changed:
            return False
        hooks_block = cfg.setdefault("hooks", {})
        if new_ss:
            hooks_block["SessionStart"] = new_ss
        else:
            hooks_block.pop("SessionStart", None)
        if not hooks_block:
            cfg.pop("hooks", None)
        settings_path.write_text(
            json.dumps(cfg, indent=2, sort_keys=False) + "\n",
            encoding="utf-8",
        )
        return True

    def uninstall_extra(
        self, *, home: Path, with_hooks: bool = True, **kwargs: Any,
    ) -> list[str]:
        if not with_hooks:
            return []
        settings_path = home / self.HARNESS_DIR / "settings.json"
        if not settings_path.exists():
            return []
        try:
            cfg = json.loads(settings_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            return [
                f"settings.json is not valid JSON; "
                f"skipped hook removal: {settings_path}"
            ]
        wanted_command = _render_session_start_hook_command(settings_path.parent)
        # Match both the rendered command (current installs) and the
        # legacy unsubstituted template (broken-by-bootstrap installs).
        removable = {wanted_command, _LEGACY_SESSION_START_HOOK_COMMAND}
        ss = cfg.get("hooks", {}).get("SessionStart", [])
        filtered: list = []
        changed = False
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
        if not changed:
            return []
        hooks = cfg.setdefault("hooks", {})
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
        return [f"removed SessionStart hook from {settings_path}"]

    # --- hook merge ------------------------------------------------------

    def _merge_session_start_hook(self, settings_path: Path) -> bool:
        """Add the SessionStart recall hook to ``settings.json``.

        Returns ``True`` iff the file was changed. Idempotent: re-running
        with the hook already present is a no-op (returns False) unless
        legacy ``{{HOME_TOOL_DIR}}`` entries are found, in which case
        they're cleaned up and the change is persisted.
        """
        current: dict = {}
        if settings_path.exists():
            try:
                current = json.loads(settings_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                # Don't clobber a file we can't parse. Caller turns this into
                # a clean CLI error rather than corrupting hand-edited config.
                raise RuntimeError(
                    f"{settings_path} exists but is not valid JSON; "
                    f"refusing to overwrite"
                )

        hooks = current.setdefault("hooks", {})
        session_start = hooks.setdefault("SessionStart", [])

        wanted_command = _render_session_start_hook_command(settings_path.parent)

        # Sweep out legacy unsubstituted entries left by the buggy v3.2
        # adapter. Without this, idempotent re-installs leave broken
        # settings.json untouched (the buggy literal sits there forever).
        cleaned_any = False
        for entry in session_start:
            original = entry.get("hooks", [])
            kept = [
                hook for hook in original
                if hook.get("command") != _LEGACY_SESSION_START_HOOK_COMMAND
            ]
            if kept != original:
                entry["hooks"] = kept
                cleaned_any = True
        # Drop entries that lost all of their hooks during cleanup so the
        # JSON stays tidy.
        session_start[:] = [e for e in session_start if e.get("hooks")]

        # Idempotency: if the rendered command is already there, only
        # write back when we cleaned legacy entries above.
        already_present = any(
            hook.get("command") == wanted_command
            for entry in session_start
            for hook in entry.get("hooks", [])
        )
        if already_present:
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


# --- backwards-compatible module-level API ------------------------------
# Tests and the existing toolkit still import functions like ``build_plan``,
# ``execute``, ``uninstall`` directly from the module. Keep those wrappers
# in place so we don't break callers; new code should use ``ClaudeAdapter``.

_DEFAULT_ADAPTER = ClaudeAdapter(__file__)


def find_plugin_root(script_path: Path | None = None) -> Path:
    """Walk up from this script (or ``script_path``) to the plugin root."""
    return _shared_find_plugin_root(script_path or Path(__file__))


def build_plan(
    *,
    home: Optional[Path] = None,
    plugin_root: Optional[Path] = None,
    with_hooks: bool = True,
) -> InstallPlan:
    """Compute (but do not execute) the work the adapter would do."""
    return _DEFAULT_ADAPTER.build_plan(
        home=home, plugin_root=plugin_root, with_hooks=with_hooks,
    )


def execute(plan: InstallPlan, *, force: bool = False) -> list[str]:
    """Apply an :class:`InstallPlan`. Returns human-readable actions."""
    actions, _ = _DEFAULT_ADAPTER.execute(
        plan,
        force=force,
        with_hooks=plan.extras.get("with_hooks", True),
    )
    return actions


def uninstall(
    *, home: Optional[Path] = None, with_hooks: bool = True,
) -> list[str]:
    """Remove pointer files and our SessionStart hook entry. Idempotent."""
    return _DEFAULT_ADAPTER.uninstall(home=home, with_hooks=with_hooks)


def _cli() -> int:
    return run_cli(_DEFAULT_ADAPTER)


if __name__ == "__main__":
    sys.exit(_cli())
