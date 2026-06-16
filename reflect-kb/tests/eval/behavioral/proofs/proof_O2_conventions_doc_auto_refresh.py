# ABOUTME: Behavioral proof for O2 — auto-refreshing per-project conventions doc.
# ABOUTME: Drives the REAL reflect_cascade.execute_observation_actions + the real
# ABOUTME: conventions_generator/reflect_db over an on-disk reflect.db: an observation
# ABOUTME: action (CREATE/UPDATE/DELETE) landing in a doc's scope REGENERATES that
# ABOUTME: doc inline (new convention in the body, last_refreshed_at advances), while a
# ABOUTME: doc whose scope was NOT touched — and a no-op action batch — leave it untouched.
"""O2 auto-refreshing conventions-doc proof.

Port O2 (bead agents-in-a-box-kdo.36) is a STORAGE port, not a file-engine
recall port. Its behaviour lives entirely in ``plugins/reflect/scripts/``
(``reflect_cascade.py``, ``conventions_generator.py``, ``reflect_db.py``) — so
this proof drives those real modules against an on-disk ``reflect.db`` sqlite
store + a tmp state dir directly. No LLM, no torch model, no vector engine is
involved: the seeds plus the literal observation-action objects fully determine
every asserted outcome. (In production the drain's LLM only *chooses* which
CREATE/UPDATE/DELETE observation action to emit; here we hand the executor the
actions verbatim, so the assertions test the executor's deterministic state
transitions and the deterministic stdlib markdown render, never an LLM
decision.)

The TRUE invariant (corrected against the real diff at 6b3a343c —
``feat(reflect): auto-refreshing per-project conventions doc (O2)``):

When ``execute_observation_actions`` applies a CREATE / UPDATE / DELETE to an
observation, it back-reacts on the conventions layer via
``trigger_conventions_refresh`` -> ``conventions_generator.refresh_for_scope``:

  REFRESH CONDITION (the trigger predicate, deterministic, stdlib-only):
    a registered ``conventions_docs`` row "covers" a touched scope when that
    scope appears in the row's ``scope_tags`` OR equals its ``project_id``
    (``scope='global'`` covers EVERY row). Every covered doc is regenerated.

  WHEN THE CONDITION HOLDS (refresh fires):
    * the doc is re-rendered as deterministic markdown over the LIVE
      observations table and re-materialized on disk at ``doc_path``;
    * ``upsert_conventions_doc`` moves ``last_refreshed_at`` to now and clears
      the stored staleness flag;
    * the summary reports ``conventions_refreshed`` >= 1.
    Because regeneration is deterministic markdown (no LLM on the path), it
    happens INLINE — unlike R13 skill refreshes which queue a drain task.

  WHEN THE CONDITION DOES NOT HOLD (refresh skipped — the control):
    * a registered doc whose scope was NOT touched keeps its old body and its
      old ``last_refreshed_at`` (it is left out of the regeneration);
    * a no-op / malformed action batch (no scope touched at all) reports
      ``conventions_refreshed == 0`` and moves nothing.

  CONTENT REFLECTS THE CHANGE (why regeneration matters):
    a CREATE puts its convention text into the regenerated body; an UPDATE
    bumps the rendered evidence count; a DELETE/retire drops the retired
    convention out of the body — the doc is a live digest of the table, not a
    frozen snapshot.

Why no LLM: the refresh predicate is pure scope-set membership; the doc body is
``render_conventions_md`` — a literal f-string over the observations rows; the
``last_refreshed_at`` move and flag-clear are a deterministic SQL upsert. The
seeds (observation content / category / scope, the registered doc's scope_tags)
plus the literal action objects fully determine the regenerated body, the
refreshed-count, and which docs move. Nothing asserted here was decided by a
model.

Falsifiability: if the trigger never fired, Arm 1's in-scope doc body would not
gain the new convention and its ``last_refreshed_at`` would not move, and
``conventions_refreshed`` would be 0. If the predicate ignored scope, Arm 1's
out-of-scope control doc would ALSO regenerate. If regeneration read a stale
cache instead of the live table, Arm 2's evidence count would not rise and the
retired convention would survive in Arm 3's body. If a no-op batch still
refreshed, Arm 4 would move the timestamp. Each arm seeds its OWN fresh DB +
state dir (no cross-arm contamination).

PORT: O2
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the R13 storage proof does so this runs from either checkout layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import conventions_generator  # noqa: E402
import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402


@pytest.fixture
def db(tmp_path, monkeypatch):
    """Fresh isolated on-disk reflect DB wired as the MODULE-DEFAULT connection.

    reflect_cascade's observation executor, the O2 trigger
    (``trigger_conventions_refresh``), and the generator
    (``refresh_for_scope`` -> ``generate_conventions_doc`` ->
    ``upsert_conventions_doc``) all call reflect_db helpers WITHOUT a conn=
    argument (production shape), so they resolve via reflect_db.get_conn.
    Pointing get_conn at this sandbox makes the real modules drive THIS db, not
    the developer's ~/.reflect.

    REFLECT_STATE_DIR is redirected too: conventions_generator materializes the
    CONVENTIONS.md under ``<state>/conventions/<project>/``, so the doc file
    lands inside the per-test tmp tree, never ~/.reflect.
    """
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    yield connection
    reflect_db.close_all()


def _doc_row(conn, project_id: str) -> dict:
    return reflect_db.get_conventions_doc(project_id, conn=conn)


def _doc_body(conn, project_id: str) -> str:
    """The on-disk CONVENTIONS.md body for *project_id* (must be materialized)."""
    path = Path(_doc_row(conn, project_id)["doc_path"])
    assert path.is_file(), f"doc file not materialized for {project_id!r}: {path}"
    return path.read_text(encoding="utf-8")


# ── Arm 1: CREATE regenerates the in-scope doc, SKIPS the out-of-scope control ──

def test_O2_create_refreshes_in_scope_doc_and_skips_control(db, tmp_path):
    """The decisive knob: an in-scope observation action regenerates the doc;
    a doc whose scope was not touched is left untouched.

    Two registered docs:
      * IN-SCOPE  — project ``alpha`` (scope_tags include ``alpha``).
      * OUT-OF-SCOPE — project ``beta`` (scope_tags include ``beta`` only).
    A CREATE observation in scope ``alpha`` must regenerate alpha's doc (new
    convention in the body, last_refreshed_at advances) and leave beta's doc
    and timestamp exactly as they were.
    """
    conn = db

    # Register both docs from empty observation tables (no conventions yet).
    conventions_generator.generate_conventions_doc("alpha", scopes=["alpha"], conn=conn)
    conventions_generator.generate_conventions_doc("beta", scopes=["beta"], conn=conn)

    alpha_before = _doc_row(conn, "alpha")
    beta_before = _doc_row(conn, "beta")
    beta_body_before = _doc_body(conn, "beta")
    assert alpha_before["observation_count"] == 0
    assert "uv over pip" not in _doc_body(conn, "alpha")

    # ---- ingest a CREATE observation scoped to alpha. ----
    convention = "alpha prefers uv over pip for all python dependency management"
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "CREATE",
            "content": convention,
            "category": "Tooling",
            "scope": "alpha",
            "source_correction_ids": ["corr-a1"],
        }],
    )

    assert summary["created"] == 1 and summary["errors"] == [], (
        f"the CREATE must insert exactly one observation; got {summary}"
    )

    # THE TRIGGER FIRED: at least one doc regenerated, and it was alpha's.
    assert summary["conventions_refreshed"] >= 1, (
        "an in-scope observation action must regenerate at least one conventions "
        f"doc inline; got conventions_refreshed={summary['conventions_refreshed']}"
    )

    alpha_after = _doc_row(conn, "alpha")
    assert alpha_after["observation_count"] == 1, (
        "alpha's doc must now aggregate the new observation"
    )
    assert convention in _doc_body(conn, "alpha"), (
        "the regenerated doc body MUST contain the new convention text — the doc "
        "is a live digest of the observations table, regenerated by the trigger"
    )
    assert alpha_after["last_refreshed_at"] > alpha_before["last_refreshed_at"], (
        "regenerating alpha's doc must move its last_refreshed_at forward"
    )

    # THE CONTROL DID NOT FIRE: beta's doc is byte-for-byte unchanged.
    beta_after = _doc_row(conn, "beta")
    assert beta_after["last_refreshed_at"] == beta_before["last_refreshed_at"], (
        "a doc whose scope was NOT touched must keep its old last_refreshed_at — "
        "the trigger is scoped to covering docs, not every registered doc"
    )
    assert _doc_body(conn, "beta") == beta_body_before, (
        "the out-of-scope control doc body must be identical to before the CREATE"
    )
    assert beta_after["observation_count"] == 0, (
        "the alpha-scoped observation must not leak into beta's digest"
    )


# ── Arm 2: UPDATE re-renders the doc with the new evidence count ──────────────

def test_O2_update_regenerates_doc_with_higher_evidence_count(db, tmp_path):
    """An UPDATE that adds evidence to an observation re-renders the doc so its
    rendered ``(evidence ×N)`` count rises — proving regeneration reads the
    LIVE table, not a frozen snapshot.
    """
    conn = db

    # Seed one observation in scope gamma, then register the doc over it.
    oid = reflect_db.add_observation(
        "gamma deploys via fly, never heroku",
        category="Deploy",
        scope="gamma",
        source_correction_ids=["c1"],
        conn=conn,
    )
    conventions_generator.generate_conventions_doc("gamma", scopes=["gamma"], conn=conn)

    body_before = _doc_body(conn, "gamma")
    assert "_(evidence ×1)_" in body_before, (
        f"a single-corroboration observation renders evidence ×1; got:\n{body_before}"
    )

    # ---- UPDATE: cite TWO new corrections as further evidence. ----
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "UPDATE",
            "target_id": oid,
            "source_correction_ids": ["c2", "c3"],
        }],
    )
    assert summary["updated"] == 1 and summary["errors"] == [], (
        f"the UPDATE must merge new evidence; got {summary}"
    )
    assert summary["conventions_refreshed"] >= 1, (
        "an UPDATE in-scope must regenerate the covering doc"
    )

    body_after = _doc_body(conn, "gamma")
    assert "_(evidence ×3)_" in body_after, (
        "the regenerated body must show the bumped evidence count (1 seed + 2 new "
        f"corrections = 3) — regeneration reads the live table; got:\n{body_after}"
    )
    assert "_(evidence ×1)_" not in body_after, (
        "the stale ×1 render must be gone — the doc was fully re-rendered"
    )


# ── Arm 3: DELETE/retire drops the retired convention out of the doc body ─────

def test_O2_delete_drops_retired_convention_from_doc(db, tmp_path):
    """A DELETE (retire) regenerates the doc so the retired convention no longer
    appears, while a co-scoped survivor stays — the doc tracks belief revision.
    """
    conn = db

    keep = reflect_db.add_observation(
        "delta uses ruff for linting",
        category="Tooling",
        scope="delta",
        source_correction_ids=["k1"],
        conn=conn,
    )
    drop = reflect_db.add_observation(
        "delta deploys on fridays",
        category="Process",
        scope="delta",
        source_correction_ids=["d1"],
        conn=conn,
    )
    conventions_generator.generate_conventions_doc("delta", scopes=["delta"], conn=conn)

    body_before = _doc_body(conn, "delta")
    assert "delta deploys on fridays" in body_before
    assert "delta uses ruff for linting" in body_before
    assert _doc_row(conn, "delta")["observation_count"] == 2

    # ---- DELETE (retire) the friday-deploy convention. ----
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "DELETE",
            "target_id": drop,
            "reason": "delta moved to continuous deploy — no more friday rule",
        }],
    )
    assert summary["deleted"] == 1 and summary["errors"] == [], (
        f"DELETE must retire exactly one observation; got {summary}"
    )
    assert summary["conventions_refreshed"] >= 1, (
        "a DELETE in-scope must regenerate the covering doc"
    )

    body_after = _doc_body(conn, "delta")
    assert "delta deploys on fridays" not in body_after, (
        "a retired convention MUST drop out of the regenerated body — the doc is a "
        f"live digest, so retirement removes it; got:\n{body_after}"
    )
    assert "delta uses ruff for linting" in body_after, (
        "the surviving co-scoped convention must remain — only the retired one drops"
    )
    assert _doc_row(conn, "delta")["observation_count"] == 1, (
        "the doc must now aggregate only the one active observation"
    )
    # Sanity: the original observation id was the one referenced (no accidental keep==drop).
    assert keep != drop


# ── Arm 4: a no-op action batch touches no scope and moves nothing ────────────

def test_O2_noop_batch_refreshes_nothing(db, tmp_path):
    """The negative control: an action batch that executes NO state change
    touches no scope, so ``conventions_refreshed == 0`` and a registered doc's
    last_refreshed_at does not move. This proves the refresh is driven by a
    real observation change, not merely by calling the executor.
    """
    conn = db

    conventions_generator.generate_conventions_doc("epsilon", scopes=["epsilon"], conn=conn)
    before = _doc_row(conn, "epsilon")
    body_before = _doc_body(conn, "epsilon")

    # An UPDATE targeting a non-existent observation: skipped, never executed,
    # so no scope is added to scopes_touched -> the trigger never runs.
    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "UPDATE",
            "target_id": "does-not-exist",
            "source_correction_ids": ["x1"],
        }],
    )
    assert summary["executed"] == 0, (
        f"a malformed/missing-target action must execute nothing; got {summary}"
    )
    assert summary["conventions_refreshed"] == 0, (
        "no scope was touched, so the conventions trigger must NOT fire — "
        f"calling the executor alone does not refresh; got {summary}"
    )

    after = _doc_row(conn, "epsilon")
    assert after["last_refreshed_at"] == before["last_refreshed_at"], (
        "with no observation change the registered doc's last_refreshed_at must "
        "stay frozen"
    )
    assert _doc_body(conn, "epsilon") == body_before, (
        "the doc body must be byte-identical after a no-op batch"
    )
