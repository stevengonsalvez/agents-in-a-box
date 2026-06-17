# ABOUTME: Behavioral proof for port C2 — the synthesis pipeline auto-triggers a consolidation
# ABOUTME: pass once N (default 30) learnings accumulate since the last run, counted from the real
# ABOUTME: learnings table, and re-arms after a pass stamps a fresh baseline and zeros the counter.
"""C2 auto-consolidation-on-N-learnings proof (storage-counter trigger primitive).

Port C2 is a STORAGE port, NOT a retrieval port. The real diff (commit
ecbfa132, "auto-trigger consolidation when N new learnings land (C2)") adds the
``should_auto_trigger`` / ``learnings_since_last_consolidation`` /
``record_consolidation_run`` trio to
``plugins/reflect/scripts/reflect_synthesis.py``. The launchd ``--check-auto``
tick counts learnings rows created since the last consolidation baseline
(``last_consolidation_at`` metric) and fires the Opus synthesis EARLY when the
pending count crosses the threshold (default 30, ``REFLECT_SYNTHESIS_AUTO_THRESHOLD``
overridable); a completed pass stamps a new baseline and zeros the
``learnings_since_last_consolidation`` counter so the trigger re-arms. The count
is recomputed from the learnings table on every read (never a writer-side
increment) so it cannot drift. ``recall.py`` never touches any of this — the
behaviour is a write-time counter strictly upstream of indexing, so the
strongest OBSERVABLE invariant lives in the real module, driven directly (no
mock, no stub, no LLM).

The supplied hypothesis was correct in shape; this proof pins it against the
real code. Two corrections from the diff:

  * the counter is derived from ``COUNT(*) FROM learnings WHERE created_at >
    last_consolidation_at`` — a fresh table-count, NOT a stored increment;
  * the trigger has a SECOND path (the weekly ``--max-age`` age fallback). To
    isolate the THRESHOLD as the cause, every threshold arm below pins a RECENT
    baseline plus a huge ``max_age``, so the age path can never fire and the only
    live trigger is the learnings count.

INVARIANT (seeds + the threshold knob fully determine each outcome — no LLM runs
in the assertion; the learnings table is the oracle). Each arm seeds its OWN
fresh isolated tmp DB (never ~/.reflect):

  1. BELOW THRESHOLD does NOT trigger: with the documented threshold 30, a recent
     baseline, and a huge max_age, seeding 29 learnings reports
     ``triggered=False`` ("below threshold (29 < 30)"). Adding learnings up to —
     but not crossing — the threshold must not fire a consolidation.

  2. CROSSING THRESHOLD DOES trigger: the SAME setup with exactly 30 learnings
     reports ``triggered=True`` ("threshold crossed (30 >= 30)"). The ``>=``
     boundary is the documented N=30 crossing point — 30 is the first count that
     fires.

  3. KNOB FLIP / FALSIFIABLE — the threshold value moves the trigger point:
     seeding exactly 5 learnings does NOT trigger at threshold 30 (default) but
     DOES trigger at threshold 5, and the documented default IS 30
     (``auto_trigger_threshold()`` with no override), flipping to 5 under the
     ``REFLECT_SYNTHESIS_AUTO_THRESHOLD`` env override. This proves the verdicts
     in (1)/(2) are caused by the count-vs-threshold comparison, not by a fixed
     "30" constant or by the age fallback.

  4. COUNTER RESET / re-arm (the storage observable): seeding 30 learnings mirrors
     ``learnings_since_last_consolidation`` = 30 into the metrics table and
     triggers; a completed pass (``record_consolidation_run``) stamps a fresh
     ``last_consolidation_at`` baseline and zeros the metric, after which the
     SAME 30 rows are counted as 0 pending and NO LONGER trigger. This is C2's
     whole reason to exist — the counter resets so an active project gets exactly
     one early pass per N-learning batch, not a re-fire every tick.

Falsifiability: if the threshold gate were broken (always-trigger), assertion 1
would FAIL (29 < 30 would fire). If the ``>=`` boundary were ``>``, assertion 2
would FAIL (30 would not fire). If the threshold were a hard-coded constant
ignoring the knob, assertion 3 would FAIL (5 would never trigger). If the reset
did not move the baseline, assertion 4 would FAIL (the same 30 rows would keep
firing forever). If any arm leaked the age fallback (recent baseline / huge
max_age guard dropped), the threshold arms would trigger for the wrong reason and
the proof would be vacuous — the reason string is asserted to be the
``threshold``/``below threshold`` path, never ``age fallback``.

Surface used: storage (real reflect_synthesis + reflect_db modules over an
isolated tmp SQLite DB), not the behavioral_kb retrieval fixture — recall is the
wrong surface for a write-time counter that recall.py never reads. No torch model
is loaded; this proof is fast.

PORT: C2
"""
from __future__ import annotations

