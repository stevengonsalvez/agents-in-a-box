# ABOUTME: Behavioral proof for S3 — continuous confidence_num (0-1) is the canonical ranking value.
# ABOUTME: Two notes that bucket to the SAME tier still rank by their numeric confidence, and it surfaces.
"""S3 numeric-confidence ranking proof.

True invariant (corrected against the real diff at 530b929d —
`feat(reflect): store numeric confidence 0-1 beside display tiers (S3)`):

Learning confidence is a CONTINUOUS float in [0, 1] (``Learning.confidence_num``)
that is the canonical value the reranker uses; HIGH/MEDIUM/LOW are kept only as
display buckets. Lookup order for the float: explicit ``confidence_num``
frontmatter → a numeric ``confidence`` (instinct-style 0.0-1.0 notes) → the tier
midpoint (HIGH→0.9, MEDIUM→0.6, LOW→0.3). The rerank formula multiplies in
``bounded_boost(confidence_num_norm(lrn.confidence_num), CONFIDENCE_ALPHA)`` where
``confidence_num_norm(num) = clamp((num - 0.3) / 0.6, 0, 1)`` and
``CONFIDENCE_ALPHA = RECALL_CONFIDENCE_ALPHA`` (default 0.2). The norm is anchored
on the tier midpoints so tier-only legacy notes keep their EXACT pre-S3 boost
(0.9→1.0, 0.6→0.5 neutral, 0.3→0.0).

The decisive thing S3 buys, and what this proof pins, is finer-grained ranking
BETWEEN two notes that fall in the SAME display bucket. Pre-S3 the formula went
through ``confidence_norm(tier)`` — a 3-valued step function — so two MEDIUM notes
got the IDENTICAL 0.5 norm and the confidence signal was a TIE between them. S3's
continuous path distinguishes them. Three observable, LLM-free manifestations:

  A. ENGINE CAUSATION (knob toggle, decisive) — two near-identical-text twins,
     both bucketing to tier MEDIUM (numeric ``confidence: 0.79`` and ``0.51`` —
     both in [0.5, 0.8) → "MEDIUM"), differ ONLY in their numeric confidence. The
     cross-encoder scores the twins a near-tie, so the confidence boost is the
     decider. With ``RECALL_CONFIDENCE_ALPHA`` at its default the 0.79 twin
     outranks the 0.51 twin; with ``RECALL_CONFIDENCE_ALPHA=0`` the confidence
     boost vanishes and that forced ordering goes away — proving the continuous
     value, via the PORT's own flag, drove the live order (not text luck). Both
     twins carry the SAME tier, so a pre-S3 tier-only reranker would have tied
     them with the boost ON too: the ordering is uniquely an S3 (numeric) effect.

  B. PURE FUNCTION (knob toggle + tier-tie contrast) — driven out-of-process
     against the REAL recall.py: ``confidence_num_norm`` maps 0.79 and 0.51 to
     DIFFERENT norms (0.817 vs 0.350), and the confidence boost under the default
     α orders them high > low > the would-be neutral, while α=0 collapses BOTH to
     a strict 1.0 no-op. Crucially we also show the tier path ties them: both
     0.79 and 0.51 bucket to "MEDIUM" and ``confidence_norm("MEDIUM")`` is the
     SAME 0.5 for each — so the rank separation can only come from the numeric
     path S3 added, not the legacy tier path.

  C. PRESENCE / DISTINCT-FROM-ENUM — a numeric ``confidence: 0.9`` is accepted by
     the real engine and surfaces in the recall JSON as a first-class
     ``confidence_num`` field equal to 0.9, side-by-side with the display
     ``confidence`` bucket ("HIGH") — the two fields are distinct, exactly the
     "float beside display tier" shape the port ported from Hindsight
     (``memory_units.confidence_score FLOAT`` beside the bucket).

Why no LLM: the confidence values are literal frontmatter, the query is fixed,
``confidence_num_norm`` / ``bounded_boost`` are pure functions, and the toggling
flag is recall.py's own ``RECALL_CONFIDENCE_ALPHA`` env. The seeds plus the flag
fully determine the live order, the boost values, and the surfaced field.

Isolation note (why this file is deterministic across arm order): every arm is
hermetic and shares NO mutable state with any other.
  * Each ``behavioral_kb`` is per-test (built from pytest's ``tmp_path``), so
    Arm A's and Arm C's KBs, GraphRAG caches, and state dirs live in different
    tmp trees — neither reindex nor search can see the other.
  * Arm B drives the pure functions in a CHILD PROCESS (one fresh import of
    recall.py per α). It never imports recall.py into the shared pytest process
    and never mutates this process's ``os.environ`` / ``sys.modules``, so it
    cannot leak the α into the live-engine arms whatever the run order.
  * Each live-engine arm reseeds its OWN fresh KB and, before asserting any
    ordering, the recall() helper's retry guard surfaces a transient empty engine
    envelope as a retry rather than a silent empty result.

PORT: S3
"""
from __future__ import annotations

