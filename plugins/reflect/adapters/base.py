#!/usr/bin/env python3
"""Shared base class for the reflect-kb harness adapters.

The Claude / Codex / Copilot adapters all do the same core thing: write
pointer ``SKILL.md`` files into ``~/.<harness>/skills/<name>/`` so each
harness's skill discovery surfaces the canonical reflect skills via its
standard scan. Only Claude additionally merges a SessionStart hook into
``settings.json``.

Before this base class the three adapters were ~80% byte-identical. This
module factors the shared mechanics out so per-harness modules become a
thin shell of constants plus, where needed, a small subclass that hooks
into the install/uninstall lifecycle.

Subclassing rules of the road:

  * Override ``POINTER_MANAGED_BY`` to a unique sentinel for your harness.
  * Override ``HARNESS_DIR`` to e.g. ``.codex`` / ``.copilot``.
  * Override ``POINTER_BODY_TEMPLATE`` if the harness needs a different
    pointer body (Codex/Copilot mention "no SessionStart hook" — Claude
    uses a generic template).
  * For harnesses with extra install steps (e.g. settings.json hook
    merging), override ``execute_extra`` / ``uninstall_extra`` and
    ``configure_install_parser`` / ``apply_install_args``.

The module also exposes ``run_cli`` which is a complete argparse-based
CLI capable of driving any subclass without per-harness boilerplate.
"""

from __future__ import annotations

import argparse
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional, Sequence

import yaml

# Skills exposed by the reflect plugin that adapters install. Each entry
# is a directory name under ``plugins/reflect/skills/``.
PLUGIN_SKILLS: tuple[str, ...] = (
    "reflect",
    "recall",
    "reflect-status",
    "consolidate",
    "ingest",
)


@dataclass
class InstallPlan:
    """What an adapter intends to do on this install run.

    ``extras`` is a free-form dict subclasses use to stash harness-specific
    follow-up state (e.g. whether the SessionStart hook should be merged).
    """

    source_skills_dir: Path
    target_harness_dir: Path
    pointers: list[tuple[Path, Path]] = field(default_factory=list)
    extras: dict[str, Any] = field(default_factory=dict)

    def describe(self) -> list[str]:
        lines = [f"pointer: {src} → {dst}" for src, dst in self.pointers]
        # Subclasses append to this via extras["describe_extra"] (a list of
        # human-readable strings) so the dry-run output stays consistent.
        lines.extend(self.extras.get("describe_extra", []))
        return lines


def _resolve_home(home: Optional[Path]) -> Path:
    if home is not None:
        return home
    env = os.environ.get("HOME")
    return Path(env) if env else Path.home()


def find_plugin_root(script_path: Path | None = None) -> Path:
    """Walk up from ``script_path`` to the reflect plugin root.

    Layout::

        plugins/reflect/                         ← return this
        └── adapters/<harness>/<adapter>.py      ← script_path

    The adapter cannot rely on a fixed relative depth because we want
    ``find_plugin_root`` to keep working if the adapters get moved deeper
    or if the plugin is installed via pipx with a different layout. Both
    ``skills/`` and ``adapters/`` must coexist at the root, which is a
    cheap structural invariant.
    """
    if script_path is None:
        raise ValueError("script_path is required; pass __file__ from the caller")
    here = script_path.resolve()
    for parent in here.parents:
        if (parent / "skills").is_dir() and (parent / "adapters").is_dir():
            return parent
    raise RuntimeError(
        f"could not find reflect plugin root walking up from {here!r}; "
        "expected a parent containing both skills/ and adapters/"
    )


