#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Claude Code memory provider.

Discovers all memory files under ``~/.claude/projects/*/memory/`` — both the
consolidated ``MEMORY.md`` index and the atomic per-fact files alongside it
(``feedback_*.md``, ``project_*.md``, ``user_*.md``, ``reference_*.md``).

The atomic files carry the rich content; ``MEMORY.md`` is only an index of
links and one-line summaries. Older versions of this provider scanned only
``MEMORY.md`` and so silently dropped every atomic auto-memory file from the
knowledge base — incident 2026-05-16.
"""

import os
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

# Allow running standalone *or* as part of the package
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


def _project_name_from_key(dir_key: str) -> Optional[str]:
    """
    Derive a human-readable project name from the path-derived directory key.

    Claude Code encodes the working directory by replacing ``/ _ .`` with ``-``.
    We reverse-engineer the last meaningful segment.
    """
    parts = dir_key.strip("-").split("-")
    # Typically the last non-empty segment is the repo name
    for part in reversed(parts):
        if part and len(part) > 1:
            return part
    return dir_key


class ClaudeProvider(BaseProvider):
    """Discover memory files written by Claude Code."""

    def __init__(self) -> None:
        cfg = get_config().get("providers", {}).get("claude", {})
        self._projects_dir = resolve_path(
            cfg.get("projects_dir", "~/.claude/projects")
        )
        self._pattern = cfg.get("memory_pattern", "*/memory/*.md")

    # ------------------------------------------------------------------
    # BaseProvider interface
    # ------------------------------------------------------------------

    def is_available(self) -> bool:
        return self._projects_dir.is_dir()

    def discover(self) -> list[DiscoveredMemory]:
        if not self.is_available():
            return []

        results: list[DiscoveredMemory] = []
        now = datetime.now(timezone.utc)

        for memory_file in self._projects_dir.glob(self._pattern):
            if not memory_file.is_file():
                continue

            try:
                content = memory_file.read_text(encoding="utf-8")
            except OSError:
                continue

            stat = memory_file.stat()
            mtime = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc)

            dir_key = memory_file.parent.parent.name
            project_name = _project_name_from_key(dir_key)

            results.append(
                DiscoveredMemory(
                    source_tool="claude",
                    source_path=memory_file.resolve(),
                    project_name=project_name,
                    content=content,
                    content_hash=DiscoveredMemory.hash_content(content),
                    discovered_at=now,
                    last_modified=mtime,
                    metadata={"dir_key": dir_key},
                )
            )

        return results

    def cleanup(self, paths: list[Path], *, dry_run: bool = True) -> list[Path]:
        removed: list[Path] = []
        projects_prefix = str(self._projects_dir)

        for p in paths:
            resolved = p.resolve()
            if not str(resolved).startswith(projects_prefix):
                continue  # safety: refuse to delete outside projects dir

            if dry_run:
                removed.append(resolved)
                continue

            if resolved.is_file():
                resolved.unlink()
                removed.append(resolved)
                # Remove parent memory/ dir if now empty
                parent = resolved.parent
                if parent.is_dir() and not any(parent.iterdir()):
                    parent.rmdir()
                    # Remove project dir if now empty
                    grandparent = parent.parent
                    if grandparent.is_dir() and not any(grandparent.iterdir()):
                        grandparent.rmdir()

        return removed