import json
import subprocess
import sys
import textwrap

# conftest resolves recall.py; reuse that exact path for the pure-function arm.
from eval.behavioral.conftest import RECALL_PY  # noqa: E402

# --------------------------------------------------------------------------
# Arm A: near-identical text twins, both tier MEDIUM, differ ONLY in numeric
# confidence. The continuous confidence boost is the tie-breaker.
#
# 0.79 and 0.51 both fall in [0.5, 0.8) so Learning.confidence == "MEDIUM" for
# BOTH (a pre-S3 tier-only reranker would tie them). Their confidence_num values
# differ (0.79 vs 0.51), so S3's numeric path separates them.
# --------------------------------------------------------------------------
_HIGH_NUM_TWIN = dict(
    name="s3-high-num-twin",
    title="Cache idempotency keys to make a payment endpoint safe to retry",
    category="reliability",
    tags=["payments", "idempotency", "retry"],
    confidence=0.79,  # numeric → tier MEDIUM, confidence_num 0.79
    created="2026-06-01",
    archived="2026-06-08T00:00:00",
    key_insight="Store an idempotency key per payment request so a retried call returns the first result instead of double-charging.",
    body=(
        "Make a payment endpoint safe to retry by recording an idempotency key "
        "for each request: on a repeat with the same key, return the stored "
        "result of the first attempt instead of charging the customer again."
    ),
)

# Same topic, near-identical body (one trailing clause differs so the two files
# are not byte-identical and don't collapse to a single chunk). Numeric
# confidence 0.51 → ALSO tier MEDIUM, but a lower confidence_num.
_LOW_NUM_TWIN = dict(
    name="s3-low-num-twin",
    title="Cache idempotency keys to make a payment endpoint safe to retry",
    category="reliability",
    tags=["payments", "idempotency", "retry"],
    confidence=0.51,  # numeric → tier MEDIUM, confidence_num 0.51
    created="2026-06-01",
    archived="2026-06-08T00:00:00",
    key_insight="Store an idempotency key per payment request so a retried call returns the first result instead of double-charging.",
    body=(
        "Make a payment endpoint safe to retry by recording an idempotency key "
        "for each request: on a repeat with the same key, return the stored "
        "result of the first attempt instead of charging the customer again, "
        "which prevents duplicate charges from network-level retries."
    ),
)

_TWIN_QUERY = "how do I make a payment endpoint safe to retry with idempotency keys"


# --------------------------------------------------------------------------
# Arm C: a numeric confidence surfaces as confidence_num, distinct from the
# display tier enum.
# --------------------------------------------------------------------------
_NUMERIC_NOTE = dict(
    name="s3-numeric-note",
    title="Use a bounded connection pool to avoid exhausting Postgres backends",
    category="database",
    tags=["postgres", "pool", "connections"],
    confidence=0.9,  # numeric → tier HIGH, confidence_num 0.9
    created="2026-06-01",
    archived="2026-06-08T00:00:00",
    key_insight="Cap the connection pool below Postgres max_connections so the app can't exhaust server backends.",
    body=(
        "Size your database connection pool with a hard upper bound that stays "
        "below Postgres max_connections, so a traffic spike queues inside the "
        "app instead of exhausting server-side backends and erroring out."
    ),
)

