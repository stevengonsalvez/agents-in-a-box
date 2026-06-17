# ABOUTME: Behavioral proof for O1 — the consolidated observations layer (the
# ABOUTME: persona/conventions aggregate store). Drives the REAL
# ABOUTME: reflect_cascade.execute_observation_actions over the real reflect_db
# ABOUTME: observations + observation_history tables on an on-disk reflect.db: a
# ABOUTME: CREATE/UPDATE/DELETE action builds/updates the aggregate so it reflects
# ABOUTME: the current persona/conventions, while an untouched scope stays unchanged.
"""O1 consolidated-observations-layer proof.

Port O1 (bead agents-in-a-box-kdo.44) is a STORAGE port. It adds the drain's
SECOND output stream: a consolidated *observations* layer — persona/convention
aggregate statements ("this team prefers X", "this codebase generally does Y")
that accumulate evidence over time — kept beside the raw correction-shaped
learnings. The layer is two real sqlite tables in ``reflect_db``
(``observations`` + ``observation_history``) maintained by the second-pass
executor ``reflect_cascade.execute_observation_actions``. This proof drives
those real modules against an on-disk ``reflect.db`` + tmp state dir directly.
No LLM, no torch model, no vector engine is on the path: the seeds plus the
literal observation-action objects fully determine every asserted outcome.

(In production the drain's LLM only *chooses* which CREATE/UPDATE/DELETE action
to emit; here we hand the executor the actions verbatim, so the assertions test
the executor's deterministic state transitions over the aggregate store, never
an LLM decision.)

The TRUE invariant (corrected against the real diff at 3f50dfc0 —
``feat(reflect): consolidated observations layer for persona/conventions
(O1)``):

``execute_observation_actions`` applies structured actions to the consolidated
observations aggregate so it reflects the CURRENT persona/conventions:

  CREATE -> a new aggregate row lands in the observations table. proof_count
    starts at the number of cited source_correction_ids (floor 1); status is
    'active'; the statement is now readable back via ``get_observations`` /
    ``get_observation``.

  UPDATE -> ``add_observation_evidence`` folds NEW correction ids into the
    SAME aggregate: source_correction_ids append uniquely and proof_count
    grows by the count of NEW ids — 50 'team prefers X' corrections folded one
    at a time end at proof 50 on ONE row, not 50 near-duplicate siblings. The
    S6 non-destructive contract holds: a snapshot of the PRIOR form lands in
    observation_history BEFORE the mutation, so the old wording/evidence is
    never lost. Re-citing only already-recorded ids is an idempotent no-op
    (returns ``updated`` 0, ``skipped`` 1) so a re-run can never inflate
    evidence.

  DELETE -> ``retire_observation`` retires the aggregate NON-destructively:
    status -> 'retired' (+ reason), so it drops out of the active tier
    (``get_observations`` default), yet the row survives and a history
    snapshot records the retirement — "why did this convention stop holding?"
    stays answerable.

  CONTROL (untouched scope unchanged): an aggregate seeded into a DIFFERENT
    scope, never named by any action, is byte-for-byte identical afterward —
    same content, same proof_count, same status, same updated_at, and zero
    history rows. The executor mutates only the targeted aggregates, never the
    whole store.

Why no LLM: every asserted value is a deterministic function of the seeds and
the literal action dicts. proof_count arithmetic is integer addition over the
cited id set; the active/retired status is a literal SQL UPDATE; the history
snapshot is a verbatim JSON dump of the prior row; the untouched-scope control
is simply a row no action references. Nothing asserted here was decided by a
model.

Falsifiability: if CREATE didn't build the aggregate, Arm 1's read-back would
be empty. If UPDATE created a sibling instead of folding evidence, Arm 2 would
show two rows / proof_count stuck at the seed value. If UPDATE were
destructive, Arm 2's observation_history would be empty and the prior wording
lost. If a re-run inflated evidence, Arm 2's idempotent re-cite would bump
proof_count. If DELETE hard-deleted, Arm 3's retired row would vanish from
``include_retired=True`` and leave no history. If the executor touched the
whole store, every arm's CONTROL row (a different, unnamed scope) would change.
Each arm builds its OWN fresh on-disk DB + state dir (no cross-arm
contamination).

PORT: O1
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the O2 storage proof does so this runs from either checkout layout.
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

    ``execute_observation_actions`` calls reflect_db helpers
    (``add_observation``/``add_observation_evidence``/``retire_observation``)
    WITHOUT a conn= argument (production shape), so they resolve via
    ``reflect_db.get_conn``. Pointing get_conn at this sandbox makes the real
    executor drive THIS db, not the developer's ~/.reflect. REFLECT_STATE_DIR
    is redirected too so anything the O2 conventions trigger materializes lands
    inside the per-test tmp tree, never ~/.reflect.
    """
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    yield connection
    reflect_db.close_all()


