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

S5 belief revision: prepare() additionally recalls existing learnings related
to the detected signals and embeds them in the slice with an explicit
CREATE/UPDATE/DELETE action contract (prefer UPDATE over CREATE), so the drain
writer revises beliefs at write time instead of always creating a new note.
The execution half is the ``revise`` subcommand: UPDATE merges as evidence
(proof_count++ + history snapshot, S4/S6), DELETE retires stale learnings
non-destructively (status -> reverted + reason).

CLI:
    reflect_cascade.py prepare <transcript.jsonl> [--out SLICE] [--context N]
        -> JSON {action, reason, signal_count, slice_path, orig_tokens,
                 slice_tokens, signal_hash, related_count}
    reflect_cascade.py revise [--actions JSON|FILE|-] [--source ID]
        -> JSON {executed, created, updated, deleted, skipped, errors}
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

# S5: related-learnings recall (belief revision candidates for the drain prompt)
_RELATED_LIMIT = 5            # max existing learnings surfaced per drain
_RELATED_MIN_OVERLAP = 0.5    # token overlap-coefficient floor for "related"
_RELATED_SCAN_CAP = 1000      # newest learnings scanned per recall (bounded)

# Tiny stopword set so overlap scoring keys on content words, not glue.
_STOPWORDS = frozenset({
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from",
    "has", "have", "here", "in", "into", "is", "it", "its", "of", "on", "or",
    "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "to", "was", "were", "when", "which", "with", "you", "your",
})

# Statuses a revision must never target again — retired/replaced beliefs.
_RETIRED_STATUSES = ("reverted", "superseded", "rejected")


@dataclass
class Prep:
    action: str                  # "reflect" | "skip"
    reason: str
    signal_count: int
    orig_tokens: int             # rough estimate of full-dialogue size
    slice_tokens: int            # rough estimate of the slice we will reflect on
    slice_path: Optional[str] = None
    signal_hash: str = ""
    proof_bumped: int = 0          # S4: learnings whose proof_count we bumped
    related_count: int = 0         # S5: related learnings embedded for revision


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


def _record_proof_for_hash(signal_hash: str, source_memory_id: str) -> int:
    """S4 UPDATE path: a dup signal set is new EVIDENCE, not noise.

    When the dedup fast-path skips a transcript whose signal hash already
    matches a stored learning, append the transcript as a source and bump
    that learning's proof_count so recall can trust well-evidenced rules.
    Best-effort: returns 0 if the DB is unavailable (the skip still happens).
    """
    if not signal_hash:
        return 0
    try:
        from reflect_db import add_learning_proof, get_learnings_by_content_hash
        bumped = 0
        for row in get_learnings_by_content_hash(signal_hash):
            if add_learning_proof(row["id"], source_memory_id):
                bumped += 1
        return bumped
    except Exception:
        return 0


def _content_tokens(text: str) -> set[str]:
    """Lowercased content-word tokens (stopwords + 1-char noise dropped)."""
    import re
    return {
        tok
        for tok in re.findall(r"[a-z0-9_+./-]+", (text or "").lower())
        if len(tok) >= 2 and tok not in _STOPWORDS
    }


def recall_related_learnings(signals, *, limit: int = _RELATED_LIMIT,
                             min_overlap: float = _RELATED_MIN_OVERLAP):
    """S5: recall existing (non-retired) learnings related to *signals*.

    Deterministic, stdlib-only token-overlap match between signal text /
    quotes and learning titles — no LLM, no network. Best-effort: returns []
    when the DB is unavailable (the drain still reflects, just without
    revision candidates — fail-open mirrors the rest of the cascade).
    """
    if not signals:
        return []
    try:
        from reflect_db import get_conn
        rows = get_conn().execute(
            f"""SELECT id, title, category, status, proof_count, created_at
                FROM learnings
                WHERE status NOT IN ({", ".join("?" for _ in _RETIRED_STATUSES)})
                ORDER BY created_at DESC LIMIT ?""",
            (*_RETIRED_STATUSES, _RELATED_SCAN_CAP),
        ).fetchall()
    except Exception:
        return []

    signal_token_sets = []
    for s in signals:
        toks = _content_tokens(getattr(s, "signal", "") or "")
        toks |= _content_tokens(getattr(s, "source_quote", "") or "")
        if toks:
            signal_token_sets.append(toks)
    if not signal_token_sets:
        return []

    scored: list[tuple[float, dict]] = []
    for row in rows:
        title_tokens = _content_tokens(row["title"])
        if not title_tokens:
            continue
        # Overlap coefficient: tolerant of length mismatch between a short
        # canonical title and a long correction sentence.
        best = max(
            len(title_tokens & sig) / min(len(title_tokens), len(sig))
            for sig in signal_token_sets
        )
        if best >= min_overlap:
            scored.append((best, {
                "id": row["id"],
                "title": row["title"],
                "category": row["category"],
                "status": row["status"],
                "proof_count": row["proof_count"],
                "created_at": row["created_at"],
                "score": round(best, 3),
            }))
    scored.sort(key=lambda pair: (-pair[0], pair[1]["created_at"]))
    return [entry for _, entry in scored[:limit]]