_POOL_QUERY = "how to size a Postgres connection pool to avoid exhausting backends"


def _rank_of(ids: list[str], name: str) -> int:
    assert name in ids, f"expected {name!r} in results, got {ids}"
    return ids.index(name)


def test_S3_numeric_confidence_breaks_a_same_tier_near_tie(behavioral_kb):
    """Arm A (engine causation, decisive knob): two twins that BOTH bucket to
    tier MEDIUM but differ in numeric confidence. With ``RECALL_CONFIDENCE_ALPHA``
    at its default the higher confidence_num twin (0.79) outranks the lower
    (0.51); zeroing the knob removes the confidence boost so that ordering is no
    longer forced. Because both twins share the SAME display tier, a pre-S3
    tier-only reranker would tie them with the boost ON — so the ON-ordering is
    uniquely the continuous S3 value's doing, proven by the flag toggle."""
    kb = behavioral_kb  # fresh per-test hermetic KB (own tmp tree; no shared state)
    kb.seed([_HIGH_NUM_TWIN, _LOW_NUM_TWIN])

    # Confidence boost ON (default α=0.2): the higher confidence_num twin leads.
    ids_on = kb.recall_ids(_TWIN_QUERY, no_mmr=True)
    assert (
        _HIGH_NUM_TWIN["name"] in ids_on and _LOW_NUM_TWIN["name"] in ids_on
    ), (
        "both twins must be retrieved on the boost-ON run from this arm's own "
        f"fresh KB; the engine returned {ids_on}. An empty/partial set here is a "
        "transient `reflect search` infra failure for THIS KB, not a ranking "
        "outcome — re-run; it is independent of any other arm's KB."
    )
    high_first_on = _rank_of(ids_on, _HIGH_NUM_TWIN["name"]) < _rank_of(
        ids_on, _LOW_NUM_TWIN["name"]
    )
    assert high_first_on, (
        "with the confidence boost ON, the twin carrying the higher numeric "
        "confidence (0.79) must outrank the near-identical twin with lower "
        "numeric confidence (0.51) — both bucket to tier MEDIUM, so only the "
        f"continuous confidence_num can separate them; got order {ids_on}"
    )

    # Confidence boost OFF (RECALL_CONFIDENCE_ALPHA=0): the confidence signal is
    # the ONLY thing that differs between the twins (same tier, tags, recency,
    # near-identical text). With it zeroed, the high-confidence-first ordering
    # must NOT be forced any more — proving the boost, not incidental text/index
    # order, drove the arm-on ordering.
    ids_off = kb.recall_ids(
        _TWIN_QUERY, no_mmr=True, env={"RECALL_CONFIDENCE_ALPHA": "0"}
    )
    assert (
        _HIGH_NUM_TWIN["name"] in ids_off and _LOW_NUM_TWIN["name"] in ids_off
    ), (
        "both twins must be retrieved on the boost-OFF run from this arm's own "
        f"fresh KB; the engine returned {ids_off}. Empty/partial == transient "
        "infra failure for THIS KB, not a ranking outcome — re-run."
    )
    high_first_off = _rank_of(ids_off, _HIGH_NUM_TWIN["name"]) < _rank_of(
        ids_off, _LOW_NUM_TWIN["name"]
    )
    assert not high_first_off, (
        "with the confidence boost OFF the higher-confidence_num twin must NOT "
        "be forced ahead — if it still leads, the continuous confidence value "
        f"was not what drove the arm-on ordering; got order {ids_off}"
    )