def parse_skill_frontmatter(text: str) -> dict[str, Any]:
    """Parse the leading YAML frontmatter from a SKILL.md document.

    Returns ``{}`` when the document has no frontmatter or the block
    cannot be parsed. We use ``yaml.safe_load`` rather than line-by-line
    splitting because:

      * Body content can legitimately contain ``---`` (horizontal rules)
        that ``text.split('---', 2)`` would misinterpret.
      * ``description: |`` style multi-line blocks need real YAML parsing
        — a hand-rolled splitter silently dropped them previously.
      * It's simpler to read.
    """
    if not text.startswith("---"):
        return {}
    # The frontmatter terminates at the next ``---`` on its own line. We
    # search for that delimiter explicitly (rather than ``split('---', 2)``)
    # so body ``---`` rules don't trick us into eating real content.
    body = text[3:]
    # Find the first newline-anchored "---" line.
    end = -1
    cursor = 0
    while cursor < len(body):
        nl = body.find("\n", cursor)
        if nl == -1:
            break
        line = body[cursor:nl].rstrip()
        if line == "---":
            end = cursor
            break
        cursor = nl + 1
    if end == -1:
        return {}
    fm_block = body[:end]
    try:
        loaded = yaml.safe_load(fm_block)
    except yaml.YAMLError:
        return {}
    return loaded if isinstance(loaded, dict) else {}


