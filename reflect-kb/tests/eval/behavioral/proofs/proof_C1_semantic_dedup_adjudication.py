# ABOUTME: Behavioral proof for C1 — per-ingest semantic-dedup adjudication. Drives the REAL
# ABOUTME: reflect_cascade.execute_revision_actions + find_semantic_twin (incl. the real
# ABOUTME: `reflect embed` subprocess into the all-mpnet-base-v2 index model) against an
# ABOUTME: on-disk reflect_db: a near-duplicate CREATE is HELD for adjudication (no second
# ABOUTME: row), while a semantically-distinct CREATE lands a new row.
"""C1 per-ingest semantic-dedup adjudication proof.

Port C1 (bead agents-in-a-box-kdo.22) is a STORAGE/STATE port. Its behaviour
lives in ``plugins/reflect/scripts/reflect_cascade.py`` — the CREATE arm of
``execute_revision_actions`` now first probes the existing learnings for a
semantic twin (``find_semantic_twin``) by embedding cosine BEFORE the row is
written. At/above the dedup threshold (default 0.97; env
``REFLECT_DEDUP_THRESHOLD`` or ``[cascade].dedup_threshold``; ``>= 1.0``
disables) the CREATE does NOT land: it is held under ``adjudications`` with a
focused 'merge?' question and ``needs_adjudication`` is incremented. A
semantically-distinct CREATE has no twin and lands a real new row.

This proof drives the real module + the real on-disk ``reflect_db`` directly,
and — crucially — the cosine that decides 'twin or not' is computed by the REAL
``reflect embed`` SUBPROCESS into ``all-mpnet-base-v2`` (the exact model
nano-graphrag indexes chunks with, so the dedup similarity lives in the index's
own embedding space). Nothing in C1's path is replaced: ``find_semantic_twin``
issues its real non-retired-candidate DB query, shells out to the real
``reflect embed`` for vectors, computes its real ``_cosine``, and applies the
real threshold; the executor makes the real hold-or-land decision. The only
test wiring is pointing ``_find_reflect_cli`` at the eval-venv ``reflect``
binary (the same full-stack CLI the recall proofs use) so the subprocess
resolves.

No LLM participates in any assertion — the seeds, the literal CREATE actions,
the real model's deterministic vectors, and the 0.97 floor fully determine
every outcome. (The drain LLM only *answers* an adjudication in production;
here we never need it, because the assertions test whether the row LANDED or was
HELD — a pure storage state the executor decides, not an LLM verdict.)

The TRUE invariant (corrected against the real diff in 006d4dd3):

  reflect_cascade.execute_revision_actions, on a CREATE without
  ``"dedup_adjudicated": true``, probes existing non-retired learnings via
  find_semantic_twin (embedding cosine in the index model's space) such that:

  1. NEAR-DUPLICATE IS HELD, NOT WRITTEN. A CREATE whose text is >= 0.97 cosine
     to an existing learning (the seed pair embeds at ~0.99 by the real model)
     does NOT land a second row. summary ``needs_adjudication == 1``,
     ``created == 0``, the learnings row count does NOT grow, and the single
     adjudication names the existing twin (its id + title), carries the new
     text and a real similarity >= 0.97, and asks the focused 'merge?' question.

  2. DISTINCT CREATE LANDS A NEW ROW (the control). A CREATE on an unrelated
     topic (real cosine ~0.0, far below threshold) has NO twin: it is written.
     ``created == 1``, ``needs_adjudication == 0``, the row count grows by one.
     Same path, same seed corpus, same probe — only the *semantic* distance
     differs, so the probe (not text luck or a blanket block) held the dup.

  3. KNOB ON -> OFF, two ways the diff documents:
     (a) threshold ``>= 1.0`` DISABLES the probe (Hindsight's ``_dedup_active``
         contract). With ``REFLECT_DEDUP_THRESHOLD=1.0`` the very SAME near-
         duplicate CREATE from arm 1 now LANDS a second row
         (``created == 1``, ``needs_adjudication == 0``) — proving the hold in
         arm 1 was caused by the dedup threshold knob, not anything else.
     (b) ``"dedup_adjudicated": true`` BYPASSES the probe (the keep verdict —
         the adjudicator already read both texts and ruled them distinct). The
         same near-duplicate CREATE, flagged, LANDS — and the probe is never
         even consulted on that path (asserted with a tripwire).

Falsifiability: if C1's probe were absent, arm 1 would see ``created == 1`` and
a second row. If the probe blocked blindly (not on cosine), arm 2's distinct
CREATE would also be held. If the ``>= 1.0`` disable were not wired, arm 3a
would still hold. If ``dedup_adjudicated`` did not bypass, arm 3b would hold
(and the tripwire would fire).

PORT: C1
"""
from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the S5/SG1 storage proofs do so this runs from either checkout layout.
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


