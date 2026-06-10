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

C1 per-ingest semantic dedup: before a CREATE lands, its text is probed
against existing learnings by embedding cosine (via `reflect embed`, the same
all-mpnet-base-v2 space nano-graphrag indexes with). If the nearest learning
is >= the dedup threshold (default 0.97; env REFLECT_DEDUP_THRESHOLD or
[cascade].dedup_threshold in reflect.toml; >= 1.0 disables), the CREATE is
held and the revise output carries a focused 1-by-1 'merge?' adjudication —
both texts plus the action contract. The drain (the LLM) answers as a final
step: UPDATE the listed id to merge (the new evidence folds into the existing
row), or re-issue the CREATE with "dedup_adjudicated": true to keep both.
The weekly consolidation pass catches dupes after they have been served for
days; this catches them at write. S5's title-overlap recall is lexical and
misses paraphrases — the cosine probe is the semantic backstop.

R13 auto-skill-refresh: a revision UPDATE/DELETE that lands on a learning
backing an installed skill (skill tags overlap the learning's title tokens or
category) marks that skill ``is_stale`` in ``reflect.db.skills`` — stale
skills stop matching in the inject tier (R11 via ``skill_index.match_skills``)
— and enqueues a ``skill_refresh`` task into ``pending_reflections.jsonl``.
The drain consumes the task by re-running the /reflect skill-edit step on the
SKILL.md so the skill catches up with the revised corpus; regeneration (mtime
change) clears the flag. This is the Hindsight
``_trigger_mental_model_refreshes`` shape: belief revision back-reacts on the
curated layer instead of letting promoted skills drift.

