#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Multi-tool Memory Discovery CLI.

Replaces memory_discovery.sh with a Python implementation that uses the
provider abstraction to discover memories across Claude, Codex, Gemini, and Copilot.

Usage:
    python memory_discovery.py discover              List all memories
    python memory_discovery.py discover --json       JSON output
    python memory_discovery.py discover --provider claude
    python memory_discovery.py stats                 Counts and line totals
    python memory_discovery.py cleanup <file>        Delete listed paths
    python memory_discovery.py cleanup <file> --execute   Actually delete (default is dry-run)
    python memory_discovery.py project-id            Git repo name
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

# Ensure the scripts directory is on sys.path for sibling imports
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from reflect_config import get_config
from providers import DiscoveredMemory, BaseProvider
from providers.claude import ClaudeProvider
from providers.codex import CodexProvider
from providers.copilot import CopilotProvider
from providers.gemini import GeminiProvider

# ---------------------------------------------------------------------------
# Provider registry
# ---------------------------------------------------------------------------

_PROVIDER_MAP: dict[str, type[BaseProvider]] = {
    "claude": ClaudeProvider,
    "codex": CodexProvider,
    "copilot": CopilotProvider,
    "gemini": GeminiProvider,
}


def _enabled_providers(filter_name: str | None = None) -> list[BaseProvider]:
    """Instantiate enabled providers, optionally filtered to one."""
    cfg = get_config()
    enabled = cfg.get("discovery", {}).get("enabled_providers", list(_PROVIDER_MAP.keys()))

    if filter_name:
        if filter_name not in _PROVIDER_MAP:
            print(f"ERROR: Unknown provider '{filter_name}'. "
                  f"Available: {', '.join(_PROVIDER_MAP)}", file=sys.stderr)
            sys.exit(1)
        enabled = [filter_name]

    providers: list[BaseProvider] = []
    for name in enabled:
        cls = _PROVIDER_MAP.get(name)
        if cls is None:
            continue
        try:
            provider = cls()
            if provider.is_available():
                providers.append(provider)
        except Exception:
            # Provider init failed (e.g. config issue) — skip gracefully
            continue

    return providers


def _discover_all(filter_provider: str | None = None) -> list[DiscoveredMemory]:
    """Run discovery across all enabled providers."""
    results: list[DiscoveredMemory] = []
    for provider in _enabled_providers(filter_provider):
        try:
            results.extend(provider.discover())
        except Exception as exc:
            print(f"WARNING: Provider {type(provider).__name__} failed: {exc}",
                  file=sys.stderr)
    return results


# ---------------------------------------------------------------------------
# CLI actions
# ---------------------------------------------------------------------------


def action_discover(args: argparse.Namespace) -> None:
    """List discovered memories."""
    memories = _discover_all(args.provider)

    if not memories:
        if args.json:
            print("[]")
        else:
            print("No memories discovered.")
        return

    if args.json:
        output = [
            {
                "source_tool": m.source_tool,
                "source_path": str(m.source_path),
                "project_name": m.project_name,
                "content_hash": m.content_hash,
                "last_modified": m.last_modified.isoformat(),
                "lines": m.content.count("\n") + 1,
                "metadata": m.metadata,
            }
            for m in memories
        ]
        print(json.dumps(output, indent=2))
    else:
        print(f"Discovered {len(memories)} memory file(s):\n")
        for m in memories:
            lines = m.content.count("\n") + 1
            print(f"  [{m.source_tool}] {m.source_path}  "
                  f"({lines} lines, project={m.project_name})")


def action_stats(args: argparse.Namespace) -> None:
    """Show aggregate statistics."""
    memories = _discover_all(args.provider if hasattr(args, "provider") else None)

    by_tool: dict[str, list[DiscoveredMemory]] = {}
    total_lines = 0
    for m in memories:
        by_tool.setdefault(m.source_tool, []).append(m)
        total_lines += m.content.count("\n") + 1

    print(f"Total memory files: {len(memories)}")
    print(f"Total lines: {total_lines}")
    print()
    for tool, mems in sorted(by_tool.items()):
        tool_lines = sum(m.content.count("\n") + 1 for m in mems)
        print(f"  {tool}: {len(mems)} file(s), {tool_lines} lines")


def action_cleanup(args: argparse.Namespace) -> None:
    """Delete memory files listed in a file (one path per line)."""
    list_file = Path(args.file)
    if not list_file.is_file():
        print(f"ERROR: File not found: {list_file}", file=sys.stderr)
        sys.exit(1)

    paths = [
        Path(line.strip())
        for line in list_file.read_text().splitlines()
        if line.strip()
    ]

    if not paths:
        print("No paths found in file.")
        return

    dry_run = not args.execute

    if dry_run:
        print("DRY RUN — pass --execute to actually delete:\n")

    deleted: list[Path] = []
    for provider in _enabled_providers():
        result = provider.cleanup(paths, dry_run=dry_run)
        deleted.extend(result)

    for p in deleted:
        action = "Would delete" if dry_run else "Deleted"
        print(f"  {action}: {p}")

    # Report unmatched paths
    deleted_set = {str(p) for p in deleted}
    unmatched = [p for p in paths if str(p.resolve()) not in deleted_set and str(p) not in deleted_set]
    if unmatched:
        print(f"\n  Skipped {len(unmatched)} path(s) outside provider scope.")

    print(f"\n{'Would delete' if dry_run else 'Deleted'}: {len(deleted)} file(s)")


def action_project_id(_args: argparse.Namespace) -> None:
    """Print the repository name derived from git remote."""
    try:
        result = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            capture_output=True, text=True, check=True,
        )
        url = result.stdout.strip()
        # Handle SSH (git@...:org/repo.git) and HTTPS (https://.../org/repo.git)
        name = url.rstrip("/").rsplit("/", 1)[-1]
        if name.endswith(".git"):
            name = name[:-4]
        print(name)
    except (subprocess.CalledProcessError, FileNotFoundError):
        print("ERROR: Not in a git repo or no 'origin' remote configured",
              file=sys.stderr)
        sys.exit(1)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Multi-tool memory discovery for Reflect"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    # discover
    p_discover = sub.add_parser("discover", help="List discovered memories")
    p_discover.add_argument("--json", action="store_true", help="JSON output")
    p_discover.add_argument("--provider", type=str, default=None,
                            help="Filter to a single provider")
    p_discover.set_defaults(func=action_discover)

    # stats
    p_stats = sub.add_parser("stats", help="Show counts and line totals")
    p_stats.add_argument("--provider", type=str, default=None)
    p_stats.set_defaults(func=action_stats)

    # cleanup
    p_cleanup = sub.add_parser("cleanup", help="Delete paths listed in a file")
    p_cleanup.add_argument("file", help="File with one path per line")
    p_cleanup.add_argument("--execute", action="store_true",
                           help="Actually delete (default is dry-run)")
    p_cleanup.set_defaults(func=action_cleanup)

    # project-id
    p_pid = sub.add_parser("project-id", help="Print git repo name")
    p_pid.set_defaults(func=action_project_id)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
