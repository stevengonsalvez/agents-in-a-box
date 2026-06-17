# ABOUTME: Behavioral proof for O3 — first-class persona/preference fields per
# ABOUTME: scope. Drives the REAL reflect_cascade.execute_persona_actions over the
# ABOUTME: real reflect_db project_persona table on an on-disk reflect.db: N
# ABOUTME: 'prefer TDD' actions aggregate to ONE testing_style=TDD field whose
# ABOUTME: confidence rises with distinct evidence (>0.8 at ~10), thin/contradictory
# ABOUTME: evidence stays below threshold, UPDATE folds (no dup row), and the
# ABOUTME: open-domain field lookup answers a matching query directly (no LLM).
"""O3 persona/preference-fields proof.

Port O3 (bead agents-in-a-box-kdo.48, Hindsight banks.disposition persona
object) is a STORAGE/retrieval port. It adds the structured-fields layer ON TOP
of the O1 observations layer: where an O1 observation is a free-text aggregate
statement ("this team generally prefers TDD"), an O3 persona field is the
DISTILLED typed answer (testing_style='TDD') keyed by (project_id, field_name)
in the real ``project_persona`` table in ``reflect_db``. Fields are AGGREGATED
from O1 observations — never hand-authored — and confidence is a DETERMINISTIC
function of the count of distinct source observations cited
(reflect_db.persona_confidence). The drain's second pass emits persona
CREATE/UPDATE actions; the executor under test is
``reflect_cascade.execute_persona_actions``.

This proof drives those real modules against an on-disk ``reflect.db`` + tmp
state dir directly. No LLM, no torch model, no vector engine is on the path: the
seeds plus the literal persona-action objects fully determine every asserted
outcome. (In production the drain's LLM only *chooses* which CREATE/UPDATE
action to emit; here we hand the executor the actions verbatim, so the
assertions test the executor's deterministic aggregation + the deterministic
confidence curve over the evidence count, never an LLM decision.)

The TRUE invariant:

``execute_persona_actions`` distils persona CREATE/UPDATE actions into a typed
``project_persona`` field whose confidence is a deterministic function of the
distinct source-observation evidence, and the open-domain field lookup answers a
matching query directly:

  AGGREGATION + CONFIDENCE RISE (decisive): ~10 'prefer TDD' actions, each
    citing a DISTINCT source observation, fold into ONE testing_style row (not
    10 siblings). source_observation_ids accumulates every distinct id and
    confidence rises monotonically with the evidence count, crossing 0.8 at ~10
    distinct sources. proof: confidence = min(1, n/12), so n=10 -> 0.833 > 0.8.

  THIN/CONTRADICTORY EVIDENCE STAYS BELOW THRESHOLD (control): a field backed by
    only 1-2 distinct sources does NOT cross 0.8, so the open-domain lookup
    refuses to answer from it — the agent does not get a confident disposition
    out of thin evidence.

  UPDATE FOLDS, NO DUP ROW + IDEMPOTENT: an UPDATE for the same field folds new
    evidence into the SAME (project_id, field_name) row — exactly one row
    survives, confidence is RECOMPUTED from the grown distinct count, and
    re-citing only already-recorded ids cannot inflate confidence past the real
    distinct evidence.

  OPEN-DOMAIN LOOKUP ANSWERS DIRECTLY (the O3 retrieval knob): for an
    open-domain query naming the field ('what testing style do we use?'),
    ``recall_persona_field`` returns the high-confidence persona value directly;
    a closed-domain query ('how do I fix the flaky test?') and an open-domain
    query that names a DIFFERENT (thin) field both return None — the caller then
    falls through to normal recall.

Why no LLM: every asserted value is a deterministic function of the seeds and
the literal action dicts. The confidence is integer-count arithmetic
(min(1, n/SATURATION)); the dedup of evidence ids is set membership; the field
match is a substring test of the field_name tokens against the lowered query;
the threshold gate is a numeric comparison. Nothing asserted here was decided by
a model.

Falsifiability: if CREATE/UPDATE spawned siblings instead of folding, the row
count would be > 1 and confidence would be stuck at the seed value. If
confidence did not rise with evidence, the ~10-source field would stay below 0.8
and the lookup would refuse. If the threshold gate were absent, the thin
1-2-source field would answer the open-domain query. If a re-cite of known ids
inflated evidence, the idempotent re-run would bump confidence. If the lookup
ignored open-domainness or field-name matching, the closed-domain / wrong-field
queries would wrongly return a value. Each arm builds its OWN fresh on-disk DB +
state dir (no cross-arm contamination).

PORT: O3
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the O1 storage proof does so this runs from either checkout layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402


@pytest.fixture
def db(tmp_path, monkeypatch):
    """Fresh isolated on-disk reflect DB wired as the MODULE-DEFAULT connection.

    ``execute_persona_actions`` calls reflect_db helpers
    (``get_persona_field``/``upsert_persona_field``) WITHOUT a conn= argument
    (production shape), so they resolve via ``reflect_db.get_conn``. Pointing
    get_conn at this sandbox makes the real executor drive THIS db, not the
    developer's ~/.reflect. REFLECT_STATE_DIR is redirected too so any state
    materialization lands inside the per-test tmp tree.
    """
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    yield connection
    reflect_db.close_all()


def _tdd_actions(n: int, *, project: str = "alpha", start: int = 0):
    """N persona CREATE/UPDATE actions, each citing a DISTINCT source obs id.

    The first is a CREATE, the rest UPDATEs — but both route through the same
    idempotent upsert, so this models the drain folding evidence one drain-slice
    at a time (the realistic shape: ~10 separate 'team preferred TDD again'
    corrections distilled across sessions).
    """
    actions = []
    for i in range(start, start + n):
        actions.append({
            "action": "CREATE" if i == start else "UPDATE",
            "project_id": project,
            "field_name": "testing_style",
            "value": "TDD",
            "source_observation_ids": [f"obs-tdd-{i}"],
        })
    return actions


# ── Arm 1: ~10 'prefer TDD' actions aggregate to ONE high-confidence field ────

def test_O3_ten_prefer_tdd_actions_aggregate_to_confident_testing_style(db):
    """The decisive knob: ~10 'prefer TDD' persona actions, each citing a
    distinct source observation, fold into ONE testing_style=TDD field whose
    confidence rises with the evidence count and crosses 0.8 at ~10 sources —
    and the evidence (source_observation_ids) reflects the count.
    """
    conn = db
    project = "alpha"

    # Fold the 10 actions in ONE at a time, capturing confidence after each so
    # we can prove it rises monotonically (never an LLM judgement).
    confidences: list[float] = []
    for action in _tdd_actions(10, project=project):
        summary = reflect_cascade.execute_persona_actions([action])
        assert summary["errors"] == [], f"clean persona apply expected; got {summary}"
        row = reflect_db.get_persona_field(project, "testing_style", conn=conn)
        confidences.append(row["confidence"])

    # ONE row, not 10 siblings.
    fields = reflect_db.get_persona_fields(project, conn=conn)
    assert len(fields) == 1, (
        f"10 'prefer TDD' actions must fold into ONE field, not siblings; got {fields}"
    )
    final = reflect_db.get_persona_field(project, "testing_style", conn=conn)
    assert final["value"] == "TDD"

    # Confidence rose monotonically with evidence.
    assert confidences == sorted(confidences), (
        f"confidence must rise (or hold) as evidence accumulates; got {confidences}"
    )
    assert confidences[0] < confidences[-1], (
        "confidence at 1 source must be below confidence at 10 sources"
    )

    # Crossed the 0.8 high-confidence threshold at ~10 distinct sources.
    assert final["confidence"] > 0.8, (
        "~10 distinct corroborating observations must produce confidence > 0.8 "
        f"(the answered-disposition threshold); got {final['confidence']}"
    )

    # Provenance reflects the evidence count — every distinct source tracked.
    sources = json.loads(final["source_observation_ids"])
    assert len(sources) == 10 and len(set(sources)) == 10, (
        "source_observation_ids must track all 10 DISTINCT source observations "
        f"the field is distilled from; got {sources}"
    )
    assert final["confidence"] == reflect_db.persona_confidence(10), (
        "confidence must be the deterministic function of the distinct evidence "
        "count — no LLM, no fudge"
    )


# ── Arm 2: thin/contradictory evidence stays below the answer threshold ───────

def test_O3_thin_evidence_stays_below_confidence_threshold(db):
    """A field backed by only 1-2 distinct sources does NOT cross 0.8, so the
    open-domain lookup refuses to answer from it — a thin disposition never
    short-circuits recall.
    """
    conn = db
    project = "beta"

    # Two distinct sources only (a thin/contradictory signal).
    for action in _tdd_actions(2, project=project):
        reflect_cascade.execute_persona_actions([action])

    row = reflect_db.get_persona_field(project, "testing_style", conn=conn)
    assert row is not None
    assert row["confidence"] < 0.8, (
        "1-2 thin sources must stay below the 0.8 threshold; "
        f"got {row['confidence']}"
    )

    # The open-domain lookup refuses to surface a sub-threshold field.
    answer = reflect_db.recall_persona_field(
        "what testing style do we generally use", project, conn=conn
    )
    assert answer is None, (
        "a thinly-evidenced field must NOT answer the open-domain query — the "
        "caller falls through to normal recall instead"
    )

    # ...but a get with no floor still SEES the stored (low-confidence) field —
    # it exists, it is simply not confident enough to short-circuit recall.
    assert reflect_db.get_persona_fields(
        project, min_confidence=0.0, conn=conn
    ), "the field is stored; it just sits below the answer threshold"


# ── Arm 3: UPDATE folds into the SAME row; idempotent re-cite cannot inflate ──

def test_O3_update_folds_into_same_row_and_recite_is_idempotent(db):
    """An UPDATE for the same field folds new evidence into the SAME
    (project, field) row — exactly one row, confidence recomputed from the
    grown distinct count — and re-citing only already-recorded ids cannot
    inflate confidence past the real distinct evidence.
    """
    conn = db
    project = "gamma"

    # Seed with 6 distinct sources.
    for action in _tdd_actions(6, project=project):
        reflect_cascade.execute_persona_actions([action])
    after_six = reflect_db.get_persona_field(project, "testing_style", conn=conn)
    assert len(json.loads(after_six["source_observation_ids"])) == 6

    # Fold 3 MORE distinct sources via UPDATE.
    more = reflect_cascade.execute_persona_actions(_tdd_actions(3, project=project, start=6))
    assert more["errors"] == []
    rows = reflect_db.get_persona_fields(project, conn=conn)
    assert len(rows) == 1, f"UPDATE must fold, never spawn a sibling; got {rows}"
    after_nine = reflect_db.get_persona_field(project, "testing_style", conn=conn)
    assert len(json.loads(after_nine["source_observation_ids"])) == 9, (
        "the 3 new distinct sources fold into the existing evidence set"
    )
    assert after_nine["confidence"] > after_six["confidence"], (
        "confidence is RECOMPUTED from the grown distinct evidence count"
    )
    assert after_nine["confidence"] == reflect_db.persona_confidence(9)

    # IDEMPOTENT: re-cite only already-recorded ids — confidence must not move.
    recite = reflect_cascade.execute_persona_actions(_tdd_actions(9, project=project, start=0))
    assert recite["errors"] == []
    final = reflect_db.get_persona_field(project, "testing_style", conn=conn)
    assert len(json.loads(final["source_observation_ids"])) == 9, (
        "re-citing known ids must not duplicate evidence"
    )
    assert final["confidence"] == after_nine["confidence"], (
        "an idempotent re-cite must NOT inflate confidence past the real "
        f"distinct evidence; got {final['confidence']} vs {after_nine['confidence']}"
    )
    assert len(reflect_db.get_persona_fields(project, conn=conn)) == 1


# ── Arm 4: open-domain lookup answers a matching query directly; misses fall through ─

def test_O3_open_domain_lookup_answers_matching_query_directly(db):
    """For an open-domain query naming the field, ``recall_persona_field``
    returns the high-confidence persona value DIRECTLY (no recall). A
    closed-domain query and an open-domain query naming a DIFFERENT, thin field
    both return None — the caller falls through to normal recall.
    """
    conn = db
    project = "delta"

    # A confident testing_style field (10 distinct sources -> > 0.8).
    for action in _tdd_actions(10, project=project):
        reflect_cascade.execute_persona_actions([action])
    # A thin commit_style field (only 2 sources -> below threshold).
    for i in range(2):
        reflect_cascade.execute_persona_actions([{
            "action": "CREATE" if i == 0 else "UPDATE",
            "project_id": project,
            "field_name": "commit_style",
            "value": "conventional",
            "source_observation_ids": [f"obs-commit-{i}"],
        }])

    # OPEN-DOMAIN + names the confident field -> direct answer.
    answer = reflect_db.recall_persona_field(
        "what testing style does this team prefer", project, conn=conn
    )
    assert answer is not None, "an open-domain query naming a confident field must answer"
    assert answer["field_name"] == "testing_style"
    assert answer["value"] == "TDD"
    assert answer["tier"] == "persona"
    assert answer["confidence"] > 0.8

    # CLOSED-DOMAIN query -> no persona short-circuit (falls through to recall).
    closed = reflect_db.recall_persona_field(
        "how do I fix the flaky testing_style assertion in CI", project, conn=conn
    )
    assert closed is None, (
        "a closed-domain 'how do I fix X' query must NOT answer from persona — "
        "it falls through to normal recall"
    )

    # OPEN-DOMAIN but names the THIN field -> below threshold, no answer.
    thin = reflect_db.recall_persona_field(
        "what commit style does this team prefer", project, conn=conn
    )
    assert thin is None, (
        "an open-domain query naming a sub-threshold field must NOT answer — "
        "thin evidence does not short-circuit recall"
    )