O1 consolidated observations: the drain emits a SECOND output stream beside
raw corrections — aggregated persona/convention-shaped observations ("this
team prefers X", "this codebase generally does Y") that accumulate evidence
over time (proof_count + source_correction_ids, the Hindsight
fact_type=observation shape). prepare() lists the scope's existing
observations in the slice with their own CREATE/UPDATE/DELETE contract; the
execution half is the ``observe`` subcommand (execute_observation_actions):
UPDATE folds new correction ids into an existing observation (history
snapshot first), DELETE retires it non-destructively. Retrieval treats
observations as a separate tier — open-domain queries surface them FIRST
(recall_tiered / reflect_db.recall_observation_tier).

O2 auto-refreshing conventions doc: every executed observation action also
back-reacts on the conventions layer — the per-project CONVENTIONS.md
(conventions_generator) aggregating the scope's observations regenerates
inline (``trigger_conventions_refresh``). Same trigger shape as R13, but
pointed at conventions instead of skills, and regeneration happens directly
rather than via a queued drain task because conventions rendering is
deterministic markdown over the observations table — no LLM needed. This is
the Hindsight ``trigger.refresh_after_consolidation`` shape: consolidation
landing on the bank refreshes the mental models built over it.

CLI:
    reflect_cascade.py prepare <transcript.jsonl> [--out SLICE] [--context N]
        -> JSON {action, reason, signal_count, slice_path, orig_tokens,
                 slice_tokens, signal_hash, related_count, observation_count}
    reflect_cascade.py revise [--actions JSON|FILE|-] [--source ID]
        -> JSON {executed, created, updated, deleted, skipped, created_ids,
                 needs_adjudication, adjudications, skills_marked_stale,
                 refreshes_queued, errors}
    reflect_cascade.py observe [--actions JSON|FILE|-] [--scope SCOPE]
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

# O1: consolidated observations second pass
_OBSERVATION_LIMIT = 10           # existing observations listed per drain slice
_OBSERVATION_SCOPE_DEFAULT = "project"

# Tiny stopword set so overlap scoring keys on content words, not glue.
_STOPWORDS = frozenset({
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from",
    "has", "have", "here", "in", "into", "is", "it", "its", "of", "on", "or",
    "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "to", "was", "were", "when", "which", "with", "you", "your",
})

# Statuses a revision must never target again — retired/replaced beliefs
# (including A3 TTL-archived rows from the forget sweep).
_RETIRED_STATUSES = ("reverted", "superseded", "rejected", "archived")

# C1: per-ingest semantic-dedup adjudication (Hindsight consolidator shape).
# A CREATE whose text is >= this cosine to an existing learning is held for a
# focused 1-by-1 merge-or-keep verdict instead of landing as a near-duplicate
# row. 0.97 matches Hindsight's consolidation_dedup_threshold default; a
# threshold >= 1.0 disables the probe entirely (same semantics as upstream).
_DEDUP_THRESHOLD_DEFAULT = 0.97
_DEDUP_SCAN_CAP = 200      # newest learnings embedded per probe (bounded)
_DEDUP_TEXT_CAP = 2000     # chars of each text handed to the embedder
_DEDUP_EMBED_TIMEOUT = 60  # seconds — first call may pay the model load

# R13: queue trigger value for auto-skill-refresh tasks. The drain branches on
# it: skill_refresh entries skip the cascade (a SKILL.md is not a transcript)
# and get the skill-edit prompt instead of the transcript prompt.
SKILL_REFRESH_TRIGGER = "skill_refresh"


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
    observation_count: int = 0     # O1: existing observations embedded for the second pass


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


def dedup_threshold() -> float:
    """C1: resolve the semantic-dedup cosine floor (config-tunable).

    Precedence: REFLECT_DEDUP_THRESHOLD env var > [cascade].dedup_threshold
    in the layered reflect.toml > 0.97 default. A value >= 1.0 disables the
    probe (Hindsight's _dedup_active semantics); unparseable values fall back
    to the default rather than failing the write path.
    """
    import os
    raw: object = os.environ.get("REFLECT_DEDUP_THRESHOLD")
    if raw is None:
        try:
            import reflect_config
            raw = reflect_config.get_config().get("cascade", {}).get(
                "dedup_threshold")
        except Exception:
            raw = None
    if raw is None:
        return _DEDUP_THRESHOLD_DEFAULT
    try:
        return float(raw)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return _DEDUP_THRESHOLD_DEFAULT


def _find_reflect_cli() -> Optional[str]:
    """C1: locate the reflect-kb CLI on $PATH (canonical `uv tool install`).
    None when absent — the probe fail-opens and the CREATE proceeds."""
    import shutil
    return shutil.which("reflect")


def _cosine(a: list[float], b: list[float]) -> float:
    """Cosine similarity. Engine vectors are unit-normalized, but guard the
    norms instead of trusting subprocess output."""
    import math
    dot = norm_a = norm_b = 0.0
    for x, y in zip(a, b):
        dot += x * y
        norm_a += x * x
        norm_b += y * y
    if norm_a <= 0.0 or norm_b <= 0.0:
        return 0.0
    return dot / math.sqrt(norm_a * norm_b)


def _fetch_dedup_embeddings(
    cli: str, anchor_text: str, candidates: list[dict],
    timeout: int = _DEDUP_EMBED_TIMEOUT,
) -> Optional[tuple[list[float], dict[str, list[float]]]]:
    """C1: embed the CREATE text + candidate learning titles via `reflect embed`.

    Same subprocess contract as recall's MMR step — the engine embeds with the
    model nano-graphrag indexes with, so the dedup cosine lives in the index's
    embedding space. Returns (anchor_vector, {learning_id: vector}) or None on
    ANY failure (slim build, legacy CLI, timeout, junk output) — the dedup
    probe is a guard, never a blocker for the write path.
    """
    import subprocess
    payload = json.dumps({
        "candidates": [
            {"id": c["id"], "text": (c["text"] or "")[:_DEDUP_TEXT_CAP]}
            for c in candidates
        ]
    })
    try:
        proc = subprocess.run(
            [cli, "embed", anchor_text[:_DEDUP_TEXT_CAP]],
            input=payload,
            capture_output=True, text=True, timeout=timeout, check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if proc.returncode != 0 or not proc.stdout:
        return None
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict) or not data.get("available"):
        return None
    qvec_raw = data.get("query_embedding")
    docs_raw = data.get("embeddings")
    if not isinstance(qvec_raw, list) or not qvec_raw or not isinstance(docs_raw, dict):
        return None
    try:
        qvec = [float(x) for x in qvec_raw]
        docs = {
            str(key): [float(x) for x in vec]
            for key, vec in docs_raw.items()
            if isinstance(vec, list) and len(vec) == len(qvec_raw)
        }
    except (TypeError, ValueError):
        return None
    return (qvec, docs) if docs else None


def find_semantic_twin(content: str, *,
                       threshold: Optional[float] = None) -> Optional[dict]:
    """C1: probe a CREATE's text against existing learnings by embedding cosine.

    Returns {"id", "title", "similarity"} for the nearest non-retired learning
    at/above the threshold, or None — no near twin, threshold >= 1.0
    (disabled), empty corpus, or any probe failure (fail-open: a missing CLI
    or slim engine must never block the write path).
    """
    text = (content or "").strip()
    if not text:
        return None
    thresh = dedup_threshold() if threshold is None else float(threshold)
    if thresh >= 1.0:
        return None  # disabled — matches Hindsight's _dedup_active contract
    try:
        from reflect_db import get_conn
        rows = get_conn().execute(
            f"""SELECT id, title FROM learnings
                WHERE status NOT IN ({", ".join("?" for _ in _RETIRED_STATUSES)})
                ORDER BY created_at DESC LIMIT ?""",
            (*_RETIRED_STATUSES, _DEDUP_SCAN_CAP),
        ).fetchall()
    except Exception:
        return None
    candidates = [
        {"id": row["id"], "text": row["title"]}
        for row in rows if (row["title"] or "").strip()
    ]
    if not candidates:
        return None
    cli = _find_reflect_cli()
    if not cli:
        return None
    embedded = _fetch_dedup_embeddings(cli, text, candidates)
    if embedded is None:
        return None
    qvec, docs = embedded
    best_id: Optional[str] = None
    best_sim = thresh  # only candidates at/above the threshold qualify
    for cand in candidates:
        vec = docs.get(cand["id"])
        if vec is None:
            continue
        sim = _cosine(qvec, vec)
        if sim >= best_sim:
            best_id, best_sim = cand["id"], sim
    if best_id is None:
        return None
    title = next(c["text"] for c in candidates if c["id"] == best_id)
    return {"id": best_id, "title": title, "similarity": round(best_sim, 4)}


def _merge_adjudication(content: str, twin: dict, threshold: float) -> dict:
    """C1: the focused 1-by-1 'merge?' question handed back to the drain.

    Mirrors Hindsight's adjudication contract — the LLM reads BOTH texts so a
    word-level difference (a number, named entity, negation, or condition) is
    respected — but the adjudicator here is the drain agent itself: it answers
    by re-running revise with either an UPDATE (merge: the new evidence folds
    into the existing learning instead of a duplicate row landing) or the
    original CREATE flagged "dedup_adjudicated": true (keep both).
    """
    return {
        "new_text": content,
        "existing_id": twin["id"],
        "existing_title": twin["title"],
        "similarity": twin["similarity"],
        "threshold": threshold,
        "question": (
            "merge? NEW (new_text) and EXISTING (existing_title) are "
            f">= {threshold:.2f} cosine-similar. If they state the same "
            "rule/fact (wording aside), re-run revise with "
            f'{{"action": "UPDATE", "target_id": "{twin["id"]}", '
            '"reason": "<one sentence>"}} — the new evidence merges into the '
            "existing learning instead of creating a duplicate. If ANY "
            "important detail differs (a number, named entity, negation, or "
            "condition), keep both: re-run the CREATE with "
            '"dedup_adjudicated": true.'
        ),
    }


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
        "- Every action carries a one-sentence reason.\n"
        "- CREATEs are semantically dedup-checked at write time: when the new\n"
        "  text is near-identical (embedding cosine) to an existing learning,\n"
        "  the revise output holds it under 'adjudications' with a 'merge?'\n"
        "  question instead of creating. Answer it as a final step — emit the\n"
        "  UPDATE it names to merge, or re-run the CREATE with\n"
        '  "dedup_adjudicated": true if an important detail genuinely\n'
        "  differs.\n\n"
        "Execute UPDATE/DELETE actions with:\n"
        f"    python3 {script} revise --source {transcript_path} "
        "--actions '<json-array>'\n\n"
        f"{payload}\n"
    )


