#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
graphml_repair.py — validate + self-heal the GraphRAG graphml (W5).

The 2026-05-31 incident's rabbit hole started because the KB graphml was
corrupt — ``not well-formed (invalid token): line 38163, column 2`` — caused by
a DOUBLED trailing close-tag block (``</graph></graphml>`` appended twice). The
drain agent discovered this mid-reflect and spent ~200 turns investigating it.

Corruption like this must self-heal as a cheap batch step, never escalate into
a reflect agent loop. This script:
  * --check   : exit 0 if the graphml parses, 1 if corrupt
  * --repair  : back up, attempt repair (truncate after the first valid
                </graphml>), re-validate; exit 0 on success/no-op, 1 if still
                broken after repair (so the caller can fall back to a full
                rebuild rather than feed a broken file forward)

Auto-discovers the graphml under common KB locations when no path is given.

CLI:
    graphml_repair.py [--check|--repair] [PATH] [--quiet]
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Optional

# Prefer defusedxml (hardened against XXE / billion-laughs entity expansion).
# The graphml is a self-generated local KB file so the risk is low, but parsing
# with the safe variant when available costs nothing. Fall back to stdlib for
# portability (the hook context is dependency-light); validity-checking catches
# all parse errors regardless of which parser is used.
try:  # pragma: no cover - import preference
    import defusedxml.ElementTree as ET  # type: ignore
except Exception:  # pragma: no cover
    import xml.etree.ElementTree as ET  # type: ignore

_CANDIDATE_GLOBS = [
    "~/.learnings/**/graph_chunk_entity_relation.graphml",
    "~/.claude/global-learnings/**/graph_chunk_entity_relation.graphml",
    "~/.reflect/**/*.graphml",
]


def discover() -> Optional[Path]:
    for pat in _CANDIDATE_GLOBS:
        base = Path(pat).expanduser()
        # split the glob: expanduser only handles ~, glob the rest
        root = Path(str(base).split("**")[0]) if "**" in str(base) else base.parent
        try:
            for hit in Path(root).glob("**/" + base.name):
                if hit.is_file():
                    return hit
        except OSError:
            continue
    return None


def is_valid(path: Path) -> bool:
    # Broad catch: stdlib raises ParseError, defusedxml may raise its own
    # EntitiesForbidden / DTDForbidden — any of these means "not a clean parse".
    try:
        ET.parse(path)
        return True
    except Exception:
        return False


def repair_text(text: str) -> Optional[str]:
    """Return repaired graphml text, or None if no safe repair is known.

    The known corruption is content AFTER the first ``</graphml>`` (a doubled
    close-tag block). Truncating to the first close tag drops the junk while
    keeping the (valid) graph above it.
    """
    close = "</graphml>"
    idx = text.find(close)
    if idx == -1:
        return None  # no close tag at all — not the doubled-tag case
    end = idx + len(close)
    if text[end:].strip() == "":
        return None  # nothing trailing — already clean (caller treats as no-op)
    return text[:end] + "\n"


def repair(path: Path, *, quiet: bool = False) -> bool:
    """Validate; if broken, back up and try the truncate repair. Returns True if
    the file ends up valid (or was already valid)."""
    def say(msg: str):
        if not quiet:
            print(msg)

    if is_valid(path):
        say(f"graphml OK: {path}")
        return True

    say(f"graphml CORRUPT: {path} — attempting repair")
    try:
        original = path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        say(f"  cannot read: {exc}")
        return False

    fixed = repair_text(original)
    if fixed is None:
        say("  no known safe repair (not the doubled-close-tag pattern); "
            "recommend a full rebuild (reflect reindex --force)")
        return False

    backup = path.with_suffix(path.suffix + ".corrupt.bak")
    try:
        backup.write_text(original, encoding="utf-8")
        path.write_text(fixed, encoding="utf-8")
    except OSError as exc:
        say(f"  write failed: {exc}")
        return False

    if is_valid(path):
        say(f"  REPAIRED (backed up to {backup.name}); dropped "
            f"{len(original) - len(fixed)} trailing bytes")
        return True

    # Repair didn't take — restore the original so we don't ship a worse file.
    try:
        path.write_text(original, encoding="utf-8")
    except OSError:
        pass
    say("  repair did not validate; restored original. Full rebuild needed.")
    return False


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Validate/repair the GraphRAG graphml")
    ap.add_argument("path", nargs="?", default=None)
    ap.add_argument("--check", action="store_true", help="validate only (exit 1 if corrupt)")
    ap.add_argument("--repair", action="store_true", help="back up + attempt repair")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    target = Path(args.path).expanduser() if args.path else discover()
    if target is None or not target.exists():
        if not args.quiet:
            print("no graphml found", file=sys.stderr)
        sys.exit(0)  # nothing to do is not an error

    if args.check or not args.repair:
        ok = is_valid(target)
        if not args.quiet:
            print(f"{'OK' if ok else 'CORRUPT'}: {target}")
        sys.exit(0 if ok else 1)

    sys.exit(0 if repair(target, quiet=args.quiet) else 1)


if __name__ == "__main__":
    main()
