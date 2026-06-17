# ABOUTME: Behavioral proof for R12 — per-arm calibrated OOD thresholds in recall.py.
# ABOUTME: One arm's own min_score floor — not the global gate — decides whether a borderline
# ABOUTME: hit from that arm survives: raise the arm's floor and it drops; lower it and it returns.
"""R12 per-arm calibrated thresholds proof.

Invariant: each retrieval arm has its OWN OOD floor (config
``recall.arm.<name>.min_score`` / env ``RECALL_ARM_<NAME>_MIN_SCORE``), applied
to that arm's candidates BEFORE RRF fusion. The arms' native scores are not
comparable (vector cosine, BM25 score, graph budget live on different scales),
so the single GLOBAL gate (R7, post-fusion ``--min-overlap``) mis-calibrates at
least three of them. R12 layers a per-arm floor underneath R7. This proof shows
the per-arm knob is DECISIVE: a borderline hit that clears the global gate is
dropped purely by raising its arm's floor, and the SAME hit returns when only
that arm's floor is lowered — with the global gate held OFF in both runs, so
nothing but the per-arm threshold can explain the difference.

Setup (one arm, fresh KB). The behavioral harness empties XDG_CACHE_HOME, so the
BM25/qmd arm is dark and the active arm is the VECTOR arm (the primary ``naive``
mode's ``reflect search``). We seed two notes for a redis-connection-pool query:

  - STRONG: states the full answer (configure redis connection pool timeout and
    max retries for a flaky upstream) — query-term coverage 1.0, well clear of
    any floor we set.
  - BORDERLINE: mentions only "redis" + "timeout" — coverage ≈ 0.22. It clears
    the OFF global gate, so without R12 it survives into the result set.

Two runs, global gate held OFF (``min_overlap=0.0``) in BOTH so R7 is never the
cause:

  1. ``RECALL_ARM_VECTOR_MIN_SCORE=0.5`` — above the borderline's 0.22 coverage,
     below the strong note's 1.0. The vector arm drops the borderline note
     before fusion; STRONG survives.
  2. ``RECALL_ARM_VECTOR_MIN_SCORE=0.0`` — the arm's floor is disabled (pre-R12
     behaviour). The borderline note returns.

If a SINGLE global threshold governed every arm (no R12), run 1 could not drop
the borderline note without also raising ``--min-overlap`` — but here
``min_overlap`` is 0 in both runs, so the per-arm knob is the only thing that
changed, and it is what flips the borderline note's fate. That is the per-arm
calibration the port adds.

No LLM participates in the assertion: the seeds, the documented per-arm env
flag, and the lexical-coverage gate fully determine the outcome. The engine's
embedding ranking only decides ORDER among returned hits; the assertion is on
INCLUSION/EXCLUSION of the borderline note, which the per-arm floor — pure
stdlib query-term coverage — controls deterministically.

PORT: R12
"""
from __future__ import annotations

# Query whose content terms are: configure, connection, flaky, max, pool, redis,
# retries, timeout, upstream.
_QUERY = (
    "how do I configure redis connection pool timeout and max retries "
    "for a flaky upstream"
)

# Coverage 1.0 — every query term appears. Clears any floor we set.
_STRONG = dict(
    name="r12-strong",
    title="Configure redis connection pool timeout and max retries",
    category="reliability",
    tags=["redis", "pool", "timeout"],
    confidence="medium",
    created="2026-06-01",
    key_insight="Set the redis pool timeout and retry budget explicitly for flaky upstreams.",
    body=(
        "Configure the redis connection pool timeout and max retries so a flaky "
        "upstream does not exhaust connections; set pool timeout and retry "
        "budget explicitly rather than relying on the library defaults."
    ),
)

# Coverage ≈ 0.22 — mentions only redis + timeout. Clears the OFF global gate,
# so it survives WITHOUT a per-arm floor; a per-arm floor of 0.5 drops it.
_BORDERLINE = dict(
    name="r12-borderline",
    title="Redis default timeout note",
    category="reliability",
    tags=["redis"],
    confidence="medium",
    created="2026-06-01",
    key_insight="Raise the default redis timeout on slow networks.",
    body="Redis has a default timeout you may want to raise for slow networks.",
)

# A per-arm floor between the borderline coverage (0.22) and the strong note's
# (1.0). Above 0.22 → drops the borderline note; below 1.0 → keeps the strong.
_HIGH_FLOOR = "0.5"


def test_R12_per_arm_floor_gates_one_arms_borderline_hit(behavioral_kb):
    """A high per-arm VECTOR floor drops a borderline vector-arm hit that the
    OFF global gate would have passed; lowering ONLY that arm's floor (global
    gate still OFF) brings the same hit back — so the per-arm knob, not the
    global one, is decisive."""
    kb = behavioral_kb
    kb.seed([_STRONG, _BORDERLINE])

    # Run 1: per-arm vector floor HIGH, global gate OFF. Borderline dropped.
    ids_high = kb.recall_ids(
        _QUERY,
        no_mmr=True,
        min_overlap=0.0,  # global R7 gate OFF in BOTH runs
        env={"RECALL_ARM_VECTOR_MIN_SCORE": _HIGH_FLOOR},
    )
    assert _STRONG["name"] in ids_high, (
        "the strong note (coverage 1.0) must survive a 0.5 per-arm floor; "
        f"got {ids_high}"
    )
    assert _BORDERLINE["name"] not in ids_high, (
        "with the per-arm VECTOR floor raised to 0.5 (above the borderline "
        "note's ~0.22 query-term coverage) and the GLOBAL gate OFF, the "
        "borderline note must be dropped by the per-arm floor before fusion; "
        f"got {ids_high}"
    )

    # Run 2: per-arm vector floor OFF (0.0), global gate still OFF. Same
    # borderline note now returns — the ONLY thing that changed is the per-arm
    # knob, so it is what gated the note in run 1 (not the global threshold).
    ids_off = kb.recall_ids(
        _QUERY,
        no_mmr=True,
        min_overlap=0.0,
        env={"RECALL_ARM_VECTOR_MIN_SCORE": "0.0"},
    )
    assert _STRONG["name"] in ids_off, (
        f"the strong note must still be present with the floor disabled; got {ids_off}"
    )
    assert _BORDERLINE["name"] in ids_off, (
        "with the per-arm VECTOR floor lowered to 0.0 (and the global gate "
        "still OFF), the SAME borderline note must return — proving the per-arm "
        "floor, not the global gate, decided its fate in run 1; "
        f"got {ids_off}"
    )