def recall_scope_observations(scope: str = _OBSERVATION_SCOPE_DEFAULT, *,
                              limit: int = _OBSERVATION_LIMIT) -> list[dict]:
    """O1: existing observations in *scope*, strongest evidence first.

    The candidates the drain's second pass revises against — listing them is
    what makes UPDATE possible (one aggregate accumulating proofs) instead of
    every drain creating a near-duplicate observation. Best-effort: returns
    [] when the DB is unavailable (fail-open mirrors the rest of the cascade).
    """
    try:
        from reflect_db import get_observations
        rows = get_observations(scope=scope, limit=limit)
    except Exception:
        return []
    return [
        {
            "id": row["id"],
            "content": row["content"],
            "category": row["category"],
            "scope": row["scope"],
            "proof_count": row["proof_count"],
        }
        for row in rows
    ]


def _build_observation_block(observations: list[dict], transcript_path: str) -> str:
    """O1: the consolidated-observations section embedded in the drain slice.

    The second-pass contract (Hindsight consolidation shape): after the raw
    revision actions execute, the drain maintains the aggregate layer with
    its own CREATE/UPDATE/DELETE pass. Always appended — even with zero
    existing observations a persona/convention-shaped finding warrants a
    CREATE, and the layer can never bootstrap if the contract is absent.
    """
    script = str(Path(__file__).resolve())
    payload = json.dumps(observations, indent=2, sort_keys=True)
    return (
        "\n\n## Consolidated observations (persona/conventions — second pass)\n"
        "Observations are the aggregate layer over raw corrections:\n"
        "persona/convention-shaped statements ('this team prefers X', 'this\n"
        "codebase generally does Y') that accumulate evidence as proof_count +\n"
        "source_correction_ids. They are NOT corrections (one specific\n"
        "rule/fix each) and NOT skills (workflow-shaped: how to do X).\n\n"
        "AFTER executing the revision actions above, run one more pass: for\n"
        "every persona/convention-shaped finding in this session, emit exactly\n"
        "one observation action:\n\n"
        '    {"action": "CREATE"|"UPDATE"|"DELETE", "target_id": "<obs id>",\n'
        '     "content": "<aggregate statement>",\n'
        '     "source_correction_ids": ["<learning ids>"],\n'
        '     "reason": "<one sentence>"}\n\n'
        "Rules:\n"
        "- PREFER UPDATE OVER CREATE: if a finding restates a listed\n"
        "  observation, emit UPDATE for that id — the evidence folds in\n"
        "  (proof_count grows by the NEW correction ids, ids append uniquely,\n"
        "  a history snapshot preserves the prior form) instead of a\n"
        "  near-duplicate aggregate landing.\n"
        "- source_correction_ids: cite the learning ids the evidence comes\n"
        "  from — ids listed under related learnings above and the\n"
        "  'created_ids' returned by the revise command.\n"
        "- UPDATE may also rewrite 'content' when the aggregate wording should\n"
        "  evolve; the old wording stays in observation_history.\n"
        "- DELETE only when the convention demonstrably no longer holds\n"
        "  (retires it non-destructively). Be very conservative.\n"
        "- Workflow-shaped findings (how to do X) belong to skills, and\n"
        "  one-off rules stay raw corrections — do NOT mirror every learning\n"
        "  as an observation.\n\n"
        "Execute observation actions with:\n"
        f"    python3 {script} observe --actions '<json-array>'\n\n"
        f"Existing observations in scope:\n{payload}\n"
    )


