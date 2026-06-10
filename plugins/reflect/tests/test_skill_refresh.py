# ABOUTME: Regression tests for port R13 — auto-skill-refresh trigger. Pins:
# ABOUTME: belief-revision UPDATE/DELETE on a learning backing a skill flips
# ABOUTME: skill.is_stale + queues a skill_refresh drain task; the drain
# ABOUTME: processes the task; stale skills are excluded from inject (R11).
"""Port R13: auto-skill-refresh trigger (hindsight
``_trigger_mental_model_refreshes`` shape).

Acceptance criteria pinned here:
  1. UPDATE on a learning in scope flips skill staleness
  2. drain processes the refresh task
  3. stale skills NOT injected (R11)

Plus the design invariants:
  - DELETE (retire) triggers the refresh too
  - out-of-scope skills (no tag overlap) are untouched
  - at most one pending skill_refresh task per skill path (queue dedup)
  - regeneration (SKILL.md mtime change) clears the flag; unchanged
    re-upserts preserve it
  - pre-R13 DBs (skills table without is_stale) migrate in place
  - the trigger is best-effort: a broken skills index never fails the
    revision write path
"""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
SKILL_DOC = PLUGIN_ROOT / "skills" / "reflect" / "SKILL.md"
sys.path.insert(0, str(SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402
import skill_index  # noqa: E402


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB + state dir per test; never touches ~/.reflect."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    yield connection
    reflect_db.close_all()


def _write_skill_md(base: Path, dirname: str, *, name: str,
                    description: str = "Does a useful thing.",
                    tags: list[str] | None = None) -> Path:
    """A minimal SKILL.md the R20 frontmatter parser understands."""
    skill_dir = base / dirname
    skill_dir.mkdir(parents=True, exist_ok=True)
    lines = ["---", f"name: {name}", "description: |", f"  {description}"]
    if tags:
        lines.append("tags:")
        lines.extend(f"  - {t}" for t in tags)
    lines.extend(["---", "", f"# {dirname}", "body"])
    path = skill_dir / "SKILL.md"
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


def _seed_skill(conn, tmp_path: Path, *, name: str = "publish",
                tags: list[str] | None = None) -> str:
    """Index one on-disk skill; returns its path (the natural key)."""
    path = _write_skill_md(
        tmp_path / "skills", name, name=name,
        tags=tags if tags is not None else ["testflight", "fastlane"],
    )
    reflect_db.upsert_skill(
        name, str(path), tags=tags if tags is not None else ["testflight", "fastlane"],
        summary="Publish apps.", mtime=path.stat().st_mtime, conn=conn,
    )
    return str(path)


def _skill_row(conn, path: str) -> sqlite3.Row:
    return conn.execute("SELECT * FROM skills WHERE path = ?", (path,)).fetchone()


def _queue_entries(tmp_path: Path) -> list[dict]:
    qfile = tmp_path / "state" / "pending_reflections.jsonl"
    if not qfile.exists():
        return []
    return [
        json.loads(line)
        for line in qfile.read_text().splitlines()
        if line.strip()
    ]


# ── acceptance 1: UPDATE on a learning in scope flips skill staleness ───────

def test_update_in_scope_flips_skill_stale_and_queues_refresh(conn, tmp_path):
    skill_path = _seed_skill(conn, tmp_path, tags=["testflight"])
    lid = reflect_db.add_learning(
        "TestFlight builds need AD_ID declaration", category="Tools", conn=conn,
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid, "reason": "restates the rule"}],
        source_memory_id="transcript-2",
    )
    assert summary["updated"] == 1 and summary["errors"] == []
    assert summary["skills_marked_stale"] == 1
    assert summary["refreshes_queued"] == 1
    assert _skill_row(conn, skill_path)["is_stale"] == 1

    entries = _queue_entries(tmp_path)
    assert len(entries) == 1
    entry = entries[0]
    assert entry["trigger"] == "skill_refresh"
    assert entry["transcript_path"] == skill_path
    assert entry["skill_name"] == "publish"
    assert entry["learning_id"] == lid
    assert entry["reason"]


