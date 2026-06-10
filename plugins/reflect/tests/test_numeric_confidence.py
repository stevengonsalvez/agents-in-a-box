# ABOUTME: Regression tests for port S3 — numeric confidence (0–1). Pins the
# ABOUTME: Hindsight memory_units.confidence_score shape: confidence_num float
# ABOUTME: beside the HIGH/MEDIUM/LOW display tier in frontmatter + the
# ABOUTME: reflect.db learnings table, the tier→midpoint auto-migration
# ABOUTME: (HIGH→0.9, MEDIUM→0.6, LOW→0.3), and recall ranking by the float —
# ABOUTME: unchanged on tier-only legacy corpus, finer-grained on new notes.
"""Port S3: confidence stored as a continuous float 0–1; tiers are display
buckets mapped only at the edges (Hindsight memory_units.confidence_score).

Acceptance bullets pinned here:
  1. both fields present in new notes (template declares confidence +
     confidence_num; create_knowledge_note and reflect_db.add_learning
     write both, deriving the float from the tier midpoint when omitted)
  2. old notes auto-migrated (legacy DB without the column gets it
     backfilled per tier on init_db; tier-only recall frontmatter derives
     the float at read time)
  3. ranking unchanged on existing corpus, finer-grained on new
     (confidence_num_norm is anchored so tier midpoints reproduce the exact
     pre-S3 R8 norms; notes with a calibrated float order within buckets)
"""

from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

import pytest
import yaml

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
TEMPLATE = PLUGIN_ROOT / "assets" / "learning_template.md"
sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(RECALL_SCRIPTS))

import output_generator  # noqa: E402
import reflect_db  # noqa: E402
from recall import (  # noqa: E402
    CONFIDENCE_ALPHA,
    Learning,
    bounded_boost,
    confidence_norm,
    confidence_num_norm,
    rerank,
)


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


@pytest.fixture()
def project(tmp_path, monkeypatch):
    """Isolated non-git project dir so notes land under tmp_path."""
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    return tmp_path


def _parse_note(path: Path) -> dict:
    text = path.read_text()
    assert text.startswith("---")
    end = text.find("\n---", 3)
    return yaml.safe_load(text[3:end])


def _lrn(name: str, **fm_extra) -> Learning:
    fm: dict = {"name": name, **fm_extra}
    return Learning(chunk_text=f"learning body for {name}", frontmatter=fm)


# =========================================================================
# Acceptance 1 — both fields present in new notes
# =========================================================================

def test_learning_template_declares_both_confidence_fields():
    text = TEMPLATE.read_text()
    assert "confidence: {{CONFIDENCE}}" in text
    assert "confidence_num: {{CONFIDENCE_NUM}}" in text
    # confidence_num must sit in frontmatter, not the prose body.
    frontmatter = text.split("---")[1]
    assert "confidence_num:" in frontmatter


def test_create_knowledge_note_writes_both_fields_derived_from_tier(project):
    path, _ = output_generator.create_knowledge_note(
        title="S3 derived note", category="testing", tags=["s3"],
        symptoms=["s"], root_cause="rc", key_insight="ki",
        problem="p", solution="s", confidence="high",
    )
    fm = _parse_note(path)
    assert fm["confidence"] == "high"
    assert fm["confidence_num"] == pytest.approx(0.9)


def test_create_knowledge_note_passes_explicit_float_through(project):
    path, _ = output_generator.create_knowledge_note(
        title="S3 explicit note", category="testing", tags=[],
        symptoms=[], root_cause="rc", key_insight="ki",
        problem="p", solution="s", confidence="high", confidence_num=0.82,
    )
    fm = _parse_note(path)
    assert fm["confidence"] == "high"
    assert fm["confidence_num"] == pytest.approx(0.82)


def test_create_knowledge_note_clamps_out_of_range_float(project):
    path, _ = output_generator.create_knowledge_note(
        title="S3 clamp note", category="testing", tags=[],
        symptoms=[], root_cause="rc", key_insight="ki",
        problem="p", solution="s", confidence="low", confidence_num=1.7,
    )
    fm = _parse_note(path)
    assert fm["confidence_num"] == pytest.approx(1.0)


