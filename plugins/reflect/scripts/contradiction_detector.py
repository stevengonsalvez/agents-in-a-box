#!/usr/bin/env python3
# ABOUTME: Cross-turn contradiction detection (port SG1, agentmemory auto-forget shape).
# ABOUTME: Negation-stripped Jaccard >0.9 + opposite negation polarity => the older learning loses is_latest.
"""Deterministic contradiction detection between learning titles.

Port SG1. reflect-kb used to store "use foo" and "never use foo" as two
independent learnings and rank them by independent recency/confidence —
the agent got contradictory injection. This module is the deterministic
half of belief revision (the LLM-judged half is the S5 ``revise`` flow in
``reflect_cascade.py``): on every new learning write, ``reflect_db``
calls into here to compare the new title against recent in-scope
learnings that share at least one concept tag.

Decision rule (clean-room reimplementation of agentmemory's
auto-forget contradiction pass — concept-index pruning, token-set
Jaccard, older-loses semantics):

1. tokenize both titles; drop stopwords and negation markers;
2. a candidate pair is a contradiction iff
   * Jaccard similarity of the negation-stripped token sets is
     strictly greater than :data:`CONTRADICTION_THRESHOLD`, AND
   * a negation marker is present in exactly ONE of the two texts;
3. the OLDER learning is demoted (``is_latest = 0``) by the caller.

Deviation from the upstream shape (recorded deliberately): agentmemory
computes Jaccard over the RAW token sets, which can never clear 0.9 for
short rules differing only by a negation word ("use foo" vs "never use
foo" is 2/3). We strip negation markers BEFORE the similarity and check
polarity separately, so a pure negation flip of the same rule scores
1.0 and is caught — which is exactly the case this port exists for.

Stdlib-only, no reflect_db import (callable from inside reflect_db
without an import cycle). Pure functions — all persistence lives with
the caller.
"""

from __future__ import annotations

import re
from typing import Optional

# Strictly-greater-than threshold, matching agentmemory's
# ``sim > CONTRADICTION_THRESHOLD`` comparison shape.
CONTRADICTION_THRESHOLD = 0.9

# Newest-N recency cap when scanning candidates (agentmemory caps its
# auto-forget pass at the 1000 most recent latest memories).
CANDIDATE_SCAN_CAP = 1000

# Negation markers: the bead's core trio (not / never / don't) plus the
# obvious n't-contraction family and "cannot". Deliberately NOT including
# bare "no" or "avoid" — far too common in non-negating positions.
_NEGATION_MARKERS = frozenset({
    "not", "never", "cannot",
    "dont", "doesnt", "didnt", "wont", "cant",
    "shouldnt", "wouldnt", "couldnt", "mustnt", "isnt", "arent",
})

# Tiny stopword set so similarity keys on content words, not glue
# (same shape as reflect_cascade._STOPWORDS), plus the auxiliary verbs
# that pair with "not" — "should not deploy" and "deploy" must compare
# as the same rule once the negation is stripped.
_STOPWORDS = frozenset({
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from",
    "has", "have", "here", "in", "into", "is", "it", "its", "of", "on", "or",
    "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "to", "was", "were", "when", "which", "with", "you", "your",
    "can", "could", "did", "do", "does", "must", "should", "will", "would",
})

# Apostrophes are token characters so "don't" survives as one token;
# normalization strips them before the negation-set lookup.
_TOKEN_RE = re.compile(r"[a-z0-9_'+./-]+")


def _raw_tokens(text: str) -> list[str]:
    """Lowercased tokens, apostrophes preserved (so contractions stay whole)."""
    return _TOKEN_RE.findall((text or "").lower())


def _normalize(token: str) -> str:
    """Strip apostrophes: ``don't`` -> ``dont`` for the negation lookup."""
    return token.replace("'", "")


def has_negation(text: str) -> bool:
    """True when *text* carries at least one negation marker."""
    return any(_normalize(tok) in _NEGATION_MARKERS for tok in _raw_tokens(text))


def content_tokens(text: str) -> set[str]:
    """Negation- and stopword-stripped content-word token set.

    These are the tokens Jaccard similarity runs over, so a pure
    negation flip of the same rule compares as identical (1.0).
    """
    out: set[str] = set()
    for tok in _raw_tokens(text):
        norm = _normalize(tok)
        if len(norm) < 2 or norm in _STOPWORDS or norm in _NEGATION_MARKERS:
            continue
        out.add(norm)
    return out


def extract_concepts(text: str) -> set[str]:
    """Concept tags for the concept_index table.

    Same vocabulary as :func:`content_tokens` — concepts exclude
    negation markers on purpose, so "use foo" and "never use foo" land
    in the same concept buckets and become comparison candidates.
    """
    return content_tokens(text)


def jaccard_similarity(a: set[str], b: set[str]) -> float:
    """Plain Jaccard. Two empty sets are 0.0 (nothing shared, nothing known)."""
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def detect_contradiction(
    text_a: str,
    text_b: str,
    *,
    threshold: float = CONTRADICTION_THRESHOLD,
) -> Optional[float]:
    """Return the similarity score when *text_a* contradicts *text_b*.

    A contradiction requires BOTH:
      * negation marker present in exactly one of the two texts, and
      * negation-stripped Jaccard similarity strictly above *threshold*.

    Returns None when the pair is not contradictory (same polarity,
    insufficient overlap, or either side vacuous after stripping).
    """
    if has_negation(text_a) == has_negation(text_b):
        return None
    tokens_a = content_tokens(text_a)
    tokens_b = content_tokens(text_b)
    if not tokens_a or not tokens_b:
        return None
    sim = jaccard_similarity(tokens_a, tokens_b)
    return sim if sim > threshold else None


if __name__ == "__main__":
    import json
    import sys

    if len(sys.argv) != 3:
        print("usage: contradiction_detector.py <text_a> <text_b>", file=sys.stderr)
        sys.exit(2)
    score = detect_contradiction(sys.argv[1], sys.argv[2])
    print(json.dumps({"contradiction": score is not None, "similarity": score}))
    sys.exit(0)
