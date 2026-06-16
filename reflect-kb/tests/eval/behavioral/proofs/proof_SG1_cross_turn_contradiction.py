# ABOUTME: Behavioral proof for SG1 — cross-turn contradiction detection. Writing
# ABOUTME: "use foo" then "never use foo" through the real on-disk reflect_db
# ABOUTME: reconciles the KB to ONE latest belief: the older row loses is_latest
# ABOUTME: and is superseded_by the newer, with an audited contradiction event.
"""SG1 cross-turn contradiction (belief-revision-at-capture) proof.

Port SG1 is a STORAGE/CAPTURE port. The detection + reconciliation run inside
``reflect_db.add_learning``'s post-write hook against the sqlite ``learnings``
table — they are deliberately invisible to the file-engine recall pipeline:
``recall.py`` indexes ``documents/*.md`` and never reads the sqlite
``is_latest`` flag (verified: ``grep is_latest plugins/reflect/.../recall.py``
finds only a comment). So the strongest OBSERVABLE invariant lives where the
behaviour actually executes — the real, migrated, on-disk reflect database —
and is read back exactly the way ``/reflect:status`` (the ``contradictions``
CLI / ``get_contradiction_count``) and any sqlite-aware injector read it.

This proof drives the REAL engine (no mock reflect_db, no stubbed detector): a
fresh on-disk sqlite DB with the real schema migration + concept_index backfill,
the real ``contradiction_detector`` Jaccard/polarity rule, and the real
``events.jsonl`` audit mirror written beside the DB. ``add_learning`` is the
production capture entry point.

Invariant (four linked assertions; the seeds + the negation flip fully
determine each outcome — no LLM participates):

  1. RECONCILE-TO-ONE: after writing "use foo" (turn 1) then "never use foo"
     (turn 2), exactly ONE learning is is_latest among the contradicting pair —
     the NEW negated belief. The older "use foo" row has is_latest=0 and its
     superseded_by_learning_id points at the newer row. The agent that reads
     "latest beliefs" now gets a single, non-contradictory injection instead of
     two rows fighting on independent recency. This is the first acceptance
     bullet and SG1's whole reason to exist.

  2. AUDITED: the demotion emits a ``contradiction_detected`` event in the
     sqlite events table AND mirrors it to ``events.jsonl`` beside the DB (the
     ``~/.reflect/events.jsonl`` audit file the acceptance contract pins). The
     event names the older (demoted) row and carries the newer/older pair.

  3. STATUS SURFACES IT: the ``reflect_db contradictions`` CLI — the exact
     surface ``/reflect:status`` shells out to — reports a non-zero count over
     the same on-disk DB (run as a subprocess to prove the real CLI path, not
     just the in-process counter). This is the third acceptance bullet.

  4. FALSIFIABLE CONTROL: a same-polarity RESTATEMENT in a *separate* DB
     ("never use bar" then "don't use bar") does NOT demote anything — both stay
     is_latest=1 and zero contradiction events fire. This rules out the trivial
     "demote on any high-overlap write" failure mode and pins that the
     opposite-polarity requirement is load-bearing.

Falsifiability of the main path: if SG1 were absent (no post-write hook, or a
broken detector), turn 2 would leave BOTH rows is_latest=1, no event would be
written to sqlite or events.jsonl, and the contradictions CLI would report 0 —
assertions 1, 2 and 3 would each FAIL. If the polarity check were dropped
(demote on overlap alone), assertion 4's restatement would wrongly demote and
FAIL.

Why no recall.py assertion: see the module docstring — the file engine cannot
observe sqlite is_latest, so a recall ranking assertion would be vacuous for
this port. The sqlite reconciliation above IS the closest (and authoritative)
observable; it is read by the same status surface the agent uses. The real
recall engine is still exercised by the SG1-family sibling proofs that touch
the document layer; SG1's behaviour is purely a capture-layer reconciliation.

PORT: SG1
"""
from __future__ import annotations

import importlib
import json
import os
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the S7 storage proof does so this runs from either checkout layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))


def _fresh_db(db_path: Path):
    """Open a real on-disk reflect DB at *db_path* with the full schema.

    Points reflect_config at db_path via REFLECT_DB_PATH and reloads the
    config + db modules so every default-conn helper (events.jsonl mirror,
    contradictions CLI) resolves THIS sandbox, not the developer's ~/.reflect.
    Returns the live reflect_db module + its connection.
    """
    os.environ["REFLECT_DB_PATH"] = str(db_path)
    import reflect_config
    importlib.reload(reflect_config)
    import reflect_db
    importlib.reload(reflect_db)
    reflect_db.close_all()
    conn = reflect_db.init_db(db_path)
    # Make the module default-conn (used by the post-write hook + CLI helpers)
    # the sandboxed connection.
    reflect_db._CONN_CACHE.clear() if hasattr(reflect_db, "_CONN_CACHE") else None
    return reflect_db, conn


def _latest_rows(reflect_db, conn, *ids):
    return {i: reflect_db.get_learning(i, conn=conn) for i in ids}


