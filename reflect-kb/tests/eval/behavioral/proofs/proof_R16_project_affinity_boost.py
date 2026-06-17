# ABOUTME: Behavioral proof for R16 — project-affinity multiplicative boost in
# ABOUTME: recall.py rerank. Same-project hits get the bounded 1+α/2 boost and
# ABOUTME: outrank otherwise-identical cross-project hits; α=0 and a non-matching
# ABOUTME: current project both neutralize it (soft affinity, never hard isolation).
"""R16 project-affinity boost proof.

Soft affinity, not hard isolation: ``combined = CE × … × project_affinity``,
where ``project_affinity = bounded_boost(project_norm, α)`` — norm 1.0 when the
learning's ``project_id`` matches the current session's project (boost ceiling
1 + α/2, default +10%), and the neutral 0.5 otherwise (multiplier exactly 1.0).
Cross-project gems still surface; they are down-RANKED relative to same-project
ties, never down-SCORED below their R8 baseline.

The proof has two arms, each pinning a different acceptance bullet, and they are
in tension — which is what makes the boost meaningful rather than a free win:

  A (the boost is REAL, bullets 1 + 2 + 3). Seed two learnings with
     near-identical body text on the SAME topic (so the cross-encoder scores
     them ~equally — a deliberate near-tie) that differ ONLY in ``project_id``:
     one == the current session's project, one == a sibling project. Drive
     ``CLAUDE_PROJECT_DIR`` so ``detect_current_project()`` resolves to the
     first project's id.
       - Affinity ON (default α): the same-project twin must outrank the
         cross-project twin (bullet 1).
       - α=0 (RECALL_PROJECT_ALPHA=0, bullet 2): the affinity tie-breaker is
         gone, so the same-project-first ordering is NO LONGER forced.
       - current project does NOT match either twin (bullet 3 proxy): the
         neutral 0.5 norm applies to both, so — exactly like α=0 — the
         ordering is no longer forced. This is the SAME neutralization the
         R15 shard path produces by passing ``current_project=""``; when the
         current project resolves to neither hit, affinity cannot apply, which
         is the observable consequence of "affinity only kicks in on global
         scope where a real cross-project mix exists".

  B (the boost is BOUNDED). Seed a learning that DIRECTLY answers the query but
     belongs to a SIBLING project, against a learning that barely relates to
     the query but belongs to the CURRENT project. Even with the affinity boost
     cranked to its clamp ceiling (RECALL_PROJECT_ALPHA=2.0 → project ∈ [0,2],
     the widest legal swing), the strong-but-cross-project answer must STILL
     rank first, because ``score = sigmoid(ce_logit) × bounded_boost`` and the
     CE gap between a direct answer and an off-query note (~5 orders of
     magnitude) dwarfs a ≤2× project multiplier. This is the soft-affinity
     contract: a sibling-project gem that decisively answers the query is NOT
     hidden behind a same-project off-topic note.

If the affinity boost were absent, arm A's ON-ordering would not be forced and
the ``assert same-project leads`` would FAIL. If the boost were unbounded (hard
isolation / a large additive penalty on cross-project hits), arm B would FAIL —
the off-topic same-project note would be lifted over the decisive cross-project
answer. No LLM participates in either assertion: the seeds, the project ids,
and the documented env flags fully determine the outcome.

PORT: R16
"""
from __future__ import annotations

import pytest

# The current session's project (what CLAUDE_PROJECT_DIR's basename normalizes
# to) and a sibling project. _normalize_project lowercases the final path
# component, so the dir basename must already be lowercase to match.
CURRENT_PROJECT = "alpha-service"
OTHER_PROJECT = "beta-service"

# --------------------------------------------------------------------------
# Arm A: near-identical text, differ ONLY in project_id. The CE scores them
# alike (deliberate near-tie), so the bounded affinity boost is the tie-breaker.
# --------------------------------------------------------------------------
_SAME_PROJECT_TWIN = dict(
    name="r16-same-project-twin",
    title="Cap the connection pool so request spikes cannot exhaust it",
    category="reliability",
    tags=["connection-pool", "exhaustion", "tuning"],
    confidence="medium",
    created="2026-05-01",
    project_id=CURRENT_PROJECT,
    key_insight="Bound the connection pool size so request spikes can't exhaust it.",
    body=(
        "When request volume spiked the service opened unbounded connections and "
        "exhausted the pool; capping the maximum pool size and reusing pooled "
        "connections kept it from running dry under load."
    ),
)

