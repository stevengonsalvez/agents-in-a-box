#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Synthetic compression fallback (A5) — deterministic, zero-LLM learning capture.

When the drain LLM is unavailable — daily token budget exhausted, network down,
the writer errored 3x into poison, or the operator passed ``--no-llm`` — the
old behaviour captured NOTHING: the correction in that transcript was lost. A5
closes that gap. ``synthetic_compress`` distils a slice + detected signals into a
structured learning record using HEURISTICS ONLY, so every signal contributes to
the index even under rate-limiting, with zero LLM tokens spent.

The shape mirrors agentmemory's ``compress-synthetic.ts`` (the zero-LLM index
path) adapted to the reflect-kb learning schema:

    title       <- tool input/output  (the "what happened" line)
    concepts    <- keyword extraction  (-> frontmatter ``tags``)
    files       <- file-path regex over the slice  (-> ``files_affected``)
    importance  <- rule table over the signal mix  (-> ``confidence`` tier +
                   numeric midpoint)

Every record carries ``compression: synthetic`` in its frontmatter so rerank can
de-prioritise heuristic captures beneath LLM-authored notes, and it is written
through the SAME ``output_generator.create_knowledge_note`` path an LLM drain
uses — so a synthetic learning is byte-shaped to index identically (same
required frontmatter fields), it just carries the extra provenance flag.

Pure/deterministic: no LLM, no network, no embedding model. The same
slice+signals always yields the same record, which is what makes it a safe
fallback for the budget-exhausted / errored / no-llm paths.

CLI (invoked by the drain hook on its fallback branches):
    synthetic_compress.py <transcript_or_slice> [--reason no_llm|budget|errored]
        -> JSON {action, note_path, title, category, confidence, compression,
                 concepts, files, importance}