def test_SG1_contradiction_reconciles_to_one_latest(tmp_path):
    db_path = tmp_path / "reflect.db"
    reflect_db, conn = _fresh_db(db_path)

    # ---- turn 1: capture the original belief. ----
    old_id = reflect_db.add_learning(
        title="use foo for config parsing",
        category="tooling",
        confidence="high",
        scope="project",
        conn=conn,
    )
    # ---- turn 2: the correction — the opposite-polarity restatement. ----
    new_id = reflect_db.add_learning(
        title="never use foo for config parsing",
        category="tooling",
        confidence="high",
        scope="project",
        conn=conn,
    )

    rows = _latest_rows(reflect_db, conn, old_id, new_id)

    # (1) RECONCILE-TO-ONE: older demoted, newer wins, supersession recorded.
    assert rows[old_id]["is_latest"] == 0, (
        "SG1 must demote the older 'use foo' belief when the contradicting "
        "'never use foo' is captured; the older row is still is_latest=1, so the "
        "agent would get BOTH contradictory rules injected (the exact bug SG1 "
        "exists to fix)."
    )
    assert rows[new_id]["is_latest"] == 1, (
        "the newer (negated) belief must remain latest after reconciliation"
    )
    assert rows[old_id]["superseded_by_learning_id"] == new_id, (
        "the demoted row must point superseded_by_learning_id at the winner "
        f"({new_id}); got {rows[old_id]['superseded_by_learning_id']!r}"
    )
    # Exactly one of the contradicting pair survives as latest.
    latest = [i for i in (old_id, new_id) if rows[i]["is_latest"] == 1]
    assert latest == [new_id], (
        f"exactly one latest belief must survive the contradiction; got {latest}"
    )

    # (2) AUDITED: sqlite event + events.jsonl mirror beside the DB.
    events = reflect_db.get_events_by_type(
        reflect_db.CONTRADICTION_EVENT_TYPE, conn=conn,
    )
    assert len(events) >= 1, (
        "a contradiction_detected event must be written to the sqlite events "
        "table; none found"
    )
    ev_details = json.loads(events[0]["details_json"] or "{}")
    assert ev_details.get("older_id") == old_id and ev_details.get("newer_id") == new_id, (
        f"the audit event must name the demoted/winner pair; got {ev_details}"
    )

    jsonl = db_path.parent / "events.jsonl"
    assert jsonl.exists(), (
        "the contradiction must mirror to events.jsonl beside the DB "
        "(~/.reflect/events.jsonl in production); file missing"
    )
    mirrored = [json.loads(l) for l in jsonl.read_text().splitlines() if l.strip()]
    contradiction_lines = [
        m for m in mirrored if m.get("type") == reflect_db.CONTRADICTION_EVENT_TYPE
    ]
    assert contradiction_lines, "no contradiction record in events.jsonl mirror"
    assert contradiction_lines[-1].get("older_id") == old_id, (
        "the events.jsonl mirror must record the demoted older row"
    )

    # (3) STATUS SURFACES IT: the real `contradictions` CLI over this on-disk DB.
    import subprocess
    cli_env = dict(os.environ)
    cli_env["REFLECT_DB_PATH"] = str(db_path)
    # Close the in-process handle so the subprocess sees committed rows on a
    # clean connection (sqlite WAL/commit already flushed by `with conn`).
    reflect_db.close_all()
    r = subprocess.run(
        [sys.executable, str(_PLUGIN_SCRIPTS / "reflect_db.py"), "contradictions"],
        capture_output=True, text=True, env=cli_env, timeout=60,
    )
    assert r.returncode == 0, f"contradictions CLI failed: {r.stderr[-600:]}"
    assert "contradictions detected: 0" not in r.stdout, (
        f"/reflect:status must surface a non-zero contradiction count after a "
        f"reconciliation; CLI said:\n{r.stdout}"
    )
    # The count line is "  contradictions detected: N" — assert N >= 1 explicitly.
    count_line = next(
        (ln for ln in r.stdout.splitlines() if "contradictions detected:" in ln), ""
    )
    n = int(count_line.rsplit(":", 1)[1].strip()) if count_line else 0
    assert n >= 1, f"expected >=1 contradiction in status, got {n} ({count_line!r})"


def test_SG1_same_polarity_restatement_does_not_demote(tmp_path):
    """Falsifiable control: opposite-polarity is load-bearing.

    Two negated restatements of the same rule are NOT a contradiction — if SG1
    demoted on token overlap alone (dropping the polarity check) this would
    wrongly demote and the assertions below would fail.
    """
    db_path = tmp_path / "reflect_ctrl.db"
    reflect_db, conn = _fresh_db(db_path)

    a_id = reflect_db.add_learning(
        title="never use bar in the hot path",
        category="perf",
        confidence="high",
        scope="project",
        conn=conn,
    )
    b_id = reflect_db.add_learning(
        title="don't use bar in the hot path",
        category="perf",
        confidence="high",
        scope="project",
        conn=conn,
    )

    rows = _latest_rows(reflect_db, conn, a_id, b_id)
    assert rows[a_id]["is_latest"] == 1 and rows[b_id]["is_latest"] == 1, (
        "two same-polarity restatements must NOT trigger a demotion — both "
        f"should stay latest; got a={rows[a_id]['is_latest']} "
        f"b={rows[b_id]['is_latest']}"
    )
    assert rows[a_id]["superseded_by_learning_id"] is None, (
        "a restatement must not record a supersession"
    )
    events = reflect_db.get_events_by_type(
        reflect_db.CONTRADICTION_EVENT_TYPE, conn=conn,
    )
    assert events == [], (
        f"no contradiction event must fire for a same-polarity restatement; "
        f"got {len(events)}"
    )
    reflect_db.close_all()
