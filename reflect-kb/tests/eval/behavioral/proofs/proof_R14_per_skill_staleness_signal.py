# ABOUTME: Behavioral proof for port R14 — per-skill computed staleness flag.
# ABOUTME: Drives the REAL reflect_db.compute_skill_is_stale / get_skills(compute_stale=)
# ABOUTME: over a real sqlite DB: a skill whose backing learning's freshness signal moved
# ABOUTME: PAST its last_refreshed_at is flagged stale, an untouched sibling skill is not,
# ABOUTME: and flipping the signal back BEFORE the refresh flips the flag back to fresh —
# ABOUTME: the flag is a pure function of the real freshness signal, no LLM, no torch model.
"""Port R14: per-skill staleness flag computed on read (hindsight
``compute_mental_model_is_stale`` shape).

INVARIANT (storage surface, decisive by the freshness SIGNAL):
  Each indexed skill carries a computed ``is_stale`` flag derived from a real
  freshness signal — the effective update time of its in-scope learnings
  (creation ``created_at``, or the newest ``learning_history`` snapshot for any
  mutation) versus the skill's own ``last_refreshed_at``. A skill is stale iff
  some in-scope learning changed AFTER the skill was last refreshed.

  Two sibling skills index the SAME DB. Skill A's backing learning is mutated
  so its history timestamp lands AFTER A's refresh → A flips stale. Skill B's
  backing learning is untouched (signal stays BEFORE B's refresh) → B stays
  fresh. This isolates the port: same engine, same read, only the per-skill
  signal differs.

  Then we FLIP THE SIGNAL: move A's backing-learning history timestamp back to
  BEFORE A's refresh and the computed flag flips back to fresh; move it forward
  again and it flips stale again. The flag tracks the signal in both
  directions, proving it is *computed from* the signal, not an incidental
  stored bit. The stored R13 column is never written — read-time only.

WHY NO LLM / NO TORCH: every assertion is over (a) sqlite rows written by the
real reflect_db ops and (b) the deterministic datetime comparison inside
``compute_skills_staleness``. The computation is pure: parse two ISO-8601
timestamps, compare, AND a tokenized scope match. No embedding model, no
cross-encoder, no ranker, no LLM participates. The only thing that flips the
flag is the pinned timestamp literal we move across the refresh floor.

This is a *storage*-surface port (the bead labels it retrieval because the flag
feeds the R11 inject short-circuit, but the runtime-observable invariant lives
in reflect_db), so per the harness contract we drive the real module directly
rather than the file-KB recall harness.

PORT: R14
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

# Resolve the real reflect plugin scripts the same way conftest.py / proof_A3 do:
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

# Pinned timestamp literals — the whole proof is deterministic off these. Refresh sits
# strictly between the OLD signal (fresh) and the NEW signal (stale), so a learning whose
# effective update time crosses the refresh floor unambiguously flips the flag.
OLD = "2026-01-01T00:00:00+00:00"   # before any skill refresh  → fresh
REFRESH = "2026-02-01T00:00:00+00:00"  # the skill's last_refreshed_at floor
NEW = "2026-03-01T00:00:00+00:00"   # after the skill refresh   → stale


@pytest.fixture
def db(tmp_path, monkeypatch):
    """A fresh, isolated real sqlite reflect DB wired as the module default connection.

    Per-test isolation: every test gets its own tmp DB file and its own get_conn override,
    so no skill/learning state leaks across arms.
    """
    db_file = tmp_path / "reflect.db"
    conn = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: conn)
    yield conn
    reflect_db.close_all()


def _seed_skill(conn, name: str, *, tags: list[str]) -> str:
    """Index one skill row, then pin its last_refreshed_at to REFRESH.

    upsert_skill always stamps last_refreshed_at = now(), so we overwrite it directly to
    pin the freshness floor deterministically (same discipline as the in-tree R14 tests).
    """
    path = f"/skills/{name}/SKILL.md"
    reflect_db.upsert_skill(name, path, tags=tags, summary=f"{name} skill.",
                            mtime=1.0, conn=conn)
    with conn:
        conn.execute("UPDATE skills SET last_refreshed_at = ? WHERE path = ?",
                     (REFRESH, path))
    return path


def _add_learning(conn, title: str, *, created_at: str) -> str:
    """Add a learning whose effective update signal (created_at + history rows) is pinned."""
    lid = reflect_db.add_learning(title, category="Unknown", conn=conn)
    _set_signal(conn, lid, created_at)
    return lid


def _set_signal(conn, lid: str, ts: str) -> None:
    """Set BOTH the learning's created_at and every learning_history row to ts.

    This is the freshness *signal* the port reads: the effective update time is
    max(created_at, newest history snapshot). Pinning both lets us move the signal
    cleanly across the refresh floor.
    """
    with conn:
        conn.execute("UPDATE learnings SET created_at = ? WHERE id = ?", (ts, lid))
        conn.execute("UPDATE learning_history SET created_at = ? WHERE learning_id = ?",
                     (ts, lid))


def _stale_of(conn, path: str) -> bool:
    """Read the computed flag through the REAL single-skill entry point."""
    return reflect_db.compute_skill_is_stale(path, conn=conn)


# ── ARM 1: changed-signal skill is flagged stale, untouched sibling is not ───────────────
def test_changed_backing_learning_flags_only_its_skill_stale(db):
    skill_a = _seed_skill(db, "fastlane", tags=["fastlane"])
    skill_b = _seed_skill(db, "kubernetes", tags=["kubernetes"])

    # A's backing learning changed AFTER A's refresh; B's stayed before B's refresh.
    _add_learning(db, "fastlane match needs the keychain unlocked", created_at=NEW)
    _add_learning(db, "kubernetes pods evict under memory pressure", created_at=OLD)

    assert _stale_of(db, skill_a) is True, "A's signal moved past its refresh → stale"
    assert _stale_of(db, skill_b) is False, "B's signal is older than its refresh → fresh"

    # Same outcome through the index-wide read; and the STORED column was never written
    # (read-time only — this is the R14 contract).
    rows = {s["path"]: s for s in reflect_db.get_skills(compute_stale=True, conn=db)}
    assert rows[skill_a]["is_stale"] == 1
    assert rows[skill_b]["is_stale"] == 0
    stored = {r["path"]: r["is_stale"] for r in
              db.execute("SELECT path, is_stale FROM skills").fetchall()}
    assert stored[skill_a] == 0, "computed staleness must not persist to the stored flag"
    assert stored[skill_b] == 0


# ── ARM 2: flip the signal → flip the flag (both directions) ─────────────────────────────
def test_flag_tracks_the_signal_in_both_directions(db):
    skill = _seed_skill(db, "fastlane", tags=["fastlane"])
    lid = _add_learning(db, "fastlane match needs the keychain unlocked", created_at=OLD)

    # Signal BEFORE refresh → fresh.
    assert _stale_of(db, skill) is False

    # Move the signal AFTER refresh → stale.
    _set_signal(db, lid, NEW)
    assert _stale_of(db, skill) is True, "signal crossed forward over the floor → stale"

    # Move it back BEFORE refresh → fresh again. The flag is recomputed from the signal,
    # not a sticky stored bit.
    _set_signal(db, lid, OLD)
    assert _stale_of(db, skill) is False, "signal moved back behind the floor → fresh"

    # And forward once more for good measure — deterministic, repeatable.
    _set_signal(db, lid, NEW)
    assert _stale_of(db, skill) is True


# ── ARM 3: control — an out-of-scope changed learning does NOT flip the flag ─────────────
def test_out_of_scope_changed_learning_does_not_flip(db):
    """The signal must be IN SCOPE: a freshly-changed learning the skill doesn't back
    leaves the flag fresh, proving the flag keys off this skill's own signal."""
    skill = _seed_skill(db, "fastlane", tags=["fastlane"])
    # New (post-refresh) learning, but nothing to do with fastlane's scope.
    _add_learning(db, "kubernetes pods evict under memory pressure", created_at=NEW)
    assert _stale_of(db, skill) is False