def _control_snapshot(conn, oid: str) -> dict:
    """The mutation-relevant fields of an aggregate row, for an exact control
    comparison. include_retired so a retired control would still be visible."""
    row = reflect_db.get_observation(oid, conn=conn)
    assert row is not None, f"control observation {oid!r} disappeared"
    return {
        "content": row["content"],
        "scope": row["scope"],
        "status": row["status"],
        "proof_count": row["proof_count"],
        "source_correction_ids": row["source_correction_ids"],
        "updated_at": row["updated_at"],
    }


# ── Arm 1: CREATE builds the aggregate; an untouched-scope control is unchanged ─

def test_O1_create_builds_aggregate_and_leaves_control_scope_unchanged(db):
    """The decisive knob: a CREATE action lands a new consolidated observation
    in the targeted scope (readable back, proof_count seeded from the cited
    corrections, status active), while an aggregate in a DIFFERENT scope —
    named by no action — is byte-for-byte unchanged.
    """
    conn = db

    # CONTROL: an aggregate in scope `beta`, seeded directly, never referenced
    # by any action below.
    control = reflect_db.add_observation(
        "beta pins all deps with a lockfile",
        category="Tooling",
        scope="beta",
        source_correction_ids=["b1"],
        conn=conn,
    )
    control_before = _control_snapshot(conn, control)
    assert reflect_db.get_observations(scope="alpha", conn=conn) == [], (
        "alpha scope must start with no aggregate — the CREATE is what builds it"
    )

    # ---- ACT: CREATE a persona/convention aggregate scoped to alpha, citing
    # two corrections as its founding evidence. ----
    convention = "alpha generally prefers uv over pip for python dependency management"
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "CREATE",
            "content": convention,
            "category": "Tooling",
            "scope": "alpha",
            "source_correction_ids": ["corr-a1", "corr-a2"],
        }],
    )

    assert summary["created"] == 1 and summary["errors"] == [], (
        f"the CREATE must insert exactly one observation; got {summary}"
    )

    # THE AGGREGATE WAS BUILT: alpha's tier now holds exactly the new statement.
    alpha = reflect_db.get_observations(scope="alpha", conn=conn)
    assert len(alpha) == 1, f"alpha must hold exactly the one new aggregate; got {alpha}"
    built = alpha[0]
    assert built["content"] == convention, (
        "the aggregate must store the persona/convention statement verbatim"
    )
    assert built["status"] == reflect_db.OBSERVATION_STATUS_ACTIVE
    assert built["proof_count"] == 2, (
        "proof_count must seed from the count of cited corrections (2) — the "
        f"aggregate is born with the evidence it consolidates; got {built['proof_count']}"
    )
    assert json.loads(built["source_correction_ids"]) == ["corr-a1", "corr-a2"], (
        "the cited correction ids are the aggregate's provenance"
    )

    # THE CONTROL DID NOT MOVE: beta's aggregate is identical, field for field.
    assert _control_snapshot(conn, control) == control_before, (
        "an aggregate in a scope no action touched must be byte-for-byte "
        "unchanged — the executor mutates only the targeted aggregates"
    )
    assert reflect_db.get_observation_history(control, conn=conn) == [], (
        "the untouched control aggregate must have accrued no history snapshots"
    )


# ── Arm 2: UPDATE folds evidence into the SAME aggregate, non-destructively ───