import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the M5 capture-layer proof does so this runs from either layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_db  # noqa: E402
import reflect_synthesis  # noqa: E402

# The documented default threshold from the C2 diff (Hindsight enable_auto_
# consolidation shape). Pinned as a literal so a silent constant change is caught.
_DOCUMENTED_DEFAULT_THRESHOLD = 30
# A huge age window so the weekly --max-age fallback can NEVER fire in the
# threshold arms — isolating the learnings count as the sole live trigger path.
_HUGE_MAX_AGE = 365 * 86400


def _fresh_conn(tmp_path: Path, *, recent_baseline: bool = True):
    """A fresh isolated DB with (optionally) a recent consolidation baseline.

    Each arm gets its OWN file so no state leaks between arms. A *recent*
    baseline (2s ago) plus _HUGE_MAX_AGE guarantees the age fallback cannot
    fire, so every trigger verdict in the threshold arms is caused strictly by
    the learnings-count >= threshold comparison.
    """
    conn = reflect_db.init_db(tmp_path / "reflect.db")
    if recent_baseline:
        base = (datetime.now(timezone.utc) - timedelta(seconds=2)).isoformat()
        reflect_db.set_metric(
            reflect_synthesis.LAST_CONSOLIDATION_KEY, base, conn=conn
        )
    return conn


def _seed_learnings(conn, n: int) -> None:
    """Insert *n* real learnings via the production writer. created_at defaults
    to now() > the recent baseline, so all n count as pending."""
    for i in range(n):
        reflect_db.add_learning(f"learning number {i} about topic {i}", conn=conn)


def _trigger(conn, threshold: int, max_age_seconds: float = _HUGE_MAX_AGE):
    return reflect_synthesis.should_auto_trigger(
        threshold, max_age_seconds, conn=conn, db=reflect_db
    )


@pytest.fixture(autouse=True)
def _isolate_db_cache():
    """Drop reflect_db's connection cache before AND after each arm so no tmp DB
    handle survives into the next arm — keeps the per-arm fresh-DB isolation
    honest and the double-pass deterministic."""
    reflect_db.close_all()
    yield
    reflect_db.close_all()


def test_C2_below_threshold_does_not_trigger(tmp_path):
    """(1) BELOW THRESHOLD: 29 learnings at the documented threshold 30 (recent
    baseline, huge max_age) must NOT trigger. Adding learnings up to — but not
    crossing — N is not enough to fire a consolidation."""
    conn = _fresh_conn(tmp_path)
    _seed_learnings(conn, 29)

    triggered, reason, count = _trigger(conn, _DOCUMENTED_DEFAULT_THRESHOLD)

    assert count == 29, "29 freshly-seeded learnings must all count as pending"
    assert triggered is False, (
        "29 pending < threshold 30 must NOT trigger — the early consolidation "
        "fires only on crossing N, not before"
    )
    assert "below threshold" in reason, (
        f"the non-trigger must be the THRESHOLD path, not the age fallback "
        f"(got reason: {reason!r}) — otherwise the arm is vacuous"
    )


def test_C2_crossing_threshold_does_trigger(tmp_path):
    """(2) CROSSING THRESHOLD: exactly 30 learnings at threshold 30 (the SAME
    isolated setup) must trigger. 30 is the documented N and the >= boundary —
    the first count that fires."""
    conn = _fresh_conn(tmp_path)
    _seed_learnings(conn, 30)

    triggered, reason, count = _trigger(conn, _DOCUMENTED_DEFAULT_THRESHOLD)

    assert count == 30, "30 freshly-seeded learnings must all count as pending"
    assert triggered is True, (
        "30 pending >= threshold 30 MUST trigger — crossing N fires the early "
        "consolidation pass; this is the C2 invariant"
    )
    assert "threshold crossed" in reason, (
        f"the trigger must be the THRESHOLD path, not the age fallback "
        f"(got reason: {reason!r}) — the recent baseline + huge max_age ensure "
        f"the age path is dead"
    )