@pytest.mark.parametrize(
    ("tier", "expected"),
    [("HIGH", 0.9), ("MEDIUM", 0.6), ("MED", 0.6), ("LOW", 0.3)],
)
def test_add_learning_derives_confidence_num_from_tier(conn, tier, expected):
    lid = reflect_db.add_learning("s3 tier note", confidence=tier, conn=conn)
    row = reflect_db.get_learning(lid, conn=conn)
    assert row["confidence"] == tier
    assert row["confidence_num"] == pytest.approx(expected)


def test_add_learning_accepts_explicit_confidence_num(conn):
    lid = reflect_db.add_learning(
        "s3 explicit float", confidence="HIGH", confidence_num=0.97, conn=conn,
    )
    row = reflect_db.get_learning(lid, conn=conn)
    assert row["confidence_num"] == pytest.approx(0.97)


def test_add_learning_clamps_and_tolerates_bad_confidence_num(conn):
    high = reflect_db.add_learning(
        "s3 clamp high", confidence="HIGH", confidence_num=3.5, conn=conn,
    )
    low = reflect_db.add_learning(
        "s3 clamp low", confidence="LOW", confidence_num=-1.0, conn=conn,
    )
    junk = reflect_db.add_learning(
        "s3 junk float", confidence="MEDIUM", confidence_num="not-a-number",
        conn=conn,
    )
    assert reflect_db.get_learning(high, conn=conn)["confidence_num"] == 1.0
    assert reflect_db.get_learning(low, conn=conn)["confidence_num"] == 0.0
    # Unparseable → tier midpoint, never a crash.
    assert reflect_db.get_learning(junk, conn=conn)["confidence_num"] == 0.6


def test_unknown_tier_lands_mid_bucket(conn):
    lid = reflect_db.add_learning("s3 odd tier", confidence="BOGUS", conn=conn)
    assert reflect_db.get_learning(lid, conn=conn)["confidence_num"] == 0.6


# =========================================================================
# Acceptance 2 — old notes auto-migrated
# =========================================================================

def test_legacy_db_rows_backfilled_per_tier(tmp_path):
    """A pre-S3 DB (no confidence_num column) gets the column added and every
    row backfilled HIGH→0.9 / MEDIUM→0.6 / LOW→0.3 on the next init_db."""
    db_file = tmp_path / "legacy.db"
    legacy = sqlite3.connect(db_file)
    legacy.executescript(
        """
        CREATE TABLE learnings (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'Unknown',
            confidence TEXT NOT NULL DEFAULT 'LOW',
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'approved', 'rejected',
                                  'indexed', 'reverted')),
            source_tool TEXT NOT NULL DEFAULT '',
            source_path TEXT NOT NULL DEFAULT '',
            content_hash TEXT NOT NULL DEFAULT '',
            commit_hash TEXT,
            created_at TEXT NOT NULL,
            approved_at TEXT,
            indexed_at TEXT,
            reverted_at TEXT,
            revert_reason TEXT
        );
        """
    )
    for lid, tier in (
        ("lrn-high", "HIGH"), ("lrn-med", "MEDIUM"), ("lrn-low", "LOW"),
        ("lrn-odd", "weird"),
    ):
        legacy.execute(
            "INSERT INTO learnings (id, title, confidence, created_at) "
            "VALUES (?, ?, ?, '2026-01-01T00:00:00+00:00')",
            (lid, f"legacy {tier}", tier),
        )
    legacy.commit()
    legacy.close()

    conn = reflect_db.init_db(db_file)
    try:
        rows = {
            r["id"]: r["confidence_num"]
            for r in conn.execute(
                "SELECT id, confidence_num FROM learnings"
            ).fetchall()
        }
    finally:
        reflect_db.close_all()
    assert rows["lrn-high"] == pytest.approx(0.9)
    assert rows["lrn-med"] == pytest.approx(0.6)
    assert rows["lrn-low"] == pytest.approx(0.3)
    assert rows["lrn-odd"] == pytest.approx(0.6)  # unknown tier → mid-bucket


def test_migration_is_idempotent_and_preserves_explicit_floats(tmp_path):
    """Re-opening an already-migrated DB must not re-snap calibrated floats
    back to tier midpoints (the backfill runs only when the column is new)."""
    db_file = tmp_path / "migrated.db"
    conn = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning(
        "calibrated", confidence="HIGH", confidence_num=0.83, conn=conn,
    )
    reflect_db.close_all()

    conn = reflect_db.init_db(db_file)
    try:
        row = reflect_db.get_learning(lid, conn=conn)
    finally:
        reflect_db.close_all()
    assert row["confidence_num"] == pytest.approx(0.83)


