# ABOUTME: Behavioral proof for port A3 — per-row TTL (`forget_after` ISO timestamp).
# ABOUTME: Drives the REAL reflect_forget_sweep + reflect_db modules over a real sqlite DB
# ABOUTME: and real artifact files: an expired row is archived + its note moved to
# ABOUTME: .forgotten/ (excluded from the next reindex/recall), a future-TTL row and a
# ABOUTME: no-TTL row both survive untouched.
"""Port A3: per-row forget_after TTL → hourly forget sweep.

INVARIANT (storage surface, decisive by knob = the per-row `forget_after` value):
  A learning whose `forget_after` ISO-8601 timestamp lies in the PAST is pruned by
  the real sweep — its DB row flips to status='archived' + is_latest=0 (non-destructive,
  with a learning_forgotten audit event) AND its on-disk note/sidecar are moved into a
  sibling `.forgotten/` directory so the next reindex drops them from recall. A row whose
  forget_after lies in the FUTURE, and a row with NO forget_after (permanent), both
  survive the SAME sweep pass: status unchanged, is_latest=1, note still in place.

WHY NO LLM: the outcome is fully determined by the seeded `forget_after` literals and a
pinned `--now` clock. The sweep is pure datetime comparison + sqlite UPDATE + file rename —
no model, no ranker, no LLM participates in the assertion. The three rows carry IDENTICAL
query relevance (same title/body text); only the TTL differs, so the prune is provably
caused by the A3 port, not by retrieval scoring.

This is a *storage*-surface port, so per the harness contract we drive the real module
directly (reflect_forget_sweep.run_sweep → reflect_db.sweep_expired_learnings) rather than
the file-KB recall harness; recall's only A3 coupling is "the .forgotten/ move drops the
note from the next reindex", which we assert directly on the filesystem here.

PORT: A3
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

# Resolve the real reflect plugin scripts the same way conftest.py resolves recall.py:
# this file lives at reflect-kb/tests/eval/behavioral/proofs/, so parents[5] is the repo
# root where plugins/ sits alongside reflect-kb/; parents[4].parent covers a standalone
# reflect-kb checkout with the plugin as a sibling dir.
_HERE = Path(__file__).resolve()
_CANDIDATES = [
    _HERE.parents[5] / "plugins" / "reflect" / "scripts",
    _HERE.parents[4].parent / "plugins" / "reflect" / "scripts",
]
_SCRIPTS = next((p for p in _CANDIDATES if (p / "reflect_db.py").exists()), _CANDIDATES[0])
if not (_SCRIPTS / "reflect_db.py").exists():
    raise RuntimeError(f"reflect scripts not found; tried {[str(p) for p in _CANDIDATES]}")
sys.path.insert(0, str(_SCRIPTS))

import reflect_db  # noqa: E402
import reflect_forget_sweep  # noqa: E402

# Pinned literals — the whole proof is deterministic off these. The sweep clock is fixed
# to NOW so "past" / "future" are unambiguous regardless of wall-clock at run time.
NOW = "2026-06-14T12:00:00+00:00"
PAST_TTL = "2026-06-13T12:00:00+00:00"   # 1 day before NOW  → expired
FUTURE_TTL = "2026-09-01T00:00:00+00:00"  # months after NOW → live

# Identical query-relevant text on every row: only the TTL differs across arms, so any
# inclusion/exclusion difference is attributable to A3 alone, never to text relevance.
SHARED_TITLE = "avoid the legacy payments-service during the incident window"
SHARED_BODY = "Route around payments-service; the incident bridge owns it for now."


@pytest.fixture
def db(tmp_path, monkeypatch):
    """A fresh, isolated real sqlite reflect DB wired as the module default connection.

    Per-test isolation (the S4 discipline): every test gets its own tmp DB file and its
    own get_conn override, so no archived/live state leaks across arms.
    """
    db_file = tmp_path / "reflect.db"
    conn = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: conn)
    yield conn
    reflect_db.close_all()


def _note_file(workdir: Path, name: str, text: str) -> Path:
    """Write a real artifact note file (the thing the sweep moves to .forgotten/)."""
    docs = workdir / "documents"
    docs.mkdir(parents=True, exist_ok=True)
    p = docs / f"{name}.md"
    p.write_text(f"# {name}\n\n{text}\n")
    return p


def _seed(conn, *, title, forget_after, artifact_path):
    return reflect_db.add_learning(
        title,
        forget_after=forget_after,
        artifact_path=str(artifact_path),
        conn=conn,
    )


# ── ARM 1 (knob = PAST TTL): expired row is pruned from DB *and* disk ────────────────────
def test_expired_ttl_row_is_archived_and_note_moved_to_forgotten(db, tmp_path):
    note = _note_file(tmp_path, "expired", SHARED_BODY)
    lid = _seed(db, title=SHARED_TITLE, forget_after=PAST_TTL, artifact_path=note)

    # Pre-sweep: row is live and the note is in place.
    pre = reflect_db.get_learning(lid, conn=db)
    assert pre["status"] != "archived"
    assert pre["is_latest"] == 1
    assert note.is_file()

    summary = reflect_forget_sweep.run_sweep(now=NOW, dry_run=False)

    # DB side: archived, non-destructively (row still exists), is_latest cleared.
    assert summary["archived"] == 1
    assert [e["id"] for e in summary["learnings"]] == [lid]
    row = reflect_db.get_learning(lid, conn=db)
    assert row is not None, "archive must be non-destructive — row must still exist"
    assert row["status"] == "archived"
    assert row["is_latest"] == 0

    # Audit trail: a learning_forgotten event was written for exactly this row.
    events = reflect_db.get_events(reflect_db.FORGET_EVENT_TYPE, conn=db)
    assert any(ev["learning_id"] == lid for ev in events)

    # File side (the recall coupling): the note left documents/ and now lives in
    # documents/.forgotten/ — so the next reindex drops it from recall.
    assert not note.is_file(), "expired note must leave the indexed documents/ dir"
    forgotten = note.parent / ".forgotten" / note.name
    assert forgotten.is_file(), "expired note must land in the sibling .forgotten/ dir"


# ── ARM 2 (knob = FUTURE TTL): live row survives the SAME sweep ──────────────────────────
def test_future_ttl_row_survives_sweep_untouched(db, tmp_path):
    note = _note_file(tmp_path, "future", SHARED_BODY)
    lid = _seed(db, title=SHARED_TITLE, forget_after=FUTURE_TTL, artifact_path=note)

    summary = reflect_forget_sweep.run_sweep(now=NOW, dry_run=False)

    assert summary["archived"] == 0
    assert summary["learnings"] == []
    row = reflect_db.get_learning(lid, conn=db)
    assert row["status"] != "archived", "future TTL must NOT be pruned"
    assert row["is_latest"] == 1
    assert note.is_file(), "future-TTL note must stay in the indexed documents/ dir"
    assert not (note.parent / ".forgotten" / note.name).exists()


# ── ARM 3 (knob = NO TTL): permanent row survives the SAME sweep ─────────────────────────
def test_permanent_row_without_ttl_survives_sweep(db, tmp_path):
    note = _note_file(tmp_path, "permanent", SHARED_BODY)
    lid = _seed(db, title=SHARED_TITLE, forget_after=None, artifact_path=note)

    summary = reflect_forget_sweep.run_sweep(now=NOW, dry_run=False)

    assert summary["archived"] == 0
    row = reflect_db.get_learning(lid, conn=db)
    assert row["status"] != "archived", "absent forget_after = permanent (never pruned)"
    assert row["is_latest"] == 1
    assert note.is_file()


# ── ARM 4: dry-run reports the expired row but mutates NOTHING (DB or disk) ───────────────
def test_dry_run_reports_but_does_not_mutate(db, tmp_path):
    note = _note_file(tmp_path, "expired_dry", SHARED_BODY)
    lid = _seed(db, title=SHARED_TITLE, forget_after=PAST_TTL, artifact_path=note)

    summary = reflect_forget_sweep.run_sweep(now=NOW, dry_run=True)

    # dry-run reports the candidate but counts zero archived and touches nothing.
    assert summary["dry_run"] is True
    assert summary["archived"] == 0
    assert [e["id"] for e in summary["learnings"]] == [lid]
    row = reflect_db.get_learning(lid, conn=db)
    assert row["status"] != "archived", "dry-run must not archive"
    assert row["is_latest"] == 1
    assert note.is_file(), "dry-run must not move the note file"
    assert not (note.parent / ".forgotten" / note.name).exists()
