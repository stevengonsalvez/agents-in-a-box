#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Provider package for Reflect memory discovery.

Exports the ``DiscoveredMemory`` dataclass and the ``BaseProvider`` ABC
that every tool-specific provider must implement.
"""

import hashlib
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Optional


@dataclass
class DiscoveredMemory:
    """Normalized memory record from any tool provider."""

    source_tool: str                          # "claude", "codex", "gemini"
    source_path: Path                         # Absolute path to memory file
    project_name: Optional[str]               # Derived project / repo name
    content: str                              # Raw content
    content_hash: str                         # SHA-256 prefix for dedup
    discovered_at: datetime                   # When we found it
    last_modified: datetime                   # File mtime
    metadata: dict = field(default_factory=dict)  # Provider-specific extras

    @staticmethod
    def hash_content(content: str) -> str:
        """Return a 16-char hex prefix of the SHA-256 digest."""
        return hashlib.sha256(content.encode()).hexdigest()[:16]


class BaseProvider(ABC):
    """
    Abstract base for tool-specific memory providers.

    Subclasses must implement ``discover``, ``is_available``, and ``cleanup``.
    """

    @abstractmethod
    def discover(self) -> list[DiscoveredMemory]:
        """Find all memory files managed by this tool."""
        ...

    @abstractmethod
    def is_available(self) -> bool:
        """Return True if the tool is installed / its directories exist."""
        ...

    @abstractmethod
    def cleanup(self, paths: list[Path], *, dry_run: bool = True) -> list[Path]:
        """
        Remove memory files at *paths*.

        When *dry_run* is True, only report what would be removed.
        Returns the list of paths actually (or would-be) deleted.
        """
        ...


__all__ = ["DiscoveredMemory", "BaseProvider"]
