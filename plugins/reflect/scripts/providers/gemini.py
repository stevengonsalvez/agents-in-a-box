#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Google Gemini memory provider.

Discovers:
  - ``~/.gemini/GEMINI.md``   (global)
  - ``<project>/GEMINI.md``   (project-local)
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
    cwd = Path.cwd()
    for parent in [cwd, *cwd.parents]:
        if (parent / ".git").exists():
            return parent
    return None


class GeminiProvider(BaseProvider):
    """Discover memory files written by Gemini CLI."""

    def __init__(self) -> None:
        cfg = get_config().get("providers", {}).get("gemini", {})
        self._home_dir = resolve_path(cfg.get("home_dir", "~/.gemini"))
        self._global_md = resolve_path(
            cfg.get("global_md", "~/.gemini/GEMINI.md")
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

        # 1. Global GEMINI.md
        if self._global_md.is_file():
            self._add_file(self._global_md, "gemini-global", results, now)

        # 2. Project-local GEMINI.md
        repo_root = _find_git_root()
        if repo_root:
            local_gemini = repo_root / "GEMINI.md"
            if local_gemini.is_file():
                self._add_file(
                    local_gemini, repo_root.name, results, now,
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
                source_tool="gemini",
                source_path=path.resolve(),
                project_name=project_name,
                content=content,
                content_hash=DiscoveredMemory.hash_content(content),
                discovered_at=now,
                last_modified=mtime,
                metadata=metadata or {},
            )
        )