"""

from __future__ import annotations

import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

# The synthetic compression flag stamped into every record's frontmatter. The
# LLM path never sets ``compression``; recall/rerank keys de-prioritisation on
# exactly this value.
COMPRESSION_SYNTHETIC = "synthetic"

_TITLE_CAP = 200          # create_knowledge_note slugs/caps titles at 200 chars
_MAX_CONCEPTS = 8         # keyword concepts kept per record (bounded)
_MAX_FILES = 20           # file paths kept per record (bounded)
_MIN_KEYWORD_LEN = 3      # shorter tokens are glue, not content concepts

# File-path regex: a slash-bearing path with a known-ish extension, OR a bare
# dotted filename. Tolerant of leading ./, a/ b/ diff prefixes, and line/col
# suffixes (path:12:3). Mirrors agentmemory's path key-matching but works over
# free-text slice content where there are no structured tool-input keys.
_FILE_RE = re.compile(
    r"""
    (?<![\w/.])                        # not mid-token
    (?:\./|\.\./|[ab]/)?               # optional ./  ../  a/  b/  prefixes
    (?:[\w.-]+/)*                      # zero or more dir segments
    [\w-]+                             # stem
    \.(?:py|ts|tsx|js|jsx|go|rs|rb|java|kt|swift|c|cc|cpp|h|hpp|cs|php|sh|
        sql|md|toml|yaml|yml|json|cfg|ini|txt|html|css|scss|vue|tf)  # ext
    (?=$|[\s:;,)\]'"`])                # boundary (allow path:line suffix)
    """,
    re.VERBOSE | re.IGNORECASE,
)

# Tiny stopword set — keyword extraction keys on content words, not glue. Kept
# in sync with reflect_cascade's stopword philosophy (content-bearing only).
_STOPWORDS = frozenset({
    "the", "and", "for", "you", "your", "this", "that", "with", "from", "into",
    "have", "has", "had", "was", "were", "are", "but", "not", "all", "any",
    "can", "will", "should", "would", "could", "must", "use", "used", "using",
    "when", "what", "which", "they", "them", "their", "then", "there", "here",
    "out", "get", "got", "set", "run", "ran", "via", "per", "now", "let",
    "its", "it's", "don't", "doesn't", "isn't", "didn't", "wasn't",
})

# A5 importance rule table (the agentmemory fixed-confidence path, but made
# signal-aware so a HIGH-confidence correction outranks an approval). Maps the
# strongest detected signal confidence to a display tier + the numeric midpoint
# create_knowledge_note expects. A correction the user explicitly stated ("never
# do X") is worth more in the index than a passing "nice". Empty/unknown signals
# fall through to LOW — synthetic capture is low-trust by construction.
_IMPORTANCE_RULES = (
    # (signal-confidence-token, display tier, numeric midpoint)
    ("HIGH", "MEDIUM", 0.6),   # an LLM would have ranked this HIGH; synthetic
                               # caps one tier lower — heuristic, not adjudicated
    ("MEDIUM", "LOW", 0.3),
    ("LOW", "LOW", 0.3),
)
_IMPORTANCE_DEFAULT = ("LOW", 0.3)


@dataclass
class SyntheticRecord:
    """The structured learning a synthetic compress produced (pre-write)."""
    title: str
    category: str
    concepts: list[str]
    files: list[str]
    confidence: str
    confidence_num: float
    importance: str
    body: str
    compression: str = COMPRESSION_SYNTHETIC
    note_path: Optional[str] = None
    source_path: str = ""
    tags: list[str] = field(default_factory=list)

    def to_summary(self) -> dict:
        return {
            "action": "synthetic",
            "note_path": self.note_path,
            "title": self.title,
            "category": self.category,
            "confidence": self.confidence,
            "confidence_num": self.confidence_num,
            "compression": self.compression,
            "concepts": self.concepts,
            "files": self.files,
            "importance": self.importance,
        }


def _signal_text(sig) -> str:
    """Pull the human-readable text off a detector Signal or a plain dict."""
    if isinstance(sig, dict):
        return str(sig.get("signal", "") or "")
    return str(getattr(sig, "signal", "") or "")


def _signal_quote(sig) -> str:
    if isinstance(sig, dict):
        return str(sig.get("source_quote", "") or "")
    return str(getattr(sig, "source_quote", "") or "")


def _signal_confidence(sig) -> str:
    """Confidence token for a signal — handles the Confidence enum, a raw
    string, or a dict. Upper-cased so the rule table keys exactly."""
    if isinstance(sig, dict):
        raw = sig.get("confidence", "")
    else:
        raw = getattr(sig, "confidence", "")
    val = getattr(raw, "value", raw)  # Confidence enum -> its .value
    return str(val or "").strip().upper()


def _signal_category(sig) -> str:
    if isinstance(sig, dict):
        raw = sig.get("category", "")
    else:
        raw = getattr(sig, "category", "")
    val = getattr(raw, "value", raw)  # Category enum -> its .value
    return str(val or "").strip()


def synthesize_title(slice_text: str, signals) -> str:
    """A5 title heuristic: the "what happened" line, from tool i/o (signals).

    Prefer the first HIGH-confidence signal's text (the explicit correction),
    then any signal text, then the first non-empty slice line. Always returns a
    non-empty, capped string so the record is never title-less — title-from-i/o
    in agentmemory; here the signal IS the distilled i/o.
    """
    ordered = sorted(
        signals or [],
        key=lambda s: {"HIGH": 0, "MEDIUM": 1, "LOW": 2}.get(
            _signal_confidence(s), 3),
    )
    for sig in ordered:
        text = _signal_text(sig).strip() or _signal_quote(sig).strip()
        if text:
            return text[:_TITLE_CAP]
    for line in (slice_text or "").splitlines():
        line = line.strip().lstrip("#").strip()
        if line and not line.startswith(("…", "[")):
            return line[:_TITLE_CAP]
    return "synthetic capture (no LLM)"


def extract_concepts(slice_text: str, signals) -> list[str]:
    """A5 concepts heuristic: keyword extraction over the signal text + slice.

    Deterministic frequency rank of content tokens (stopwords/short tokens
    dropped), signal text weighted ahead of generic slice prose. Drives the
    frontmatter ``tags`` so recall's tag-overlap arm still has something to bite
    on for a heuristic capture. Bounded to ``_MAX_CONCEPTS``.
    """
    counts: dict[str, int] = {}
    order: dict[str, int] = {}

    def _eat(text: str, weight: int) -> None:
        for tok in re.findall(r"[a-z][a-z0-9_+-]{2,}", (text or "").lower()):
            if len(tok) < _MIN_KEYWORD_LEN or tok in _STOPWORDS:
                continue
            counts[tok] = counts.get(tok, 0) + weight
            order.setdefault(tok, len(order))

    for sig in signals or []:
        _eat(_signal_text(sig), weight=3)
        _eat(_signal_quote(sig), weight=2)
    _eat(slice_text, weight=1)

    # Frequency desc, then first-seen order — fully deterministic.
    ranked = sorted(counts, key=lambda t: (-counts[t], order[t]))
    return ranked[:_MAX_CONCEPTS]


def extract_files(slice_text: str, signals) -> list[str]:
    """A5 files heuristic: file-path regex over the slice + signal quotes.

    Normalises diff/relative prefixes and strips path:line:col suffixes so the
    same file referenced as ``a/foo.py`` and ``foo.py:12`` dedups to one entry.
    Order-preserving + de-duplicated, bounded to ``_MAX_FILES``. This is the
    "files affected" half of the record agentmemory pulls from path-like keys.
    """
    haystacks = [slice_text or ""]
    for sig in signals or []:
        haystacks.append(_signal_text(sig))
        haystacks.append(_signal_quote(sig))

    seen: list[str] = []
    seen_set: set[str] = set()
    for hay in haystacks:
        for m in _FILE_RE.finditer(hay):
            path = m.group(0)
            # Strip diff/relative prefixes for a canonical key.
            for prefix in ("a/", "b/", "./"):
                if path.startswith(prefix):
                    path = path[len(prefix):]
            path = path.strip()
            if path and path not in seen_set:
                seen_set.add(path)
                seen.append(path)
    return seen[:_MAX_FILES]


def score_importance(signals) -> tuple[str, float, str]:
    """A5 importance heuristic: rule table over the strongest detected signal.

    Returns ``(confidence_tier, confidence_num, importance_label)``. The
    strongest signal confidence drives the row; a synthetic capture is always
    capped one tier below what an LLM-adjudicated note would carry (heuristic,
    not reasoned), and an empty signal set falls through to LOW. This is the
    deterministic stand-in for agentmemory's fixed ``importance: 5,
    confidence: 0.3`` — signal-aware so corrections outrank approvals.
    """
    present = {_signal_confidence(s) for s in (signals or [])}
    for token, tier, num in _IMPORTANCE_RULES:
        if token in present:
            return tier, num, token.lower()
    tier, num = _IMPORTANCE_DEFAULT
    return tier, num, "none"


def _dominant_category(signals) -> str:
    """Most common non-empty signal category, else 'Unknown'. Deterministic
    (frequency, then first-seen)."""
    counts: dict[str, int] = {}
    order: dict[str, int] = {}
    for sig in signals or []:
        cat = _signal_category(sig)
        if not cat:
            continue
        counts[cat] = counts.get(cat, 0) + 1
        order.setdefault(cat, len(order))
    if not counts:
        return "Unknown"
    return sorted(counts, key=lambda c: (-counts[c], order[c]))[0]


def _build_body(record_title: str, concepts: list[str], files: list[str],
                signals, reason: str) -> str:
    """Human-readable rationale body — deterministic, no LLM. Records WHY this
    was captured heuristically (so a reviewer sees it was a fallback) plus the
    raw signal lines that drove it."""
    lines = [
        f"Synthetic (no-LLM) capture: {record_title}",
        "",
        f"Captured by the heuristic fallback because the drain LLM was "
        f"unavailable (reason: {reason or 'no_llm'}). No tokens were spent; the "
        f"fields below were derived by regex/keyword heuristics, not reasoning.",
        "",
        "### Signals",
    ]
    for sig in signals or []:
        conf = _signal_confidence(sig) or "LOW"
        text = _signal_text(sig).strip()
        if text:
            lines.append(f"- [{conf}] {text}")
    if concepts:
        lines += ["", "### Concepts", ", ".join(concepts)]
    if files:
        lines += ["", "### Files affected", "\n".join(f"- {f}" for f in files)]
    return "\n".join(lines)


def synthetic_compress(slice_text: str, signals, *,
                       source_path: str = "",
                       reason: str = "no_llm",
                       write: bool = True) -> SyntheticRecord:
    """A5: distil a slice + signals into a structured learning, ZERO LLM tokens.

    Heuristics only:
      * ``title``       <- tool i/o (strongest signal text), capped
      * ``concepts``    <- keyword extraction -> frontmatter ``tags``
      * ``files``       <- file-path regex -> ``files_affected``
      * ``importance``  <- rule table over the signal mix -> ``confidence``

    Every record carries ``compression: synthetic``. When ``write`` is True the
    record is persisted through ``output_generator.create_knowledge_note`` — the
    SAME path the LLM drain writes through — so the on-disk note is byte-shaped
    to index identically (same required frontmatter fields), differing only by
    the extra ``compression``/``files_affected`` provenance keys. ``write=False``
    returns the in-memory record without touching disk (used by callers that
    only need the structured fields, and by the proof).

    Deterministic and side-effect-bounded: the same inputs always produce the
    same record, which is what makes this a safe fallback for the
    budget-exhausted / errored-3x / --no-llm drain branches.
    """
    title = synthesize_title(slice_text, signals)
    concepts = extract_concepts(slice_text, signals)
    files = extract_files(slice_text, signals)
    confidence, confidence_num, importance = score_importance(signals)
    category = _dominant_category(signals)
    body = _build_body(title, concepts, files, signals, reason)

    record = SyntheticRecord(
        title=title,
        category=category,
        concepts=concepts,
        files=files,
        confidence=confidence,
        confidence_num=confidence_num,
        importance=importance,
        body=body,
        compression=COMPRESSION_SYNTHETIC,
        source_path=source_path,
        tags=list(concepts),
    )

    if write:
        record.note_path = _persist(record, reason=reason)
    return record


def _persist(record: SyntheticRecord, *, reason: str) -> Optional[str]:
    """Write the synthetic record via the shared knowledge-note writer.

    Routes through ``output_generator.create_knowledge_note`` so a synthetic
    note is byte-identical in frontmatter SHAPE to an LLM note (title/category/
    tags/symptoms/root_cause/key_insight/created/confidence/confidence_num/
    provenance), then stamps the ``compression`` + ``files_affected`` keys into
    the frontmatter that mark it synthetic. Best-effort: a writer failure
    returns None (the caller logs it) but never raises — the fallback must not
    crash the drain hook.
    """
    try:
        import output_generator
    except Exception:
        return None
    try:
        filepath, _slug = output_generator.create_knowledge_note(
            title=record.title,
            category=record.category,
            tags=record.tags,
            symptoms=[],
            root_cause="",
            key_insight=record.title,
            problem=record.title,
            solution=record.body,
            confidence=record.confidence,
            confidence_num=record.confidence_num,
            source_tool="reflect-synthetic",
            source_path=record.source_path,
        )
    except Exception:
        return None
    _stamp_synthetic_frontmatter(filepath, record, reason=reason)
    return str(filepath)


def stamp_frontmatter(content: str, record: "SyntheticRecord",
                      *, reason: str = "no_llm") -> str:
    """Insert the synthetic-provenance keys into an existing note's frontmatter.

    Pure string transform (no disk) so it is independently testable. Handles the
    exact shape ``output_generator.create_knowledge_note`` emits — a leading
    ``---\\n`` fence, the YAML block, then a closing ``---`` that may be GLUED to
    the last YAML line (``detected_at: "..."---``) because the writer template is
    ``f"---\\n{fm}---\\n\\n## Problem"`` and ``fm`` does not always end in a
    newline. We locate the closing fence as the ``---`` that immediately precedes
    the ``## Problem`` body and inject ``compression`` + ``files_affected`` +
    ``synthetic_reason`` just before it. Idempotent — a re-stamp does not add a
    second ``compression`` line.
    """
    if not content.startswith("---"):
        return content
    # The closing fence is the LAST ``---`` before the body. The writer template
    # is ``f"---\\n{fm}---\\n\\n## Problem"``; ``fm`` ends in a newline under
    # PyYAML (so the fence reads ``\\n---``) but NOT under the manual fallback
    # (so the fence is GLUED: ``detected_at: "..."---``). Anchor on the body
    # marker and walk back to the ``---`` that terminates the YAML, so both
    # shapes resolve to the same insertion point.
    body_marker = content.find("\n\n## ")
    search_end = body_marker if body_marker != -1 else len(content)
    close = content.rfind("---", 3, search_end + 3)  # skip the opening fence
    if close == -1:
        return content
    fm_block = content[3:close]  # between leading '---' and closing '---'
    if "compression:" in fm_block:
        return content  # idempotent — already stamped
    # Preserve a separating newline before the injected keys: under the manual
    # fallback the block does not end in one, so the last YAML key would glue to
    # ``compression`` without it.
    sep = "" if fm_block.endswith("\n") else "\n"
    inject = f"{sep}compression: {record.compression}\n"
    if record.files:
        inject += "files_affected:\n" + "".join(
            f"- {f}\n" for f in record.files)
    inject += f"synthetic_reason: {reason or 'no_llm'}\n"
    return content[:close] + inject + content[close:]


def _stamp_synthetic_frontmatter(filepath, record: SyntheticRecord,
                                 *, reason: str) -> None:
    """Disk wrapper around :func:`stamp_frontmatter`. Best-effort."""
    try:
        content = Path(filepath).read_text(encoding="utf-8")
        Path(filepath).write_text(
            stamp_frontmatter(content, record, reason=reason),
            encoding="utf-8",
        )
    except Exception:
        return


def _load_signals(slice_text: str):
    """CLI helper: detect signals off the slice when a detector is available;
    return [] otherwise (synthetic_compress degrades to slice-line heuristics)."""
    try:
        from signal_detector import detect_signals
    except Exception:
        return []
    try:
        return detect_signals(slice_text)
    except Exception:
        return []


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(
        description="Synthetic (no-LLM) compression fallback (A5)")
    ap.add_argument("transcript", help="transcript or cascade slice file")
    ap.add_argument("--reason", default="no_llm",
                    help="why the LLM drain was skipped (no_llm|budget|errored)")
    ap.add_argument("--no-write", action="store_true",
                    help="emit the structured record without writing a note")
    args = ap.parse_args()

    p = Path(args.transcript)
    slice_text = p.read_text(encoding="utf-8") if p.exists() else ""
    signals = _load_signals(slice_text)
    record = synthetic_compress(
        slice_text, signals,
        source_path=str(p), reason=args.reason, write=not args.no_write,
    )
    print(json.dumps(record.to_summary()))
    sys.exit(0)


if __name__ == "__main__":
    main()
