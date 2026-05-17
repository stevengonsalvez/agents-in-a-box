#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
OpenAI Codex / Codex CLI memory provider.

Discovers:
  - ``~/.codex/memories/*.md``
  - ``~/.codex/AGENTS.md``
  - ``<repo>/AGENTS.md`` (project-local)
"""

from datetime import datetime, timezone
from pathlib import Path

try:
    from providers import BaseProvider, DiscoveredMemory
except ImportError:
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from __init__ import BaseProvider, DiscoveredMemory

try:
    from reflect_config import get_config, resolve_path
except ImportError:
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from reflect_config import get_config, resolve_path


def _find_git_root() -> Path | None:
    """Walk up from cwd to find the nearest .git directory."""
    cwd = Path.cwd()
    for parent in [cwd, *cwd.parents]:
        if (parent / ".git").exists():
            return parent
    return None


class CodexProvider(BaseProvider):
    """Discover memory files written by Codex CLI."""

    def __init__(self) -> None:
        cfg = get_config().get("providers", {}).get("codex", {})
        self._home_dir = resolve_path(cfg.get("home_dir", "~/.codex"))
        self._memories_dir = resolve_path(
            cfg.get("memories_dir", "~/.codex/memories")
        )
        self._agents_md = resolve_path(
            cfg.get("agents_md", "~/.codex/AGENTS.md")
        )

    # ------------------------------------------------------------------
    # BaseProvider interface
    # ------------------------------------------------------------------

    def is_available(self) -> bool:
        return self._home_dir.is_dir()

    def discover(self) -> list[DiscoveredMemory]:
        if not self.is_available():
            return []

        results: list[DiscoveredMemory] = []
        now = datetime.now(timezone.utc)

        # 1. Global AGENTS.md
        if self._agents_md.is_file():
            self._add_file(self._agents_md, "codex-global", results, now)

        # 2. Per-memory markdown files
        if self._memories_dir.is_dir():
            for md_file in self._memories_dir.glob("*.md"):
                if md_file.is_file():
                    self._add_file(md_file, "codex-memories", results, now)

        # 3. Project-local AGENTS.md
        repo_root = _find_git_root()
        if repo_root:
            local_agents = repo_root / "AGENTS.md"
            if local_agents.is_file():
                self._add_file(
                    local_agents, repo_root.name, results, now,
                    metadata={"scope": "project-local"},
                )

        return results

    def cleanup(self, paths: list[Path], *, dry_run: bool = True) -> list[Path]:
        removed: list[Path] = []
        safe_prefix = str(self._home_dir)

        for p in paths:
            resolved = p.resolve()
            # Only allow cleanup under ~/.codex
            if not str(resolved).startswith(safe_prefix):
                continue

            if dry_run:
                removed.append(resolved)
                continue

            if resolved.is_file():
                resolved.unlink()
                removed.append(resolved)

        return removed

    # ------------------------------------------------------------------
    # Internal
    # ------------------------------------------------------------------

    def _add_file(
        self,
        path: Path,
        project_name: str,
        results: list[DiscoveredMemory],
        now: datetime,
        *,
        metadata: dict | None = None,
    ) -> None:
        try:
            content = path.read_text(encoding="utf-8")
        except OSError:
            return

        stat = path.stat()
        mtime = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc)

        results.append(
            DiscoveredMemory(
                source_tool="codex",
                source_path=path.resolve(),
                project_name=project_name,
                content=content,
                content_hash=DiscoveredMemory.hash_content(content),
                discovered_at=now,
                last_modified=mtime,
                metadata=metadata or {},
            )
        )
