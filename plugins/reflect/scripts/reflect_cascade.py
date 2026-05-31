#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Reflect cascade — bounded, cheap pre-processing for the drainer (W4).

The 2026-05-31 incident burned 41.5M tokens because the drainer handed a full
123K-token transcript to an Opus agent that roamed with Bash for 223 turns.
The cascade replaces that with a deterministic, cheap front-end:

    1 GATE   reflect_gate.evaluate ($0) -> skip reflect-on-reflect / no-signal
    2 SLICE  keep only the signal-bearing dialogue windows (~5-15K), not 123K
    3 DEDUP  content-hash the signal set; skip if already captured (fast-path)
    -> hand the SLICE to the existing /reflect write workflow, on Sonnet, with
       a low turn budget.

Why slice instead of reimplementing extract/write: the existing /reflect skill
already knows how to write learning docs + entity sidecars into the KB and
dedup against it (the vector half of decision #7). The cascade's job is to make
its INPUT tiny and its MODEL cheap — that is the 20-50x lever. We do NOT
duplicate the KB write/vector-dedup layer here.

`prepare` is pure/deterministic (no LLM, no network) so it is fully unit
testable; the actual Sonnet /reflect call happens in the drainer.

CLI:
    reflect_cascade.py prepare <transcript.jsonl> [--out SLICE] [--context N]
        -> JSON {action, reason, signal_count, slice_path, orig_tokens,
                 slice_tokens, signal_hash}
"""

from __future__ import annotations

import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import reflect_gate  # noqa: E402

try:
    from signal_detector import detect_signals  # noqa: E402
except Exception:  # pragma: no cover
    detect_signals = None  # type: ignore[assignment]


_DEFAULT_CONTEXT_LINES = 3
_MAX_SLICE_CHARS = 60_000  # ~15K tokens — the bounded input handed to /reflect


@dataclass
class Prep:
    action: str                  # "reflect" | "skip"
    reason: str
    signal_count: int
    orig_tokens: int             # rough estimate of full-dialogue size
    slice_tokens: int            # rough estimate of the slice we will reflect on
    slice_path: Optional[str] = None
    signal_hash: str = ""


def _est_tokens(text: str) -> int:
    return len(text) // 4


def _signal_set_hash(signals) -> str:
    """Stable hash over the normalized signal strings — identical signal sets
    across re-runs collapse to one hash (the cheap candidate-dedup fast-path)."""
    keys = sorted({(s.signal or "").lower().strip() for s in signals})
    try:
        from reflect_db import compute_content_hash
        return compute_content_hash({"signals": keys})
    except Exception:
        import hashlib
        blob = json.dumps({"signals": keys}, sort_keys=True).encode("utf-8")
        return hashlib.sha256(blob).hexdigest()[:16]


def _signal_hash_seen(signal_hash: str) -> bool:
    """True if a learning with this content hash already exists in the KB.
    Best-effort: returns False if the DB is unavailable (fail-open to reflect)."""
    if not signal_hash:
        return False
    try:
        from reflect_db import get_known_content_hashes
        return signal_hash in get_known_content_hashes()
    except Exception:
        return False


def slice_dialogue(text: str, signals, context_lines: int = _DEFAULT_CONTEXT_LINES,
                   max_chars: int = _MAX_SLICE_CHARS) -> str:
    """Keep only windows of ±context_lines around each signal line; merge
    overlapping windows; preserve order; cap total size."""
    lines = text.split("\n")
    n = len(lines)
    keep = [False] * n
    for s in signals:
        ln = (s.line_number or 0) - 1  # signal line_number is 1-based
        if 0 <= ln < n:
            for j in range(max(0, ln - context_lines), min(n, ln + context_lines + 1)):
                keep[j] = True

    out: list[str] = []
    total = 0
    in_gap = False
    for i in range(n):
        if keep[i]:
            if in_gap and out:
                out.append("…")
            in_gap = False
            line = lines[i]
            out.append(line)
            total += len(line) + 1
            if total >= max_chars:
                out.append("… [slice truncated]")
                break
        else:
            in_gap = True
    return "\n".join(out)


def prepare(transcript: str | Path, *, context_lines: int = _DEFAULT_CONTEXT_LINES,
            out_path: Optional[str | Path] = None) -> Prep:
    """Gate + slice + hash-dedup. No LLM. Writes the slice file when reflecting."""
    p = Path(transcript)
    verdict = reflect_gate.evaluate(p)
    dialogue = reflect_gate.extract_dialogue(p) if p.exists() else ""
    orig_tokens = _est_tokens(dialogue)

    if verdict.action == "skip":
        return Prep("skip", verdict.reason, verdict.signal_count, orig_tokens, 0)

    if detect_signals is None:
        # No detector → reflect on the (capped) full dialogue rather than drop.
        return Prep("reflect", "detector-unavailable", 0, orig_tokens, _est_tokens(dialogue[:_MAX_SLICE_CHARS]))

    signals = detect_signals(dialogue)
    signal_hash = _signal_set_hash(signals)
    if _signal_hash_seen(signal_hash):
        return Prep("skip", "dup-signal-hash", len(signals), orig_tokens, 0,
                    signal_hash=signal_hash)

    sliced = slice_dialogue(dialogue, signals, context_lines)
    if not sliced.strip():
        sliced = dialogue[:_MAX_SLICE_CHARS]  # fail-safe: never hand empty input

    prep = Prep(
        action="reflect",
        reason="has-signal",
        signal_count=len(signals),
        orig_tokens=orig_tokens,
        slice_tokens=_est_tokens(sliced),
        signal_hash=signal_hash,
    )

    if out_path is None:
        # Sibling temp file next to the transcript's basename, in a stable spot.
        import tempfile
        fd, tmp = tempfile.mkstemp(prefix="reflect-slice-", suffix=".txt")
        out_path = tmp
        import os
        os.close(fd)
    header = (
        f"# Reflect slice of {p.name}\n"
        f"# {len(signals)} signal-bearing windows extracted from a "
        f"{orig_tokens}-token transcript ({prep.slice_tokens} tokens).\n"
        f"# Only correction/approval/knowledge exchanges are kept.\n\n"
    )
    Path(out_path).write_text(header + sliced, encoding="utf-8")
    prep.slice_path = str(out_path)
    return prep


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Reflect cascade pre-processing")
    sub = ap.add_subparsers(dest="cmd", required=True)
    pp = sub.add_parser("prepare")
    pp.add_argument("transcript")
    pp.add_argument("--out", default=None)
    pp.add_argument("--context", type=int, default=_DEFAULT_CONTEXT_LINES)
    args = ap.parse_args()

    if args.cmd == "prepare":
        prep = prepare(args.transcript, context_lines=args.context, out_path=args.out)
        print(json.dumps(asdict(prep)))
        # exit 0 = reflect (slice ready), 1 = skip
        sys.exit(0 if prep.action == "reflect" else 1)


if __name__ == "__main__":
    main()