# ── seeds ────────────────────────────────────────────────────────────────────
# An anchor learning, a near-duplicate paraphrase (real-model cosine ~0.9935 —
# comfortably >= 0.97), and a semantically unrelated CREATE (~0.0 cosine).
_ANCHOR_TITLE = (
    "Run database migrations inside a transaction so they roll back on failure"
)
_NEAR_DUP_TEXT = (
    "Run DB migrations inside a transaction so they roll back on failure"
)
_DISTINCT_TEXT = "Use ripgrep instead of grep for plain text search"


def _resolve_reflect_cli() -> str:
    """The full-stack `reflect` CLI (with the `embed` subcommand) the recall
    proofs use. RECALL_EVAL_BIN_DIR is set by the eval runner; fall back to a
    PATH lookup. Skip cleanly if neither resolves — the probe needs it."""
    bin_dir = os.environ.get("RECALL_EVAL_BIN_DIR")
    if bin_dir:
        cand = Path(bin_dir) / "reflect"
        if cand.exists():
            return str(cand)
    found = shutil.which("reflect")
    if found:
        return found
    pytest.skip(
        "`reflect` CLI (with `embed`) not resolvable — set RECALL_EVAL_BIN_DIR "
        "to the full-stack eval venv bin dir"
    )


@pytest.fixture
def reflect_cli(monkeypatch):
    """Point C1's probe at the real full-stack `reflect` binary.

    ``find_semantic_twin`` resolves the embedder via
    ``reflect_cascade._find_reflect_cli`` (production: ``shutil.which('reflect')``).
    The system `reflect` on a dev box may be a slim build without `embed`, so we
    pin it to the eval venv binary that recall proofs already rely on. The REAL
    ``_fetch_dedup_embeddings`` subprocess is left untouched — this is the
    genuine production transport.
    """
    cli = _resolve_reflect_cli()
    monkeypatch.setattr(reflect_cascade, "_find_reflect_cli", lambda: cli)
    return cli


@pytest.fixture
def db(tmp_path, monkeypatch):
    """Fresh isolated on-disk reflect DB wired as the MODULE-DEFAULT connection.

    reflect_cascade's executor and find_semantic_twin call reflect_db helpers
    WITHOUT a conn= argument (production shape), resolving via
    reflect_db.get_conn. Pointing get_conn at this sandbox makes the real module
    drive THIS db, not the developer's ~/.reflect. The dedup threshold is pinned
    to the documented default so a local reflect.toml override can never flip an
    outcome.
    """
    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "0.97")
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


def _learning_count(conn) -> int:
    return conn.execute("SELECT COUNT(*) FROM learnings").fetchone()[0]


def _seed_anchor(conn) -> str:
    return reflect_db.add_learning(
        title=_ANCHOR_TITLE,
        category="tooling",
        confidence="high",
        scope="project",
        source_memory_ids=["transcript-anchor"],
        conn=conn,
    )


# ── arm 1: near-duplicate CREATE is HELD, not written ────────────────────────

def test_C1_near_duplicate_create_is_held_not_written(db, reflect_cli):
    """A >=0.97-cosine CREATE is held under adjudications; no second row lands."""
    conn = db
    anchor_id = _seed_anchor(conn)
    assert _learning_count(conn) == 1

    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": _NEAR_DUP_TEXT, "category": "tooling"}],
        source_memory_id="transcript-dup",
    )

    assert summary["needs_adjudication"] == 1 and summary["created"] == 0, (
        "a near-duplicate CREATE must be HELD for adjudication, not created; "
        f"got {summary}"
    )
    assert summary["errors"] == [], f"unexpected errors: {summary['errors']}"
    assert _learning_count(conn) == 1, (
        "the near-duplicate row must NOT land — the whole point of the C1 probe; "
        f"row count grew to {_learning_count(conn)}"
    )

    adj = summary["adjudications"][0]
    assert adj["existing_id"] == anchor_id, (
        f"the adjudication must name the existing twin {anchor_id}; got {adj['existing_id']}"
    )
    assert adj["existing_title"] == _ANCHOR_TITLE
    assert adj["new_text"] == _NEAR_DUP_TEXT
    assert adj["similarity"] >= 0.97, (
        "the held CREATE must be at/above the dedup floor by the REAL model; "
        f"got cosine {adj['similarity']}"
    )
    assert adj["threshold"] == pytest.approx(0.97)
    # The focused 1-by-1 merge question carries both verdict paths.
    assert adj["question"].startswith("merge?")
    assert anchor_id in adj["question"]