def test_C2_threshold_knob_moves_the_trigger_point(tmp_path):
    """(3) KNOB FLIP: the threshold VALUE moves the trigger point. The SAME 5
    learnings do not trigger at 30 but do at 5, and the documented default IS 30,
    flipping to 5 under the env override. Proves the verdict is the
    count-vs-threshold comparison, not a baked-in 30 or the age fallback."""
    conn = _fresh_conn(tmp_path)
    _seed_learnings(conn, 5)

    # SAME data, two thresholds -> opposite verdicts.
    triggered_hi, reason_hi, count_hi = _trigger(conn, 30)
    triggered_lo, reason_lo, count_lo = _trigger(conn, 5)

    assert count_hi == count_lo == 5, "the underlying pending count is identical"
    assert triggered_hi is False, "5 < 30: must NOT trigger at the high threshold"
    assert "below threshold" in reason_hi
    assert triggered_lo is True, "5 >= 5: MUST trigger once the threshold drops to 5"
    assert "threshold crossed" in reason_lo, (
        "lowering the threshold below the pending count moves the trigger point — "
        "the threshold is a real knob, not a constant"
    )

    # The documented default is 30; the env override moves it (pin both ends).
    import os

    prior = os.environ.pop("REFLECT_SYNTHESIS_AUTO_THRESHOLD", None)
    try:
        assert reflect_synthesis.auto_trigger_threshold() == _DOCUMENTED_DEFAULT_THRESHOLD, (
            "with no override the documented default threshold must be 30 — the "
            "Hindsight enable_auto_consolidation value the diff pins"
        )
        os.environ["REFLECT_SYNTHESIS_AUTO_THRESHOLD"] = "5"
        assert reflect_synthesis.auto_trigger_threshold() == 5, (
            "REFLECT_SYNTHESIS_AUTO_THRESHOLD must override the default threshold — "
            "active projects can tune the trigger point"
        )
    finally:
        if prior is None:
            os.environ.pop("REFLECT_SYNTHESIS_AUTO_THRESHOLD", None)
        else:
            os.environ["REFLECT_SYNTHESIS_AUTO_THRESHOLD"] = prior


def test_C2_counter_resets_and_rearms_after_a_pass(tmp_path):
    """(4) COUNTER RESET: 30 learnings mirror the pending count into the metrics
    table and trigger; a completed pass stamps a fresh baseline and zeros the
    counter, after which the SAME 30 rows count as 0 pending and NO LONGER
    trigger. The counter re-arms — one early pass per batch, not a re-fire every
    tick."""
    conn = _fresh_conn(tmp_path)
    _seed_learnings(conn, 30)

    # The count is mirrored into the live metric — the inspectable observable.
    pending = reflect_synthesis.learnings_since_last_consolidation(conn=conn, db=reflect_db)
    assert pending == 30
    assert reflect_db.get_metric(
        reflect_synthesis.PENDING_LEARNINGS_KEY, conn=conn
    ) == 30, "learnings_since_last_consolidation must be mirrored into metrics"

    triggered_before, _, _ = _trigger(conn, _DOCUMENTED_DEFAULT_THRESHOLD)
    assert triggered_before is True, "30 pending triggers before the pass runs"

    # A completed (non-dry-run) pass IS a consolidation — stamp baseline, zero counter.
    reflect_synthesis.record_consolidation_run(conn=conn, db=reflect_db)

    assert reflect_db.get_metric(
        reflect_synthesis.PENDING_LEARNINGS_KEY, conn=conn
    ) == 0, "the pending counter must be zeroed after a consolidation pass"
    assert reflect_db.get_metric(
        reflect_synthesis.LAST_CONSOLIDATION_KEY, conn=conn
    ), "a fresh consolidation baseline must be stamped"

    # SAME 30 rows, but the moved baseline means 0 are now pending -> no re-fire.
    pending_after = reflect_synthesis.learnings_since_last_consolidation(
        conn=conn, db=reflect_db
    )
    assert pending_after == 0, (
        "after the baseline moves, the already-consolidated 30 rows count as 0 "
        "pending — the counter resets"
    )
    triggered_after, reason_after, _ = _trigger(conn, _DOCUMENTED_DEFAULT_THRESHOLD)
    assert triggered_after is False, (
        "the SAME 30 rows must NOT re-trigger after a pass — the counter re-arms, "
        "giving exactly one early consolidation per N-learning batch"
    )
    assert "below threshold" in reason_after


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