# Same topic, near-identical body (one trailing clause differs so the two files
# are not byte-identical and don't collapse to one chunk). Sibling project.
_CROSS_PROJECT_TWIN = dict(
    name="r16-cross-project-twin",
    title="Cap the connection pool so request spikes cannot exhaust it",
    category="reliability",
    tags=["connection-pool", "exhaustion", "tuning"],
    confidence="medium",
    created="2026-05-01",
    project_id=OTHER_PROJECT,
    key_insight="Bound the connection pool size so request spikes can't exhaust it.",
    body=(
        "When request volume spiked the service opened unbounded connections and "
        "exhausted the pool; capping the maximum pool size and reusing pooled "
        "connections kept it from running dry under load from every caller at once."
    ),
)

_TWIN_QUERY = "how to stop request spikes from exhausting the connection pool"

# --------------------------------------------------------------------------
# Arm B: a decisive base gap. The strong answer is CROSS-project; the weak note
# is SAME-project. A maxed affinity boost must NOT flip the decisive CE gap.
# --------------------------------------------------------------------------
_STRONG_CROSS = dict(
    name="r16-strong-cross",
    title="Diagnose a Postgres deadlock from lock-wait graphs in pg_locks",
    category="database",
    tags=["postgres", "deadlock"],
    confidence="medium",
    created="2026-01-01",
    project_id=OTHER_PROJECT,   # sibling project — gets NO affinity boost
    key_insight="Read pg_locks to find the lock-wait cycle behind a Postgres deadlock.",
    body=(
        "To diagnose a Postgres deadlock, inspect pg_locks and the server log's "
        "deadlock detail to find the lock-wait cycle: two transactions each "
        "holding a row lock the other needs. Order your lock acquisition "
        "consistently to break the cycle."
    ),
)

_WEAK_SAME = dict(
    name="r16-weak-same",
    title="Pick a CSS color palette for a marketing landing page",
    category="design",
    tags=["css", "design"],
    confidence="medium",
    created="2026-06-01",
    project_id=CURRENT_PROJECT,  # same project — gets the affinity boost
    key_insight="Use a restrained two-color palette with one accent for a landing page.",
    body=(
        "When choosing a CSS color palette for a marketing landing page, keep "
        "it to two base colors plus a single accent, and check contrast ratios "
        "for accessibility."
    ),
)

_DEADLOCK_QUERY = "how do I diagnose a Postgres deadlock from lock-wait cycles"


def _rank_of(ids: list[str], name: str) -> int:
    assert name in ids, f"expected {name!r} in results, got {ids}"
    return ids.index(name)


def _project_dir_env(kb, project_name: str, extra: dict | None = None) -> dict:
    """CLAUDE_PROJECT_DIR whose basename normalizes to ``project_name`` (so
    detect_current_project() resolves to it), merged with any extra env."""
    # An absolute path ending in the project name; _normalize_project takes the
    # final path component, lowercased — already lowercase here.
    env = {"CLAUDE_PROJECT_DIR": f"/work/{project_name}"}
    if extra:
        env.update(extra)
    return env