def test_update_matches_on_category_tag(conn, tmp_path):
    """A skill tag equal to the learning's category is in scope."""
    skill_path = _seed_skill(conn, tmp_path, name="sec", tags=["security"])
    lid = reflect_db.add_learning(
        "Always rotate leaked credentials immediately",
        category="Security", conn=conn,
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t2",
    )
    assert summary["skills_marked_stale"] == 1
    assert _skill_row(conn, skill_path)["is_stale"] == 1


def test_update_out_of_scope_skill_untouched(conn, tmp_path):
    skill_path = _seed_skill(conn, tmp_path, tags=["kubernetes", "helm"])
    lid = reflect_db.add_learning(
        "Never use var in TypeScript", category="Code Style", conn=conn,
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t2",
    )
    assert summary["updated"] == 1
    assert summary["skills_marked_stale"] == 0
    assert summary["refreshes_queued"] == 0
    assert _skill_row(conn, skill_path)["is_stale"] == 0
    assert _queue_entries(tmp_path) == []


def test_multiword_tag_must_match_whole(conn, tmp_path):
    """'belief revision' must not fire on a title that only says 'revision'."""
    skill_path = _seed_skill(conn, tmp_path, name="rev", tags=["belief revision"])
    lid = reflect_db.add_learning(
        "Schema revision requires a migration", category="Unknown", conn=conn,
    )
    reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t2",
    )
    assert _skill_row(conn, skill_path)["is_stale"] == 0


def test_delete_in_scope_flips_skill_stale(conn, tmp_path):
    skill_path = _seed_skill(conn, tmp_path, tags=["fastlane"])
    lid = reflect_db.add_learning(
        "Use fastlane match for signing", category="Tools", conn=conn,
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "DELETE", "target_id": lid,
          "reason": "superseded: signing moved to Xcode cloud"}],
    )
    assert summary["deleted"] == 1 and summary["errors"] == []
    assert summary["skills_marked_stale"] == 1
    assert _skill_row(conn, skill_path)["is_stale"] == 1
    entries = _queue_entries(tmp_path)
    assert len(entries) == 1
    assert "Xcode cloud" in entries[0]["reason"]


def test_idempotent_update_does_not_trigger_refresh(conn, tmp_path):
    """A no-op UPDATE (same source already proved) must not flip staleness."""
    skill_path = _seed_skill(conn, tmp_path, tags=["testflight"])
    lid = reflect_db.add_learning(
        "TestFlight rule", source_memory_ids=["t1"], conn=conn,
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t1",
    )
    assert summary["updated"] == 0 and summary["skipped"] == 1
    assert summary["skills_marked_stale"] == 0
    assert _skill_row(conn, skill_path)["is_stale"] == 0


def test_refresh_queue_dedup_one_task_per_skill(conn, tmp_path):
    """Two revisions on the same skill's ground → one pending refresh task."""
    skill_path = _seed_skill(conn, tmp_path, tags=["testflight"])
    lid_a = reflect_db.add_learning("TestFlight rule A", conn=conn)
    lid_b = reflect_db.add_learning("TestFlight rule B", conn=conn)
    first = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid_a}], source_memory_id="t2",
    )
    second = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid_b}], source_memory_id="t3",
    )
    assert first["refreshes_queued"] == 1
    assert second["refreshes_queued"] == 0  # already pending
    assert len(_queue_entries(tmp_path)) == 1
    assert _skill_row(conn, skill_path)["is_stale"] == 1


def test_trigger_failure_never_breaks_the_revision(conn, tmp_path, monkeypatch):
    """A broken skills index must not fail the UPDATE itself (best-effort)."""
    lid = reflect_db.add_learning("TestFlight rule", conn=conn)
    monkeypatch.setattr(
        reflect_db, "get_skills",
        lambda **kw: (_ for _ in ()).throw(RuntimeError("index broken")),
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t2",
    )
    assert summary["updated"] == 1 and summary["errors"] == []
    assert summary["skills_marked_stale"] == 0


# ── staleness lifecycle: regeneration clears, unchanged re-upsert keeps ─────