class AdapterBase:
    """Subclass once per harness; override class constants only.

    The lifecycle methods (``build_plan``, ``execute``, ``uninstall``)
    intentionally take their inputs as kwargs so subclasses can extend
    behavior by overriding the smaller ``execute_extra`` /
    ``uninstall_extra`` hook methods rather than rewriting the whole
    install loop.
    """

    # --- subclass-overridden constants -----------------------------------

    #: Sentinel written into the pointer's ``managed_by:`` field. Must be
    #: unique per harness so cross-harness uninstalls don't trample each
    #: other when a user has multiple harnesses installed.
    POINTER_MANAGED_BY: str = ""

    #: Subdirectory of ``$HOME`` the harness reads (``.claude`` etc).
    HARNESS_DIR: str = ""

    #: Display name surfaced in CLI help text.
    HARNESS_LABEL: str = ""

    #: Body template for the pointer SKILL.md. Subclasses override when
    #: they want different end-user copy (e.g. Codex/Copilot mention
    #: "no SessionStart hook" up front, while Claude hides it).
    #: Available placeholders: ``{name}``, ``{description}``,
    #: ``{managed_by}``, ``{source}``, ``{harness_label}``.
    POINTER_BODY_TEMPLATE: str = (
        "---\n"
        "name: {name}\n"
        "description: {description}\n"
        "managed_by: {managed_by}\n"
        "source: {source}\n"
        "---\n\n"
        "Pointer skill installed by the reflect-kb {harness_label} adapter.\n\n"
        "Canonical skill definition lives at `{source}`.\n"
    )

    #: Set of CLI flag names that ``configure_install_parser`` may add.
    #: Subclasses use this to advertise extra flags to the shared CLI.

    # --- subclass __file__ injection -------------------------------------

    def __init__(self, script_file: str) -> None:
        # Subclasses pass ``__file__`` so ``find_plugin_root`` can walk
        # from the right location. Storing it on the instance avoids
        # leaking module-level globals between adapters.
        self.script_path = Path(script_file)

    # --- core mechanics --------------------------------------------------

    def find_plugin_root(self) -> Path:
        return find_plugin_root(self.script_path)

    def build_plan(
        self,
        *,
        home: Optional[Path] = None,
        plugin_root: Optional[Path] = None,
        **kwargs: Any,
    ) -> InstallPlan:
        resolved_home = _resolve_home(home)
        root = plugin_root or self.find_plugin_root()
        src_skills = root / "skills"
        harness_dir = resolved_home / self.HARNESS_DIR

        pointers: list[tuple[Path, Path]] = []
        for name in PLUGIN_SKILLS:
            src_skill = src_skills / name / "SKILL.md"
            if not src_skill.exists():
                # Forward-compatible: skill set may evolve upstream.
                continue
            dst_skill = harness_dir / "skills" / name / "SKILL.md"
            pointers.append((src_skill, dst_skill))

        plan = InstallPlan(
            source_skills_dir=src_skills,
            target_harness_dir=harness_dir,
            pointers=pointers,
        )
        self.augment_plan(plan, home=resolved_home, **kwargs)
        return plan

    def augment_plan(
        self, plan: InstallPlan, *, home: Path, **kwargs: Any,
    ) -> None:
        """Hook for subclasses to add harness-specific extras to the plan
        (e.g. set ``plan.extras['add_session_start_hook'] = True`` on
        Claude). Default implementation does nothing.
        """

    def _pointer_body(self, source_skill: Path) -> str:
        """Render the pointer SKILL.md body using upstream metadata.

        We preserve the upstream ``name`` and ``description`` (instead of
        fabricating them) so each harness's skill UI shows accurate copy.
        ``yaml.safe_load`` handles multi-line ``description: |`` blocks
        and is resilient to body content containing ``---`` rules.
        """
        name = source_skill.parent.name
        description = "Installed by reflect-kb adapter; see source for details."
        try:
            text = source_skill.read_text(encoding="utf-8")
        except OSError:
            text = ""
        meta = parse_skill_frontmatter(text)
        upstream_name = meta.get("name")
        if isinstance(upstream_name, str) and upstream_name.strip():
            name = upstream_name.strip()
        upstream_desc = meta.get("description")
        if isinstance(upstream_desc, str) and upstream_desc.strip():
            # Collapse multi-line descriptions to a single line so the
            # generated frontmatter stays valid one-line YAML. Multi-line
            # YAML in the *generated* file would round-trip fine, but the
            # one-line form keeps the pointer easy to grep.
            collapsed = " ".join(upstream_desc.split())
            description = collapsed

        return self.POINTER_BODY_TEMPLATE.format(
            name=name,
            description=description,
            managed_by=self.POINTER_MANAGED_BY,
            source=source_skill,
            harness_label=self.HARNESS_LABEL,
        )

    def _write_pointer(
        self, src: Path, dst: Path, *, force: bool = False,
    ) -> tuple[bool, str]:
        """Write the pointer at ``dst``. Returns ``(written, action_msg)``.

        Sentinel-aware skip: refuses to overwrite a pre-existing file
        that lacks the harness's ``managed_by`` sentinel unless
        ``force=True``. This protects hand-written ``SKILL.md`` files
        from being silently clobbered when the user happens to have
        named one ``recall`` (etc).

        The previous behaviour silently overwrote anything in the slot,
        which made the install destructive against existing user state.
        """
        existing = None
        if dst.exists():
            try:
                existing = dst.read_text(encoding="utf-8")
            except OSError:
                existing = None
        is_foreign = (
            existing is not None and self.POINTER_MANAGED_BY not in existing
        )
        if is_foreign and not force:
            return False, (
                f"refused to overwrite non-pointer file at {dst} "
                f"(use --force to replace)"
            )
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(self._pointer_body(src), encoding="utf-8")
        if is_foreign:
            return True, f"replaced non-pointer file at {dst}"
        return True, f"wrote pointer {dst}"

    def execute(
        self, plan: InstallPlan, *, force: bool = False, **kwargs: Any,
    ) -> tuple[list[str], int]:
        """Apply ``plan``. Returns ``(actions, exit_code)``.

        Exit code is non-zero if any pointer was refused due to a
        foreign sibling. Refused pointers still appear in ``actions`` so
        the user sees the warning before re-running with ``--force``.
        """
        actions: list[str] = []
        exit_code = 0
        for src, dst in plan.pointers:
            written, msg = self._write_pointer(src, dst, force=force)
            actions.append(msg)
            if not written:
                exit_code = 1

        extra_actions, extra_exit = self.execute_extra(plan, **kwargs)
        actions.extend(extra_actions)
        if extra_exit:
            exit_code = extra_exit
        return actions, exit_code

    def execute_extra(
        self, plan: InstallPlan, **kwargs: Any,
    ) -> tuple[list[str], int]:
        """Hook: apply harness-specific install steps. Returns
        ``(actions, exit_code)``. Default no-op.
        """
        return [], 0

    def uninstall(
        self, *, home: Optional[Path] = None, **kwargs: Any,
    ) -> list[str]:
        """Remove pointer files. Idempotent. Foreign files left untouched."""
        actions: list[str] = []
        resolved_home = _resolve_home(home)
        skills_dir = resolved_home / self.HARNESS_DIR / "skills"
        for name in PLUGIN_SKILLS:
            pointer = skills_dir / name / "SKILL.md"
            if not pointer.exists():
                continue
            try:
                content = pointer.read_text(encoding="utf-8")
            except OSError:
                continue
            if self.POINTER_MANAGED_BY not in content:
                actions.append(f"left foreign file untouched: {pointer}")
                continue
            pointer.unlink()
            try:
                # Best-effort: don't touch dirs the user has populated.
                pointer.parent.rmdir()
            except OSError:
                pass
            actions.append(f"removed pointer {pointer}")

        actions.extend(self.uninstall_extra(home=resolved_home, **kwargs))
        return actions

    def uninstall_extra(
        self, *, home: Path, **kwargs: Any,
    ) -> list[str]:
        """Hook: harness-specific uninstall steps. Default no-op."""
        return []

    # --- CLI -------------------------------------------------------------

    def configure_install_parser(self, parser: argparse.ArgumentParser) -> None:
        """Hook: subclasses may add extra ``install`` flags here."""

    def configure_uninstall_parser(self, parser: argparse.ArgumentParser) -> None:
        """Hook: subclasses may add extra ``uninstall`` flags here."""

    def install_kwargs_from_args(self, args: argparse.Namespace) -> dict[str, Any]:
        """Hook: subclasses turn extra install flags into kwargs for
        ``build_plan`` / ``execute``. Default returns ``{}``.
        """
        return {}

    def uninstall_kwargs_from_args(self, args: argparse.Namespace) -> dict[str, Any]:
        """Hook: subclasses turn extra uninstall flags into kwargs."""
        return {}

    def _cli(self, argv: Optional[Sequence[str]] = None) -> int:
        parser = argparse.ArgumentParser(
            prog=f"{self.HARNESS_DIR.lstrip('.')}-adapter",
            description=(
                f"Install the reflect-kb skill set into ~/{self.HARNESS_DIR}/."
            ),
        )
        sub = parser.add_subparsers(dest="command", required=True)

        install = sub.add_parser(
            "install", help=f"Write pointer skills into ~/{self.HARNESS_DIR}/.",
        )
        install.add_argument("--dry-run", action="store_true",
                             help="Report without changing files.")
        install.add_argument("--home", type=Path, default=None,
                             help="Override HOME (testing).")
        install.add_argument(
            "--force", action="store_true",
            help="Overwrite hand-written SKILL.md files that lack our "
                 "managed_by sentinel. Off by default to protect user files.",
        )
        self.configure_install_parser(install)

        uninst = sub.add_parser(
            "uninstall", help="Remove adapter-managed pointers.",
        )
        uninst.add_argument("--home", type=Path, default=None)
        self.configure_uninstall_parser(uninst)

        args = parser.parse_args(argv)

        if args.command == "install":
            install_kwargs = self.install_kwargs_from_args(args)
            plan = self.build_plan(home=args.home, **install_kwargs)
            if args.dry_run:
                print("[dry-run] would perform the following actions:")
                lines = plan.describe()
                if not lines:
                    print("  (nothing to do)")
                for line in lines:
                    print(f"  {line}")
                return 0
            actions, exit_code = self.execute(
                plan, force=args.force, **install_kwargs,
            )
            for line in actions:
                print(line)
            return exit_code

        if args.command == "uninstall":
            uninstall_kwargs = self.uninstall_kwargs_from_args(args)
            for line in self.uninstall(home=args.home, **uninstall_kwargs):
                print(line)
            return 0

        parser.error(f"unknown command: {args.command}")
        return 2


def run_cli(adapter: AdapterBase, argv: Optional[Sequence[str]] = None) -> int:
    """Entry point used by per-harness ``__main__`` blocks."""
    return adapter._cli(argv)