def _build_revision_block(related: list[dict], transcript_path: str) -> str:
    """S5: the belief-revision section embedded in the slice handed to /reflect.

    Carries the related learnings plus the exact action contract and the
    command to execute it, so the drain writer needs no extra wiring. The
    'prefer UPDATE over CREATE' rule is the heart of the port: one canonical
    learning with many proofs beats near-duplicate siblings.
    """
    script = str(Path(__file__).resolve())
    payload = json.dumps(related, indent=2, sort_keys=True)
    return (
        "\n\n## Related existing learnings (belief revision)\n"
        "The learnings below already cover ground related to this session's\n"
        "signals. For each finding, emit exactly one structured action:\n\n"
        '    {"action": "CREATE"|"UPDATE"|"DELETE", "target_id": "<id>",\n'
        '     "content": "<CREATE only>", "reason": "<one sentence>"}\n\n'
        "Rules:\n"
        "- PREFER UPDATE OVER CREATE: if a finding restates a listed learning\n"
        "  (same rule, fix, or decision), do NOT write a duplicate note — emit\n"
        "  UPDATE for that id. It merges as evidence: proof_count increments,\n"
        "  this transcript is appended as a source, and a history snapshot is\n"
        "  recorded.\n"
        "- Match by the specific rule/facet, not general topic. CREATE remains\n"
        "  correct for genuinely new knowledge with no match below.\n"
        "- DELETE only when new evidence directly contradicts or supersedes a\n"
        "  listed learning (retires it as stale, non-destructively). Be very\n"
        "  conservative with deletes.\n"
        "- Every action carries a one-sentence reason.\n\n"
        "Execute UPDATE/DELETE actions with:\n"
        f"    python3 {script} revise --source {transcript_path} "
        "--actions '<json-array>'\n\n"
        f"{payload}\n"
    )