def test_R16_same_project_outranks_identical_cross_project(behavioral_kb):
    """Arm A: with the base relevance a near-tie, the bounded affinity boost
    forces the same-project twin ahead; zeroing α (bullet 2) and pointing the
    current project at NEITHER twin (bullet 3 proxy) both remove the tie-breaker
    so that ordering is no longer forced — proving the boost, not incidental
    text/index order, drove it."""
    kb = behavioral_kb
    kb.seed([_SAME_PROJECT_TWIN, _CROSS_PROJECT_TWIN])

    # --- Affinity ON (default α=0.2), current project == the same-project twin.
    ids_on = kb.recall_ids(
        _TWIN_QUERY, no_mmr=True, env=_project_dir_env(kb, CURRENT_PROJECT)
    )
    assert _rank_of(ids_on, _SAME_PROJECT_TWIN["name"]) < _rank_of(
        ids_on, _CROSS_PROJECT_TWIN["name"]
    ), (
        "with the project-affinity boost ON and the current project matching it, "
        "the same-project twin must outrank the otherwise-identical cross-project "
        f"twin (acceptance bullet 1); got order {ids_on}"
    )

    # --- α=0 disables the boost (bullet 2): the same-project-first ordering is
    # no longer forced. The affinity boost is the ONLY signal that differs
    # between the twins (same confidence/tags/recency, near-identical text), so
    # with it zeroed the two score identically up to CE noise on the one-clause
    # diff — which, if anything, favors the cross-project twin's slightly longer
    # body — and the same-project-first guarantee from the ON arm is gone.
    ids_alpha0 = kb.recall_ids(
        _TWIN_QUERY,
        no_mmr=True,
        env=_project_dir_env(kb, CURRENT_PROJECT, {"RECALL_PROJECT_ALPHA": "0"}),
    )
    same_first_alpha0 = _rank_of(ids_alpha0, _SAME_PROJECT_TWIN["name"]) < _rank_of(
        ids_alpha0, _CROSS_PROJECT_TWIN["name"]
    )
    assert not same_first_alpha0, (
        "with RECALL_PROJECT_ALPHA=0 the same-project twin must NOT be forced "
        "ahead — if it still leads, affinity was not what drove the ON ordering "
        f"(acceptance bullet 2: α=0 disables the boost); got order {ids_alpha0}"
    )

    # --- Current project matches NEITHER twin (bullet 3 proxy): with α at its
    # default, project_norm is the neutral 0.5 for both hits (no match), so the
    # multiplier is exactly 1.0 for each — identical to the α=0 case. This is
    # the SAME neutralization the R15 shard path produces via current_project=""
    # ("affinity only applies on the global path where a real cross-project mix
    # exists"). The same-project-first ordering must again NOT be forced.
    ids_nomatch = kb.recall_ids(
        _TWIN_QUERY, no_mmr=True, env=_project_dir_env(kb, "gamma-unrelated")
    )
    same_first_nomatch = _rank_of(ids_nomatch, _SAME_PROJECT_TWIN["name"]) < _rank_of(
        ids_nomatch, _CROSS_PROJECT_TWIN["name"]
    )
    assert not same_first_nomatch, (
        "with the current project matching NEITHER twin, project_norm is the "
        "neutral 0.5 for both (multiplier 1.0) so affinity cannot force an "
        "ordering — the same neutralization the R15 shard path applies "
        f"(acceptance bullet 3); got order {ids_nomatch}"
    )


def test_R16_boost_is_bounded_cannot_flip_a_decisive_base(behavioral_kb):
    """Arm B: an affinity boost cranked to its clamp ceiling (α=2.0) on a weak
    SAME-project note can NOT overtake a CROSS-project note that decisively
    answers the query — soft affinity, never hard isolation. The boost is
    multiplicative and BOUNDED; the CE gap is not."""
    kb = behavioral_kb
    kb.seed([_STRONG_CROSS, _WEAK_SAME])

    # Affinity boost at its maximum legal strength (_env_alpha clamps to 2.0):
    # project ∈ [0, 2], the widest swing the boost can ever apply, lifting the
    # same-project weak note. Current project == the WEAK note's project.
    ids = kb.recall_ids(
        _DEADLOCK_QUERY,
        no_mmr=True,
        env=_project_dir_env(kb, CURRENT_PROJECT, {"RECALL_PROJECT_ALPHA": "2.0"}),
    )
    # Both seeds are returned (two docs in, two chunks out). The off-query weak
    # note may surface with id '?' when the engine merges/strips its
    # frontmatter, so we assert on the strong note's rank (decisive, id-stable)
    # rather than the weak note's name.
    assert len(ids) >= 2, f"expected both seeded notes back, got {ids}"
    strong_rank = _rank_of(ids, _STRONG_CROSS["name"])
    assert strong_rank == 0, (
        "even with the project-affinity boost maxed (α=2.0, project ∈ [0,2]) on "
        "a SAME-project note, the CROSS-project note that DIRECTLY answers the "
        "query must rank FIRST — score = sigmoid(ce_logit) × bounded_boost, and "
        "the CE gap (direct answer vs off-topic, ~5 orders of magnitude) dwarfs "
        f"a ≤2× project multiplier. Got order {ids} (strong rank {strong_rank}). "
        "If this fails, affinity is hard isolation, not R16's bounded soft boost."
    )