def trigger_conventions_refresh(scopes) -> int:
    """O2: regenerate the conventions doc(s) covering *scopes*.

    The R13 trigger shape pointed at the conventions layer: observation
    actions landing in a scope refresh every registered CONVENTIONS.md that
    aggregates it (and bootstrap one for a brand-new scope). Unlike skill
    refreshes — which queue a drain task because rewriting a SKILL.md needs
    the LLM — conventions regeneration is deterministic markdown over the
    observations table, so it happens inline. Returns the number of docs
    regenerated. Best-effort end to end: a refresh failure must never fail
    the observation write itself.
    """
    refreshed = 0
    for scope in sorted({str(s or "").strip() for s in (scopes or [])} - {""}):
        try:
            import conventions_generator
            refreshed += conventions_generator.refresh_for_scope(scope)
        except Exception:
            continue
    return refreshed


def execute_observation_actions(actions, *,
                                scope: str = _OBSERVATION_SCOPE_DEFAULT) -> dict:
    """O1: apply structured CREATE/UPDATE/DELETE actions to observations.

    The second-pass executor (Hindsight consolidator action shape applied to
    the aggregate layer):

    - CREATE  -> new observation row; proof_count starts at the number of
                 cited source_correction_ids (floor 1)
    - UPDATE  -> evidence merge via add_observation_evidence: new correction
                 ids append uniquely, proof_count grows by the NEW ids, an
                 optional content rewrite lands, and a history snapshot
                 fires first (S6) — re-citing known ids is an idempotent skip
    - DELETE  -> non-destructive retire (status -> 'retired' + reason)

    O2: every executed action back-reacts on the conventions layer — the
    CONVENTIONS.md docs aggregating the touched scopes regenerate inline
    (``conventions_refreshed`` counts them; see
    :func:`trigger_conventions_refresh`).

    Per-action failures are collected in ``errors`` — one malformed action
    never blocks the rest of the batch (same contract as
    :func:`execute_revision_actions`).
    """
    summary = {"executed": 0, "created": 0, "updated": 0, "deleted": 0,
               "skipped": 0, "conventions_refreshed": 0, "errors": []}
    try:
        import reflect_db
    except Exception as exc:  # pragma: no cover - import environment broken
        summary["errors"].append(f"observations DB unavailable: {exc}")
        return summary

    scopes_touched: set[str] = set()
    for raw in actions or []:
        if not isinstance(raw, dict):
            summary["skipped"] += 1
            summary["errors"].append(f"not an action object: {raw!r}")
            continue
        action = str(raw.get("action", "")).strip().upper()
        target = str(raw.get("target_id", "") or "").strip()
        content = str(raw.get("content", "") or "").strip()
        reason = str(raw.get("reason", "") or "").strip()
        correction_ids = raw.get("source_correction_ids")
        try:
            if action == "UPDATE":
                if not target:
                    summary["skipped"] += 1
                    summary["errors"].append("UPDATE missing target_id")
                    continue
                row = reflect_db.get_observation(target)
                if row is None:
                    summary["skipped"] += 1
                    summary["errors"].append(
                        f"UPDATE {target}: observation not found")
                    continue
                if reflect_db.add_observation_evidence(
                    target, correction_ids, content=content or None,
                ):
                    summary["updated"] += 1
                    scopes_touched.add(str(row.get("scope") or scope))
                else:
                    # Idempotent: every cited correction already recorded.
                    summary["skipped"] += 1
            elif action == "DELETE":
                if not target:
                    summary["skipped"] += 1
                    summary["errors"].append("DELETE missing target_id")
                    continue
                row = reflect_db.get_observation(target)
                if row is None:
                    summary["skipped"] += 1
                    summary["errors"].append(
                        f"DELETE {target}: observation not found")
                    continue
                if reflect_db.retire_observation(
                    target,
                    reason=reason or "observation no longer holds",
                ):
                    summary["deleted"] += 1
                    scopes_touched.add(str(row.get("scope") or scope))
                else:
                    summary["skipped"] += 1  # already retired — idempotent
            elif action == "CREATE":
                if not content:
                    summary["skipped"] += 1
                    summary["errors"].append("CREATE missing content")
                    continue
                effective_scope = str(raw.get("scope", "") or scope)
                reflect_db.add_observation(
                    content,
                    category=str(raw.get("category", "") or "Unknown"),
                    scope=effective_scope,
                    source_correction_ids=correction_ids,
                )
                summary["created"] += 1
                scopes_touched.add(effective_scope)
            else:
                summary["skipped"] += 1
                summary["errors"].append(f"unknown action: {action or '<empty>'}")
        except Exception as exc:
            summary["errors"].append(f"{action} {target or content[:40]}: {exc}")
    summary["executed"] = summary["created"] + summary["updated"] + summary["deleted"]
    # O2: observation changes back-react on the conventions layer — the docs
    # aggregating the touched scopes regenerate inline (best-effort).
    if scopes_touched:
        summary["conventions_refreshed"] = trigger_conventions_refresh(scopes_touched)
    return summary