def test_tier_only_recall_note_derives_float_at_read_time():
    assert _lrn("a", confidence="HIGH").confidence_num == pytest.approx(0.9)
    assert _lrn("b", confidence="medium").confidence_num == pytest.approx(0.6)
    assert _lrn("c", confidence="LOW").confidence_num == pytest.approx(0.3)
    assert _lrn("d").confidence_num == pytest.approx(0.6)  # missing → neutral


def test_recall_note_prefers_explicit_confidence_num():
    lrn = _lrn("e", confidence="HIGH", confidence_num=0.72)
    assert lrn.confidence_num == pytest.approx(0.72)
    # Display bucket still derives from the tier field.
    assert lrn.confidence == "HIGH"


def test_recall_note_numeric_legacy_confidence_used_directly():
    """Instinct-style notes carry a bare float in `confidence` — the float
    is the ranking value; the tier remains the display bucket."""
    lrn = _lrn("f", confidence=0.85)
    assert lrn.confidence_num == pytest.approx(0.85)
    assert lrn.confidence == "HIGH"


def test_recall_note_clamps_and_tolerates_malformed_values():
    assert _lrn("g", confidence_num=2.0).confidence_num == 1.0
    assert _lrn("h", confidence_num=-0.5).confidence_num == 0.0
    # Malformed float degrades to the tier path, never a crash.
    assert _lrn("i", confidence="LOW", confidence_num="junk").confidence_num \
        == pytest.approx(0.3)


# =========================================================================
# Acceptance 3 — ranking unchanged on existing corpus, finer-grained on new
# =========================================================================

@pytest.mark.parametrize("tier", ["HIGH", "MEDIUM", "LOW", "BOGUS"])
def test_norm_anchored_to_pre_s3_tier_norms(tier):
    """The numeric norm reproduces the exact R8 tier norms at the bucket
    midpoints — tier-only legacy corpora score identically to before S3."""
    midpoint = _lrn("x", confidence=tier).confidence_num
    assert confidence_num_norm(midpoint) == pytest.approx(confidence_norm(tier))


def test_norm_is_clamped_outside_bucket_anchors():
    assert confidence_num_norm(0.0) == 0.0
    assert confidence_num_norm(0.3) == pytest.approx(0.0)
    assert confidence_num_norm(0.6) == pytest.approx(0.5)
    assert confidence_num_norm(0.9) == pytest.approx(1.0)
    assert confidence_num_norm(1.0) == 1.0


def test_rerank_order_unchanged_for_tier_only_corpus():
    high = _lrn("high", confidence="HIGH")
    med = _lrn("med", confidence="MEDIUM")
    low = _lrn("low", confidence="LOW")
    ordered = rerank([low, med, high])
    assert [lrn.id for lrn in ordered] == ["high", "med", "low"]


def test_rerank_boost_matches_pre_s3_formula_for_tiers():
    """Bounded boost from the numeric path == the old tier path, exactly."""
    for tier in ("HIGH", "MEDIUM", "LOW"):
        lrn = _lrn("x", confidence=tier)
        assert bounded_boost(
            confidence_num_norm(lrn.confidence_num), CONFIDENCE_ALPHA,
        ) == pytest.approx(bounded_boost(confidence_norm(tier), CONFIDENCE_ALPHA))


def test_rerank_finer_grained_within_a_bucket():
    """Two notes in the same display bucket order by their float — the
    information the bucketed version lost at the bin edges."""
    stronger = _lrn("stronger", confidence="HIGH", confidence_num=0.95)
    weaker = _lrn("weaker", confidence="HIGH", confidence_num=0.82)
    ordered = rerank([weaker, stronger])
    assert [lrn.id for lrn in ordered] == ["stronger", "weaker"]


def test_rerank_float_orders_across_buckets_consistently():
    """A calibrated 0.7 lands between the MEDIUM and HIGH midpoints."""
    high = _lrn("high", confidence="HIGH")            # 0.9
    between = _lrn("between", confidence_num=0.7)
    med = _lrn("med", confidence="MEDIUM")            # 0.6
    ordered = rerank([med, between, high])
    assert [lrn.id for lrn in ordered] == ["high", "between", "med"]


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