def test_O1_update_folds_evidence_into_same_aggregate_and_snapshots_history(db):
    """An UPDATE accumulates evidence on ONE aggregate instead of spawning a
    near-duplicate: source ids append uniquely, proof_count grows by the NEW
    ids, the prior form is snapshotted to observation_history (S6), and a
    re-cite of only-known ids is an idempotent no-op. A control aggregate in
    another scope stays unchanged.
    """
    conn = db

    # The aggregate under test, seeded with one founding correction.
    oid = reflect_db.add_observation(
        "team prefers small single-concern commits",
        category="Process",
        scope="project",
        source_correction_ids=["c1"],
        conn=conn,
    )
    # CONTROL in a different scope, never referenced by the UPDATE.
    control = reflect_db.add_observation(
        "ops uses terraform for all infra",
        category="Infra",
        scope="ops",
        source_correction_ids=["o1"],
        conn=conn,
    )
    control_before = _control_snapshot(conn, control)

    before = reflect_db.get_observation(oid, conn=conn)
    assert before["proof_count"] == 1
    assert reflect_db.get_observation_history(oid, conn=conn) == [], (
        "no history before the first mutation"
    )

    # ---- ACT: UPDATE folds in two NEW corrections plus an evolved wording. ----
    evolved = "team prefers small single-concern commits, rebased before push"
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "UPDATE",
            "target_id": oid,
            "content": evolved,
            "source_correction_ids": ["c1", "c2", "c3"],  # c1 is already recorded
        }],
    )
    assert summary["updated"] == 1 and summary["errors"] == [], (
        f"the UPDATE must merge new evidence onto the existing aggregate; got {summary}"
    )

    # SAME aggregate — no sibling spawned.
    rows = reflect_db.get_observations(scope="project", conn=conn)
    assert len(rows) == 1, (
        f"UPDATE must fold into the existing aggregate, not create a sibling; got {rows}"
    )
    after = reflect_db.get_observation(oid, conn=conn)
    assert after["proof_count"] == 3, (
        "proof_count grows by the count of NEW ids only (c2, c3 = +2 over 1); "
        f"c1 was already recorded so it does not double-count; got {after['proof_count']}"
    )
    assert json.loads(after["source_correction_ids"]) == ["c1", "c2", "c3"], (
        "new correction ids append uniquely; the already-recorded id is not duplicated"
    )
    assert after["content"] == evolved, (
        "the aggregate wording evolves to reflect the current convention"
    )

    # S6 non-destructive contract: the PRIOR form survives in history.
    history = reflect_db.get_observation_history(oid, conn=conn)
    assert len(history) == 1, (
        f"the UPDATE must snapshot the prior form exactly once; got {history}"
    )
    snap = json.loads(history[0]["snapshot_json"])
    assert snap["content"] == before["content"] and snap["proof_count"] == 1, (
        "the snapshot must capture the PRE-update wording and evidence count — "
        f"the old form is never lost; got {snap}"
    )

    # IDEMPOTENT re-run: re-citing only already-recorded ids changes nothing.
    rerun = reflect_cascade.execute_observation_actions(
        [{
            "action": "UPDATE",
            "target_id": oid,
            "source_correction_ids": ["c1", "c2", "c3"],
        }],
    )
    assert rerun["updated"] == 0 and rerun["skipped"] == 1, (
        f"re-citing only known ids must be an idempotent no-op; got {rerun}"
    )
    assert reflect_db.get_observation(oid, conn=conn)["proof_count"] == 3, (
        "an idempotent re-run must NOT inflate proof_count beyond the real evidence"
    )

    # CONTROL untouched across both passes.
    assert _control_snapshot(conn, control) == control_before, (
        "the ops-scoped control aggregate must be unchanged by the project UPDATEs"
    )


# ── Arm 3: DELETE retires the aggregate non-destructively (active tier drops) ──

def test_O1_delete_retires_aggregate_nondestructively(db):
    """A DELETE retires the aggregate: it drops out of the default (active)
    tier yet the row survives with status 'retired' + reason, and a history
    snapshot records the retirement. A co-scoped survivor and a control in
    another scope both remain active.
    """
    conn = db

    keep = reflect_db.add_observation(
        "this codebase uses ruff for linting",
        category="Tooling",
        scope="project",
        source_correction_ids=["k1"],
        conn=conn,
    )
    drop = reflect_db.add_observation(
        "this codebase deploys only on fridays",
        category="Process",
        scope="project",
        source_correction_ids=["d1"],
        conn=conn,
    )
    control = reflect_db.add_observation(
        "docs are written in mkdocs",
        category="Docs",
        scope="docs",
        source_correction_ids=["x1"],
        conn=conn,
    )
    control_before = _control_snapshot(conn, control)

    active_before = {r["id"] for r in reflect_db.get_observations(scope="project", conn=conn)}
    assert active_before == {keep, drop}

    # ---- ACT: DELETE (retire) the friday-deploy convention. ----
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "DELETE",
            "target_id": drop,
            "reason": "moved to continuous deploy — friday rule no longer holds",
        }],
    )
    assert summary["deleted"] == 1 and summary["errors"] == [], (
        f"DELETE must retire exactly one aggregate; got {summary}"
    )

    # The active tier now excludes the retired aggregate, keeps the survivor.
    active_after = {r["id"] for r in reflect_db.get_observations(scope="project", conn=conn)}
    assert active_after == {keep}, (
        "the retired aggregate must drop out of the active tier; the co-scoped "
        f"survivor stays; got {active_after}"
    )

    # NON-destructive: the row survives with retired status + reason + history.
    retired = reflect_db.get_observation(drop, conn=conn)
    assert retired is not None, "retire must NOT hard-delete the row"
    assert retired["status"] == reflect_db.OBSERVATION_STATUS_RETIRED
    assert "continuous deploy" in retired["retired_reason"], (
        "the retire reason must be recorded so 'why did this stop holding?' is answerable"
    )
    assert drop in {
        r["id"] for r in reflect_db.get_observations(
            scope="project", include_retired=True, conn=conn
        )
    }, "include_retired must still surface the retired row — it was retired, not deleted"

    history = reflect_db.get_observation_history(drop, conn=conn)
    assert len(history) == 1 and history[0]["change_type"] == "retired", (
        f"the retirement must leave exactly one 'retired' history snapshot; got {history}"
    )

    # CONTROL in another scope is untouched.
    assert _control_snapshot(conn, control) == control_before, (
        "the docs-scoped control aggregate must be unchanged by the project DELETE"
    )
    assert keep != drop