class _QuerySignal:
    """Free-text query shaped like a detector signal so the S5 overlap
    scorer (:func:`recall_related_learnings`) can rank learnings against it."""

    def __init__(self, text: str):
        self.signal = text
        self.source_quote = ""
        self.line_number = 1


def recall_tiered(query: str, *, scope: str = _OBSERVATION_SCOPE_DEFAULT,
                  limit: int = _RELATED_LIMIT) -> list[dict]:
    """O1 retrieval composition: observation tier FIRST, raw learnings after.

    Open-domain queries ('what conventions does this codebase use?', 'what
    does this team prefer?') get the pre-aggregated observation tier ahead
    of correction-shaped learnings — the agent reads one proof-ranked
    aggregate instead of folding 5 raw corrections in-context. Closed-domain
    queries skip the tier entirely (it returns []) and behave exactly as
    before. Entries carry ``tier`` ('observation' | 'learning'). Best-effort:
    a missing DB yields [] for either tier, never an exception.
    """
    observations: list[dict] = []
    try:
        from reflect_db import recall_observation_tier
        observations = recall_observation_tier(query, scope=scope, limit=limit)
    except Exception:
        observations = []
    learnings = (
        recall_related_learnings([_QuerySignal(query)], limit=limit)
        if (query or "").strip() else []
    )
    for entry in learnings:
        entry["tier"] = "learning"
    return observations + learnings