def execute_revision_actions(actions, *, source_memory_id: str = "") -> dict:
    """S5: apply structured CREATE/UPDATE/DELETE actions to the learnings DB.

    - CREATE  -> new learning row (proof_count starts at 1, S4 semantics)
    - UPDATE  -> evidence merge via add_learning_proof: proof_count++ +
                 source appended + history snapshot (S6 fires inside)
    - DELETE  -> non-destructive retire: status -> 'reverted' with the
                 action's reason (history snapshot fires inside the status
                 transition). Hindsight hard-deletes; our ledger keeps the
                 row so 'why was this retired?' stays answerable.

    Per-action failures are collected in ``errors`` — one malformed action
    never blocks the rest of the batch.
    """
    summary = {"executed": 0, "created": 0, "updated": 0, "deleted": 0,
               "skipped": 0, "errors": []}
    try:
        import reflect_db
        from domain.enums import LearningStatus
    except Exception as exc:  # pragma: no cover - import environment broken
        summary["errors"].append(f"learnings DB unavailable: {exc}")
        return summary

    for raw in actions or []:
        if not isinstance(raw, dict):
            summary["skipped"] += 1
            summary["errors"].append(f"not an action object: {raw!r}")
            continue
        action = str(raw.get("action", "")).strip().upper()
        target = str(raw.get("target_id", "") or "").strip()
        content = str(raw.get("content", "") or "").strip()
        reason = str(raw.get("reason", "") or "").strip()
        sid = str(raw.get("source_memory_id", "") or source_memory_id).strip()
        try:
            if action == "UPDATE":
                if not target:
                    summary["skipped"] += 1
                    summary["errors"].append("UPDATE missing target_id")
                    continue
                if reflect_db.get_learning(target) is None:
                    summary["skipped"] += 1
                    summary["errors"].append(f"UPDATE {target}: learning not found")
                    continue
                if reflect_db.add_learning_proof(target, sid):
                    summary["updated"] += 1
                else:
                    # Idempotent: this source already proved this learning.
                    summary["skipped"] += 1
            elif action == "DELETE":
                if not target:
                    summary["skipped"] += 1
                    summary["errors"].append("DELETE missing target_id")
                    continue
                row = reflect_db.get_learning(target)
                if row is None:
                    summary["skipped"] += 1
                    summary["errors"].append(f"DELETE {target}: learning not found")
                    continue
                if row.get("status") in _RETIRED_STATUSES:
                    summary["skipped"] += 1  # already retired — idempotent
                    continue
                reflect_db.update_learning_status(
                    target,
                    LearningStatus.REVERTED.value,
                    revert_reason=reason or "belief-revision: retired as stale",
                )
                summary["deleted"] += 1
            elif action == "CREATE":
                if not content:
                    summary["skipped"] += 1
                    summary["errors"].append("CREATE missing content")
                    continue
                reflect_db.add_learning(
                    title=content[:200],
                    category=str(raw.get("category", "") or "Unknown"),
                    confidence=str(raw.get("confidence", "") or "MEDIUM"),
                    content_hash=str(raw.get("content_hash", "") or ""),
                    source_memory_ids=[sid] if sid else None,
                )
                summary["created"] += 1
            else:
                summary["skipped"] += 1
                summary["errors"].append(f"unknown action: {action or '<empty>'}")
        except Exception as exc:
            summary["errors"].append(f"{action} {target or content[:40]}: {exc}")
    summary["executed"] = summary["created"] + summary["updated"] + summary["deleted"]
    return summary


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
        bumped = _record_proof_for_hash(signal_hash, str(p))
        return Prep("skip", "dup-signal-hash", len(signals), orig_tokens, 0,
                    signal_hash=signal_hash, proof_bumped=bumped)

    sliced = slice_dialogue(dialogue, signals, context_lines)
    if not sliced.strip():
        sliced = dialogue[:_MAX_SLICE_CHARS]  # fail-safe: never hand empty input

    # S5: recall existing learnings related to the signal set so the drain
    # writer can revise beliefs (UPDATE/DELETE) instead of always creating.
    related = recall_related_learnings(signals)

    prep = Prep(
        action="reflect",
        reason="has-signal",
        signal_count=len(signals),
        orig_tokens=orig_tokens,
        slice_tokens=_est_tokens(sliced),
        signal_hash=signal_hash,
        related_count=len(related),
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
    # M6: the slice is the LLM-bound payload — strip <private> spans and
    # machine-context wrapper tags before anything reaches the drain model.
    try:
        from privacy_filter import strip_private  # noqa: E402
        sliced = strip_private(sliced)
    except ImportError:  # pragma: no cover
        pass  # filter is best-effort; the cascade must never hard-fail on it
    body = header + sliced
    if related:
        # Appended AFTER the privacy filter on purpose: titles come from the
        # learnings DB (already-vetted artefacts), not the raw transcript.
        body += _build_revision_block(related, str(p))
    Path(out_path).write_text(body, encoding="utf-8")
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
    rv = sub.add_parser("revise")
    rv.add_argument(
        "--actions", default="-",
        help="JSON array of actions, a path to a JSON file, or '-' for stdin",
    )
    rv.add_argument(
        "--source", default="",
        help="source memory id (transcript path) recorded as UPDATE evidence",
    )
    args = ap.parse_args()

    if args.cmd == "prepare":
        prep = prepare(args.transcript, context_lines=args.context, out_path=args.out)
        print(json.dumps(asdict(prep)))
        # exit 0 = reflect (slice ready), 1 = skip
        sys.exit(0 if prep.action == "reflect" else 1)

    if args.cmd == "revise":
        raw = args.actions
        if raw == "-":
            raw = sys.stdin.read()
        elif Path(raw).is_file():
            raw = Path(raw).read_text(encoding="utf-8")
        try:
            parsed = json.loads(raw)
        except (json.JSONDecodeError, TypeError) as exc:
            print(json.dumps({"executed": 0, "created": 0, "updated": 0,
                              "deleted": 0, "skipped": 0,
                              "errors": [f"invalid actions JSON: {exc}"]}))
            sys.exit(1)
        if isinstance(parsed, dict):
            parsed = parsed.get("actions", [parsed] if parsed.get("action") else [])
        summary = execute_revision_actions(parsed, source_memory_id=args.source)
        print(json.dumps(summary))
        # exit 0 = clean run, 1 = at least one action failed/was malformed
        sys.exit(0 if not summary["errors"] else 1)


if __name__ == "__main__":
    main()
