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
    python memory_discovery.py discover --new-only   Filter to unindexed (hash dedup)
    python memory_discovery.py stats                 Counts and line totals
    python memory_discovery.py ingested? <file>      Exit 0 if file already indexed
    python memory_discovery.py cleanup <file>        Delete listed paths
    python memory_discovery.py cleanup <file> --execute   Actually delete (default is dry-run)
    python memory_discovery.py project-id            Git repo name

Dedup vs the ingest log (`~/.learnings/.memory-ingest-log.yaml`) is BY
CONTENT HASH, not by path — path-based dedup misses files that moved
or were re-archived. Use `--new-only` or `ingested?` to get the right
answer in one call instead of agents reinventing the hash check.
"""

import argparse
import hashlib
import json
import os
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
# Ingest-log dedup helpers
# ---------------------------------------------------------------------------


def _default_ingest_log_path() -> Path:
    """Return the default ingest log path (env override honoured)."""
    learnings_home = Path(os.environ.get("LEARNINGS_HOME", Path.home() / ".learnings"))
    return learnings_home / ".memory-ingest-log.yaml"


def _load_ingested_hashes(log_path: Path | None = None) -> set[str]:
    """Parse the ingest log and return every `content_hash` value already indexed.

    Hand-rolled line scanner (no yaml dep) to keep this script standalone per
    the `# dependencies = []` shebang. The log's flat shape — repeating
    `- file:` / `  content_hash:` / `  ingested_at:` / … blocks — makes this
    safe; we extract every line that starts with `content_hash:` regardless
    of nesting.

    Quoted or unquoted hash values both work; we strip surrounding quotes.
    Hashes are 16 hex chars (sha256 prefix) but we don't validate length
    here — any non-empty string is treated as a fingerprint.
    """
    if log_path is None:
        log_path = _default_ingest_log_path()
    if not log_path.is_file():
        return set()
    hashes: set[str] = set()
    for raw in log_path.read_text().splitlines():
        stripped = raw.strip()
        if not stripped.startswith("content_hash:"):
            continue
        value = stripped.split(":", 1)[1].strip().strip('"').strip("'")
        if value:
            hashes.add(value)
    return hashes


def _hash_file(path: Path, hash_len: int = 16) -> str:
    """Return the sha256 prefix used as content fingerprint elsewhere in reflect.

    Matches the convention used by `providers/*.py` when stamping
    `DiscoveredMemory.content_hash`. Default length 16 hex chars keeps logs
    compact while collision-safe at this corpus size.
    """
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()[:hash_len]


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

    # --new-only: filter against the ingest log by content_hash. Per the
    # SKILL.md dedup rule, hashes are the canonical key — paths can shift
    # across runs (worktree paths, archive relocations) but a content
    # fingerprint doesn't, so this is the agent-safe answer to "has this
    # file been ingested already?".
    if getattr(args, "new_only", False):
        ingested = _load_ingested_hashes()
        memories = [m for m in memories if m.content_hash not in ingested]

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


def action_ingested(args: argparse.Namespace) -> None:
    """Check whether a file's content is already in the ingest log.

    Exits 0 if the file's sha256 prefix matches an entry in the log,
    1 if it's new (not yet indexed). Designed for shell/agent integration:

        if memory_discovery.py "ingested?" "$f" >/dev/null; then
            echo "skip — already indexed"
        else
            # process $f
        fi

    Use `--print-hash` to also emit the computed fingerprint for logs.
    """
    path = Path(args.file)
    if not path.is_file():
        print(f"ERROR: File not found: {path}", file=sys.stderr)
        sys.exit(2)
    file_hash = _hash_file(path)
    ingested = _load_ingested_hashes()
    is_ingested = file_hash in ingested
    if args.print_hash:
        print(file_hash)
    if args.json:
        print(json.dumps({
            "path": str(path),
            "content_hash": file_hash,
            "ingested": is_ingested,
        }))
    elif not args.print_hash:
        print("ingested" if is_ingested else "new")
    sys.exit(0 if is_ingested else 1)


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
    p_discover.add_argument(
        "--new-only", action="store_true",
        help="Only emit memories whose content_hash is NOT in the ingest log",
    )
    p_discover.set_defaults(func=action_discover)

    # stats
    p_stats = sub.add_parser("stats", help="Show counts and line totals")
    p_stats.add_argument("--provider", type=str, default=None)
    p_stats.set_defaults(func=action_stats)

    # ingested?  (exit code = was this file already indexed by reflect:ingest?)
    p_ingested = sub.add_parser(
        "ingested?",
        help="Check if a file's content_hash appears in the ingest log",
    )
    p_ingested.add_argument("file", help="Path to a memory file")
    p_ingested.add_argument("--json", action="store_true",
                            help="JSON output with hash + ingested flag")
    p_ingested.add_argument("--print-hash", action="store_true",
                            help="Print the computed sha256 prefix")
    p_ingested.set_defaults(func=action_ingested)

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