def _probe_confidence_funcs(alpha: str | None) -> dict:
    """Import the REAL recall.py in a FRESH child process under a given
    ``RECALL_CONFIDENCE_ALPHA`` and return its ``CONFIDENCE_ALPHA`` plus the
    confidence boost over the numeric grid AND the tier-path norms.

    Out-of-process on purpose: recall.py reads α at IMPORT time, so a fresh
    interpreter is the only faithful way to capture the knob — and the child
    cannot touch the pytest process's ``os.environ`` / ``sys.modules``, so this
    arm can never leak the α into the live-engine arms (A, C) whatever the run
    order. The boost is a pure function of the numeric confidence and α with no
    engine noise, so the values are fully determined by the grid and the flag.
    """
    probe = textwrap.dedent(
        """
        import json, importlib.util, sys
        spec = importlib.util.spec_from_file_location("recall_probe", sys.argv[1])
        mod = importlib.util.module_from_spec(spec)
        # recall.py uses @dataclass classes whose forward-ref resolution looks
        # up sys.modules[cls.__module__]; register the module before exec so the
        # import does not crash. (Same idiom the S4 proof's probe uses.)
        sys.modules[spec.name] = mod
        spec.loader.exec_module(mod)

        # The two Arm-A confidence_num values plus the tier midpoints, so the
        # parent can assert both the numeric separation AND the tier-path tie.
        nums = [0.51, 0.79]
        def cboost(num):
            return mod.bounded_boost(mod.confidence_num_norm(num), mod.CONFIDENCE_ALPHA)
        print(json.dumps({
            "alpha": mod.CONFIDENCE_ALPHA,
            "norms": {str(n): mod.confidence_num_norm(n) for n in nums},
            "boosts": {str(n): cboost(n) for n in nums},
            # Tier path: both 0.51 and 0.79 coerce to "MEDIUM", and the legacy
            # tier norm is identical for each -> a tie pre-S3.
            "tier_norm_medium": mod.confidence_norm("MEDIUM"),
        }))
        """
    )
    import os

    env = dict(os.environ)
    if alpha is None:
        env.pop("RECALL_CONFIDENCE_ALPHA", None)  # force the documented default
    else:
        env["RECALL_CONFIDENCE_ALPHA"] = alpha
    proc = subprocess.run(
        [sys.executable, "-c", probe, str(RECALL_PY)],
        capture_output=True, text=True, env=env, timeout=120,
    )
    assert proc.returncode == 0, (
        f"recall.py confidence probe failed (α={alpha!r}):\n{proc.stderr[-800:]}"
    )
    return json.loads(proc.stdout)


def test_S3_confidence_num_norm_separates_same_tier_and_toggles_with_alpha():
    """Arm B (pure function, knob ON vs OFF + tier-tie contrast): the REAL
    ``confidence_num_norm`` maps 0.79 and 0.51 to DIFFERENT norms, and the
    confidence boost orders them high > low under the default α but collapses
    BOTH to a strict 1.0 no-op at α=0. The tier path ties them (both "MEDIUM",
    same ``confidence_norm``), so the separation is uniquely the numeric path
    S3 added — and the toggling flag is recall.py's own RECALL_CONFIDENCE_ALPHA.
    """
    on = _probe_confidence_funcs(None)  # default α (0.2)
    on_alpha = on["alpha"]
    assert on_alpha > 0.0, f"expected a positive default confidence α, got {on_alpha}"

    # The continuous norms are distinct and ordered: 0.79 maps higher than 0.51.
    n_low, n_high = on["norms"]["0.51"], on["norms"]["0.79"]
    assert n_low < n_high, (
        f"confidence_num_norm must separate the two same-tier values; got "
        f"0.51→{n_low}, 0.79→{n_high}"
    )
    # Anchored rescale (num-0.3)/0.6: 0.51→0.35, 0.79→0.8166..., both strictly
    # inside (0, 1) so neither is clamped flat (a flat clamp would re-tie them).
    assert abs(n_low - 0.35) < 1e-9 and abs(n_high - (0.79 - 0.3) / 0.6) < 1e-9, (
        f"confidence_num_norm should be the anchored (num-0.3)/0.6 rescale; got "
        f"0.51→{n_low}, 0.79→{n_high}"
    )

    # Boost ON: the higher confidence_num gets the strictly larger multiplier,
    # and both sit within the ±α/2 clamp.
    b_low, b_high = on["boosts"]["0.51"], on["boosts"]["0.79"]
    assert b_low < b_high, (
        f"confidence boost must be monotone in confidence_num; got "
        f"0.51→{b_low}, 0.79→{b_high}"
    )
    assert (
        1.0 - on_alpha / 2 - 1e-9 <= b_low and b_high <= 1.0 + on_alpha / 2 + 1e-9
    ), (
        f"confidence boost must stay within [1-α/2, 1+α/2]; got "
        f"0.51→{b_low}, 0.79→{b_high} (α={on_alpha})"
    )

    # The tier path ties them: both 0.79 and 0.51 bucket to "MEDIUM" and the
    # legacy tier norm is the SAME for each. So pre-S3 (tier-only) the confidence
    # signal could not have separated these twins — the Arm-A ordering is an S3
    # (numeric) effect, not a tier effect.
    assert on["tier_norm_medium"] == 0.5, (
        "confidence_norm('MEDIUM') must be the neutral 0.5 — both Arm-A twins "
        f"share this tier, so the tier path ties them; got {on['tier_norm_medium']}"
    )

    # Boost OFF (RECALL_CONFIDENCE_ALPHA=0): the SAME function on the SAME inputs
    # is now a strict no-op — α=0 makes bounded_boost(norm, 0) == 1.0 for every
    # confidence_num. This is the decisive toggle: the knob from the S3 diff, not
    # text luck, produced the separation above.
    off = _probe_confidence_funcs("0")
    assert off["alpha"] == 0.0
    for num, val in off["boosts"].items():
        assert val == 1.0, (
            f"with RECALL_CONFIDENCE_ALPHA=0 the confidence boost must be a "
            f"strict no-op for confidence_num={num}; got {val}"
        )