def test_mtime_change_clears_staleness_via_refresh(conn, tmp_path):
    """The regeneration loop: edit SKILL.md → refresh_if_stale → flag off."""
    skill_path = _seed_skill(conn, tmp_path, tags=["testflight"])
    reflect_db.mark_skills_stale([skill_path], conn=conn)
    assert _skill_row(conn, skill_path)["is_stale"] == 1

    # Regenerate: content + mtime change, then the stat()-only refresh pass.
    p = Path(skill_path)
    p.write_text(p.read_text() + "\n\n## Refreshed guidance\n", encoding="utf-8")
    os.utime(p, (time.time() + 5, time.time() + 5))
    skill_index.refresh_if_stale(tmp_path / "skills", conn=conn)

    assert _skill_row(conn, skill_path)["is_stale"] == 0


def test_unchanged_reupsert_preserves_staleness(conn, tmp_path):
    """A full rebuild_index over an UNCHANGED file must not launder the flag."""
    skill_path = _seed_skill(conn, tmp_path, tags=["testflight"])
    reflect_db.mark_skills_stale([skill_path], conn=conn)
    skill_index.rebuild_index(tmp_path / "skills", conn=conn)
    assert _skill_row(conn, skill_path)["is_stale"] == 1


def test_clear_skill_stale_explicit(conn, tmp_path):
    skill_path = _seed_skill(conn, tmp_path)
    reflect_db.mark_skills_stale([skill_path], conn=conn)
    assert reflect_db.clear_skill_stale(skill_path, conn=conn) is True
    assert _skill_row(conn, skill_path)["is_stale"] == 0
    assert reflect_db.clear_skill_stale(skill_path, conn=conn) is False  # idempotent


def test_get_stale_skills_lists_only_flagged(conn, tmp_path):
    stale = _seed_skill(conn, tmp_path, name="stale-one", tags=["a"])
    _seed_skill(conn, tmp_path, name="fresh-one", tags=["b"])
    reflect_db.mark_skills_stale([stale], conn=conn)
    rows = reflect_db.get_stale_skills(conn=conn)
    assert [r["path"] for r in rows] == [stale]
    assert rows[0]["tags"] == ["a"]


def test_pre_r13_db_migrates_is_stale_column(tmp_path):
    """An existing skills table without is_stale gains the column in place."""
    db_file = tmp_path / "old.db"
    raw = sqlite3.connect(str(db_file))
    raw.execute(
        """CREATE TABLE skills (
               path TEXT PRIMARY KEY, name TEXT NOT NULL,
               tags TEXT NOT NULL DEFAULT '[]', summary TEXT NOT NULL DEFAULT '',
               mtime REAL NOT NULL DEFAULT 0, last_refreshed_at TEXT NOT NULL)"""
    )
    raw.execute(
        "INSERT INTO skills VALUES ('/s/SKILL.md', 'old', '[]', '', 1.0, 'ts')"
    )
    raw.commit()
    raw.close()

    connection = reflect_db.init_db(db_file)
    try:
        row = connection.execute(
            "SELECT is_stale FROM skills WHERE path = '/s/SKILL.md'"
        ).fetchone()
        assert row["is_stale"] == 0
    finally:
        reflect_db.close_all()


# ── acceptance 2: drain processes the refresh task ───────────────────────────

def _drain_env(state_dir: Path, **overrides) -> dict[str, str]:
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state_dir),
        "REFLECT_DRAIN_DRY_RUN": "1",
        "REFLECT_DRAIN_SKIP_REINDEX": "1",
        "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
        # Cascade stays ON: skill_refresh entries must bypass it themselves —
        # the gate would otherwise drop a SKILL.md as no-signal.
        "REFLECT_DRAIN_CASCADE": "1",
    })
    env.update({k: str(v) for k, v in overrides.items()})
    return env


