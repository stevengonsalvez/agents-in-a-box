#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
GitHub Copilot CLI memory provider.

Discovers:
  - ``~/.copilot/AGENTS.md`` (user-level agent instructions)
  - ``<repo>/AGENTS.md`` (project-local, if used by Copilot)

Note: Copilot's cloud-backed per-repo memory (auto-expires 28 days) is
server-side only and not accessible from the filesystem. This provider
covers the local instruction files that the Copilot CLI reads at startup.
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


class CopilotProvider(BaseProvider):
    """Discover instruction files used by GitHub Copilot CLI."""

    def __init__(self) -> None:
        cfg = get_config().get("providers", {}).get("copilot", {})
        self._home_dir = resolve_path(cfg.get("home_dir", "~/.copilot"))
        self._agents_md = resolve_path(
            cfg.get("agents_md", "~/.copilot/AGENTS.md")
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
            self._add_file(self._agents_md, "copilot-global", results, now)

        # 2. Project-local AGENTS.md (shared with Codex convention)
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
                source_tool="copilot",
                source_path=path.resolve(),
                project_name=project_name,
                content=content,
                content_hash=DiscoveredMemory.hash_content(content),
                discovered_at=now,
                last_modified=mtime,
                metadata=metadata or {},
            )
        )