def test_S3_numeric_confidence_surfaces_distinct_from_display_tier(behavioral_kb):
    """Arm C (presence / distinct-from-enum): a numeric ``confidence: 0.9`` is
    accepted by the real engine and surfaces in the recall JSON as a first-class
    ``confidence_num`` equal to 0.9, side-by-side with the display
    ``confidence`` bucket ("HIGH") — the float-beside-tier shape the port
    ported. The two fields are distinct: one is a number, one is the enum."""
    kb = behavioral_kb  # fresh per-test hermetic KB (own tmp tree; no shared state)
    kb.seed([_NUMERIC_NOTE])

    payload = kb.recall(_POOL_QUERY, limit=5, no_mmr=True)
    rows = {r.get("id"): r for r in payload.get("results", [])}
    assert _NUMERIC_NOTE["name"] in rows, (
        "the numeric-confidence note must be retrieved from this arm's own fresh "
        f"KB; got {list(rows)}. Empty/partial == transient infra failure — re-run."
    )
    row = rows[_NUMERIC_NOTE["name"]]

    # The continuous value surfaces as its own field, equal to the seeded float.
    surfaced_num = row.get("confidence_num")
    assert isinstance(surfaced_num, (int, float)) and not isinstance(surfaced_num, bool), (
        "recall JSON must carry a numeric confidence_num field (S3); got "
        f"{surfaced_num!r}. If absent/None, render_json is not emitting the "
        "continuous value."
    )
    assert abs(float(surfaced_num) - 0.9) < 1e-9, (
        "the seeded numeric confidence 0.9 must surface verbatim in "
        f"confidence_num; got {surfaced_num!r}"
    )

    # The display tier is the legacy enum, derived from the same number, and is
    # a DISTINCT field — proving the float lives beside the bucket, not instead
    # of it. 0.9 ≥ 0.8 → "HIGH".
    surfaced_tier = row.get("confidence")
    assert surfaced_tier == "HIGH", (
        "the display confidence tier must be the legacy enum bucket for 0.9 "
        f"(HIGH); got {surfaced_tier!r}. The float and the enum are distinct "
        "fields — that distinction is the S3 invariant."
    )
    assert isinstance(surfaced_tier, str), (
        f"the display tier must remain the string enum, not a number; got "
        f"{surfaced_tier!r}"
    )