def test_drain_processes_skill_refresh_task(tmp_path):
    state = tmp_path / "state"
    state.mkdir(parents=True)
    skill_md = _write_skill_md(
        tmp_path / "skills", "publish", name="publish", tags=["testflight"],
    )
    entry = {
        "ts": "t", "session_id": "skill-refresh",
        "transcript_path": str(skill_md), "trigger": "skill_refresh",
        "skill_name": "publish", "learning_id": "lrn-1",
        "reason": "learning revised (update)",
    }
    queue = state / "pending_reflections.jsonl"
    queue.write_text(json.dumps(entry) + "\n")

    result = subprocess.run(
        ["bash", str(DRAIN)], env=_drain_env(state),
        capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    log = (state / "drain.log").read_text()
    # The entry was processed via the skill-refresh branch, not gated away
    # by the cascade ("cascade skip") or dropped as malformed.
    assert "skill-refresh publish" in log
    assert "cascade skip" not in log
    # Processed entries leave the queue.
    assert queue.read_text().strip() == ""


def test_drain_skill_refresh_missing_skill_md_is_permanent_skip(tmp_path):
    """A refresh task whose SKILL.md was uninstalled is dropped, not retried."""
    state = tmp_path / "state"
    state.mkdir(parents=True)
    entry = {
        "ts": "t", "session_id": "skill-refresh",
        "transcript_path": str(tmp_path / "gone" / "SKILL.md"),
        "trigger": "skill_refresh", "skill_name": "gone",
        "learning_id": "lrn-1", "reason": "r",
    }
    queue = state / "pending_reflections.jsonl"
    queue.write_text(json.dumps(entry) + "\n")

    subprocess.run(
        ["bash", str(DRAIN)], env=_drain_env(state),
        capture_output=True, text=True, timeout=60,
    )
    assert "skip-stale" in (state / "drain.log").read_text()
    assert queue.read_text().strip() == ""


def test_drain_script_carries_skill_refresh_prompt():
    """Pin the live-path prompt branch (not reachable in DRY_RUN tests)."""
    script = DRAIN.read_text()
    assert "skill_refresh" in script
    assert "skill-edit step" in script
    assert "clear_skill_stale" in script
    # The cascade bypass for refresh tasks.
    assert '"$trigger" != "skill_refresh"' in script


# ── acceptance 3: stale skills NOT injected (R11) ────────────────────────────

def test_match_skills_excludes_stale(conn, tmp_path):
    skill_path = _seed_skill(conn, tmp_path, name="publish", tags=["testflight"])
    assert [r["path"] for r in skill_index.match_skills("testflight", conn=conn)] \
        == [skill_path]
    reflect_db.mark_skills_stale([skill_path], conn=conn)
    assert skill_index.match_skills("testflight", conn=conn) == []


FAKE_LEARNING = "- prior learning about playwright retries [lrn-fake-1]"


def test_stale_skill_not_injected_at_session_start(tmp_path):
    """End-to-end R11 pin: with the tiered inject ON, a stale skill loses the
    tier and the hook falls through to the raw-learnings inject."""
    (tmp_path / "home").mkdir()
    project = tmp_path / "playwright"        # project name = the query
    project.mkdir()
    skills_dir = tmp_path / "skills"
    _write_skill_md(
        skills_dir, "webapp-testing", name="webapp-testing",
        description="Drive Playwright browser tests.", tags=["playwright"],
    )
    uv_dir = tmp_path / "uvbin"
    uv_dir.mkdir()
    uv = uv_dir / "uv"
    uv.write_text(f"#!/bin/sh\necho '{FAKE_LEARNING}'\n", encoding="utf-8")
    uv.chmod(0o755)

    # Seed the DB: index the skill, then flag it stale.
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    skill_index.rebuild_index(skills_dir, conn=connection)
    paths = [r["path"] for r in reflect_db.get_skills(conn=connection)]
    assert reflect_db.mark_skills_stale(paths, conn=connection) == 1
    reflect_db.close_all()

    env = {
        "PATH": str(uv_dir),
        "HOME": str(tmp_path / "home"),
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "REFLECT_DB_PATH": str(db_file),
        "REFLECT_SKILLS_DIR": str(skills_dir),
        "CLAUDE_PROJECT_DIR": str(project),
        "REFLECT_TIERED_INJECT": "1",
    }
    result = subprocess.run(
        [sys.executable, str(HOOK)], input="{}", capture_output=True,
        text=True, env=env, timeout=30,
    )
    assert result.returncode == 0, result.stderr
    ctx = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
    assert "webapp-testing" not in ctx, f"stale skill leaked into inject: {ctx!r}"
    assert "lrn-fake-1" in ctx, f"learnings fallback expected: {ctx!r}"


# ── plumbing pin: the skill doc documents the loop ───────────────────────────

def test_skill_documents_auto_refresh():
    doc = SKILL_DOC.read_text()
    assert "Auto Skill Refresh" in doc
    assert "is_stale" in doc
    assert "skill_refresh" in doc
    assert "skills_marked_stale" in doc


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