def skills_backing_learning(learning: dict) -> list[dict]:
    """R13: indexed skills whose tags overlap *learning*'s tags/category.

    A skill is "backed by" the learning when any of its tags either equals
    the learning's category (case-insensitive) or has ALL of its content
    tokens present in the learning's title — a multi-word tag must match
    whole, so the tag "belief revision" doesn't fire on every learning that
    merely says "revision". Deterministic, stdlib-only, and best-effort:
    returns [] when the skills index is unavailable.
    """
    title_tokens = _content_tokens(str(learning.get("title", "") or ""))
    category = str(learning.get("category", "") or "").strip().lower()
    if not title_tokens and not category:
        return []
    try:
        from reflect_db import get_skills
        skills = get_skills()
    except Exception:
        return []
    hits: list[dict] = []
    for skill in skills:
        for tag in skill.get("tags") or []:
            tag = str(tag).strip().lower()
            if not tag:
                continue
            if category and tag == category:
                hits.append(skill)
                break
            tag_tokens = _content_tokens(tag)
            if tag_tokens and tag_tokens <= title_tokens:
                hits.append(skill)
                break
    return hits


def _skill_refresh_queue_file() -> Path:
    """The drain's pending-reflections queue (REFLECT_STATE_DIR-aware)."""
    import os
    state = os.environ.get("REFLECT_STATE_DIR", "")
    base = Path(state).expanduser() if state else Path.home() / ".reflect"
    return base / "pending_reflections.jsonl"