# ── arm 2: semantically-distinct CREATE lands a new row (control) ─────────────

def test_C1_distinct_create_lands_new_row(db, reflect_cli):
    """An unrelated CREATE has no twin and is written — proves the probe is semantic."""
    conn = db
    _seed_anchor(conn)
    assert _learning_count(conn) == 1

    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": _DISTINCT_TEXT, "category": "tooling"}],
        source_memory_id="transcript-distinct",
    )

    assert summary["created"] == 1 and summary["needs_adjudication"] == 0, (
        "a semantically-distinct CREATE (far below the cosine floor) must LAND, "
        f"not be held; got {summary}"
    )
    assert summary["errors"] == [], f"unexpected errors: {summary['errors']}"
    assert _learning_count(conn) == 2, (
        "the distinct learning must add a real new row — same path, same corpus, "
        f"only the semantic distance differs; row count is {_learning_count(conn)}"
    )


# ── arm 3a: knob OFF — threshold >= 1.0 disables the probe ────────────────────

def test_C1_threshold_disable_lets_near_duplicate_land(db, reflect_cli, monkeypatch):
    """The SAME near-duplicate that was held now LANDS once the knob disables dedup.

    This is the decisive knob-on -> knob-off flip: only the threshold changed
    (0.97 -> 1.0), so the hold in arm 1 was caused by the dedup probe, not by
    incidental text or a blanket block.
    """
    conn = db
    _seed_anchor(conn)
    assert _learning_count(conn) == 1

    # threshold >= 1.0 -> find_semantic_twin returns None before any embedding.
    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "1.0")

    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": _NEAR_DUP_TEXT, "category": "tooling"}],
        source_memory_id="transcript-dup",
    )

    assert summary["created"] == 1 and summary["needs_adjudication"] == 0, (
        "with the dedup probe disabled (threshold >= 1.0) the near-duplicate "
        f"CREATE must LAND, proving the knob caused arm 1's hold; got {summary}"
    )
    assert _learning_count(conn) == 2, (
        "the knob-OFF path must let the duplicate row land; row count is "
        f"{_learning_count(conn)}"
    )


# ── arm 3b: dedup_adjudicated bypass — the keep verdict lands without probing ──

def test_C1_dedup_adjudicated_flag_bypasses_probe(db, monkeypatch):
    """A near-duplicate flagged "dedup_adjudicated": true lands and never probes.

    This is the 'keep both' verdict: the adjudicator already read both texts and
    ruled them distinct, so the executor must NOT re-probe. A tripwire on
    find_semantic_twin asserts the probe is never consulted on this path AND the
    row lands. (No reflect_cli fixture: the probe must not run, so no CLI needed.)
    """
    conn = db
    _seed_anchor(conn)
    assert _learning_count(conn) == 1

    def _boom(*args, **kwargs):
        raise AssertionError("dedup_adjudicated=true must BYPASS the semantic probe")

    monkeypatch.setattr(reflect_cascade, "find_semantic_twin", _boom)

    summary = reflect_cascade.execute_revision_actions(
        [{
            "action": "CREATE",
            "content": _NEAR_DUP_TEXT,
            "category": "tooling",
            "dedup_adjudicated": True,
        }],
        source_memory_id="transcript-keep",
    )

    assert summary["created"] == 1 and summary["needs_adjudication"] == 0, (
        "a CREATE flagged dedup_adjudicated=true (keep verdict) must LAND without "
        f"a probe; got {summary}"
    )
    assert _learning_count(conn) == 2, (
        f"the kept-both row must land; row count is {_learning_count(conn)}"
    )