def _skill_refresh_already_queued(qfile: Path, skill_path: str) -> bool:
    """True when a refresh task for *skill_path* is already pending.

    Fail-closed on read errors (returns False → enqueue anyway): a duplicate
    refresh is cheap, a silently dropped one leaves the skill stale forever.
    """
    if not qfile.exists():
        return False
    try:
        with open(qfile, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    entry = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if (
                    entry.get("trigger") == SKILL_REFRESH_TRIGGER
                    and entry.get("transcript_path") == skill_path
                ):
                    return True
    except OSError:
        return False
    return False


def enqueue_skill_refresh(skill: dict, *, learning_id: str = "",
                          reason: str = "") -> bool:
    """R13: append a ``skill_refresh`` task to ``pending_reflections.jsonl``.

    ``transcript_path`` carries the SKILL.md path on purpose — the drain
    keys ALL its queue mechanics (existence check, retry counters, rewrite,
    poison) on that field, so a refresh task rides the existing machinery;
    the ``trigger`` discriminator switches the drain to the skill-edit
    prompt. Dedup: at most one pending refresh per skill path.
    """
    skill_path = str(skill.get("path", "") or "")
    if not skill_path:
        return False
    qfile = _skill_refresh_queue_file()
    if _skill_refresh_already_queued(qfile, skill_path):
        return False
    from datetime import datetime
    entry = {
        "ts": datetime.now().isoformat(),
        "session_id": "skill-refresh",
        "transcript_path": skill_path,
        "trigger": SKILL_REFRESH_TRIGGER,
        "skill_name": str(skill.get("name", "") or ""),
        "learning_id": learning_id,
        "reason": reason,
    }
    qfile.parent.mkdir(parents=True, exist_ok=True)
    with open(qfile, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(entry) + "\n")
    return True


def trigger_skill_refreshes(learning: dict, *, action: str = "",
                            reason: str = "") -> dict:
    """R13: after a revision lands on *learning*, refresh the skills it backs.

    Marks every overlapping skill ``is_stale`` (drops it out of the inject
    matcher immediately) and enqueues one ``skill_refresh`` drain task per
    skill. Returns ``{"skills_marked_stale": N, "refreshes_queued": M}``.
    Best-effort end to end: any failure returns zero counts — the back-
    reaction loop must never break the revision write path itself.
    """
    summary = {"skills_marked_stale": 0, "refreshes_queued": 0}
    try:
        backing = skills_backing_learning(learning)
        if not backing:
            return summary
        from reflect_db import mark_skills_stale
        summary["skills_marked_stale"] = mark_skills_stale(
            [skill["path"] for skill in backing]
        )
        learning_id = str(learning.get("id", "") or "")
        why = reason or f"learning revised ({action.lower() or 'update'})"
        for skill in backing:
            if enqueue_skill_refresh(
                skill, learning_id=learning_id, reason=why,
            ):
                summary["refreshes_queued"] += 1
    except Exception:
        pass
    return summary


def execute_revision_actions(actions, *, source_memory_id: str = "") -> dict:
    """S5: apply structured CREATE/UPDATE/DELETE actions to the learnings DB.

    - CREATE  -> new learning row (proof_count starts at 1, S4 semantics)
    - UPDATE  -> evidence merge via add_learning_proof: proof_count++ +
                 source appended + history snapshot (S6 fires inside)
    - DELETE  -> non-destructive retire: status -> 'reverted' with the
                 action's reason (history snapshot fires inside the status
                 transition). Hindsight hard-deletes; our ledger keeps the
                 row so 'why was this retired?' stays answerable.

    C1: every CREATE is first probed against existing learnings by embedding
    cosine. At/above the dedup threshold the row does NOT land — the CREATE
    is held in ``adjudications`` with both texts and a focused 'merge?'
    question, and ``needs_adjudication`` counts the holds. The caller (the
    drain LLM) answers as a final step: UPDATE the twin to merge, or re-run
    the CREATE with ``"dedup_adjudicated": true`` to keep both. The probe
    fail-opens (no CLI / slim engine / timeout -> plain CREATE).

    R13: every executed UPDATE/DELETE back-reacts on the skills index —
    skills whose tags overlap the revised learning are flagged stale
    (``skills_marked_stale``) and a ``skill_refresh`` drain task is queued
    per skill (``refreshes_queued``). Best-effort: a refresh-trigger
    failure never fails the revision itself.

    Per-action failures are collected in ``errors`` — one malformed action
    never blocks the rest of the batch.
    """
    summary = {"executed": 0, "created": 0, "updated": 0, "deleted": 0,
               "skipped": 0, "created_ids": [],
               "needs_adjudication": 0, "adjudications": [],
               "skills_marked_stale": 0, "refreshes_queued": 0,
               "errors": []}
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
                row = reflect_db.get_learning(target)
                if row is None:
                    summary["skipped"] += 1
                    summary["errors"].append(f"UPDATE {target}: learning not found")
                    continue
                if reflect_db.add_learning_proof(target, sid):
                    summary["updated"] += 1
                    # R13: an updated learning may back a promoted skill —
                    # mark those skills stale + queue their regeneration.
                    refreshed = trigger_skill_refreshes(
                        row, action="UPDATE", reason=reason,
                    )
                    summary["skills_marked_stale"] += refreshed["skills_marked_stale"]
                    summary["refreshes_queued"] += refreshed["refreshes_queued"]
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
                # R13: retiring a learning invalidates skills built on it.
                refreshed = trigger_skill_refreshes(
                    row, action="DELETE", reason=reason,
                )
                summary["skills_marked_stale"] += refreshed["skills_marked_stale"]
                summary["refreshes_queued"] += refreshed["refreshes_queued"]
            elif action == "CREATE":
                if not content:
                    summary["skipped"] += 1
                    summary["errors"].append("CREATE missing content")
                    continue
                # C1: per-ingest semantic dedup — hold near-duplicate CREATEs
                # for a focused merge-or-keep verdict instead of letting both
                # rows land. "dedup_adjudicated" is the keep verdict (the
                # adjudicator already read both texts and ruled them distinct).
                if not raw.get("dedup_adjudicated"):
                    twin = find_semantic_twin(content)
                    if twin is not None:
                        summary["needs_adjudication"] += 1
                        summary["adjudications"].append(
                            _merge_adjudication(content, twin, dedup_threshold())
                        )
                        continue
                # A3: the drain may flag clearly time-bounded knowledge
                # (incident workaround, sprint/migration scope) with an
                # optional forget_after ISO timestamp; the hourly forget
                # sweep archives the row once it passes. Absent = permanent.
                forget_after = str(raw.get("forget_after", "") or "").strip()
                created_id = reflect_db.add_learning(
                    title=content[:200],
                    category=str(raw.get("category", "") or "Unknown"),
                    confidence=str(raw.get("confidence", "") or "MEDIUM"),
                    content_hash=str(raw.get("content_hash", "") or ""),
                    source_memory_ids=[sid] if sid else None,
                    forget_after=forget_after or None,
                )
                summary["created"] += 1
                # O1: surface the new id so the drain's observation second
                # pass can cite it in source_correction_ids.
                summary["created_ids"].append(created_id)
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
    # O1: the consolidated-observations second pass rides every drain — the
    # block carries the action contract plus the scope's existing
    # observations (DB-vetted, like the revision block; safe post-filter).
    observations = recall_scope_observations()
    prep.observation_count = len(observations)
    body += _build_observation_block(observations, str(p))
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
    ob = sub.add_parser("observe")
    ob.add_argument(
        "--actions", default="-",
        help="JSON array of observation actions, a path to a JSON file, or '-' for stdin",
    )
    ob.add_argument(
        "--scope", default=_OBSERVATION_SCOPE_DEFAULT,
        help="scope new observations land in when the action omits one",
    )
    args = ap.parse_args()

    if args.cmd == "prepare":
        prep = prepare(args.transcript, context_lines=args.context, out_path=args.out)
        print(json.dumps(asdict(prep)))
        # exit 0 = reflect (slice ready), 1 = skip
        sys.exit(0 if prep.action == "reflect" else 1)

    if args.cmd in ("revise", "observe"):
        raw = args.actions
        if raw == "-":
            raw = sys.stdin.read()
        elif Path(raw).is_file():
            raw = Path(raw).read_text(encoding="utf-8")
        try:
            parsed = json.loads(raw)
        except (json.JSONDecodeError, TypeError) as exc:
            error = f"invalid actions JSON: {exc}"
            if args.cmd == "revise":
                print(json.dumps({"executed": 0, "created": 0, "updated": 0,
                                  "deleted": 0, "skipped": 0, "created_ids": [],
                                  "needs_adjudication": 0, "adjudications": [],
                                  "skills_marked_stale": 0, "refreshes_queued": 0,
                                  "errors": [error]}))
            else:
                print(json.dumps({"executed": 0, "created": 0, "updated": 0,
                                  "deleted": 0, "skipped": 0,
                                  "conventions_refreshed": 0,
                                  "errors": [error]}))
            sys.exit(1)
        if isinstance(parsed, dict):
            parsed = parsed.get("actions", [parsed] if parsed.get("action") else [])
        if args.cmd == "revise":
            summary = execute_revision_actions(parsed, source_memory_id=args.source)
        else:
            summary = execute_observation_actions(parsed, scope=args.scope)
        print(json.dumps(summary))
        # exit 0 = clean run, 1 = at least one action failed/was malformed
        sys.exit(0 if not summary["errors"] else 1)


if __name__ == "__main__":
    main()
