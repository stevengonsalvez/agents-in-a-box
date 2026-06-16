# ABOUTME: Regression tests for port R20 — skills index in reflect.db.
# ABOUTME: Pins the skills table population from SKILL.md files (name/path/
# ABOUTME: tags/summary/last_refreshed_at) and the stat()-only cheap refresh
# ABOUTME: that re-parses ONLY new/mtime-changed skills and prunes deletions.
"""Port R20: skills index (hindsight mental_models shape).

Acceptance criteria pinned here:
  1. table populated for current skills
  2. cheap rebuild on mtime change
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

import reflect_db  # noqa: E402
import skill_index  # noqa: E402


@pytest.fixture
def conn(tmp_path):
    """Fresh isolated DB per test; never touches ~/.reflect."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    yield connection
    reflect_db.close_all()


def _write_skill(
    base: Path,
    dirname: str,
    *,
    name: str | None = None,
    description: str = "Does a useful thing.\nSecond line is dropped.",
    triggers: list[str] | None = None,
    tags: list[str] | None = None,
) -> Path:
    """Write a realistic SKILL.md (block-scalar description, list triggers)."""
    skill_dir = base / dirname
    skill_dir.mkdir(parents=True)
    lines = ["---"]
    if name is not None:
        lines.append(f"name: {name}")
    lines.append("description: |")
    lines.extend(f"  {ln}" for ln in description.splitlines())
    lines.append('version: "1.0.0"')
    if tags:
        lines.append("tags:")
        lines.extend(f"  - {t}" for t in tags)
    if triggers:
        lines.append("triggers:")
        lines.extend(f"  - {t}" for t in triggers)
    lines.extend(["allowed-tools:", "  - Bash", "---", "", f"# {dirname}", "body"])
    path = skill_dir / "SKILL.md"
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


@pytest.fixture
def skills_dir(tmp_path):
    base = tmp_path / "skills"
    _write_skill(
        base,
        "recall",
        name="reflect:recall",
        description="Retrieve relevant prior learnings from the knowledge base.",
        triggers=["reflect:recall", "prior learnings"],
    )
    _write_skill(
        base,
        "tmux-monitor",
        description="Watch tmux sessions for agent activity.",
        tags=["tmux", "monitoring"],
    )
    return base


# ---------- acceptance 1: table populated for current skills ----------

def test_rebuild_populates_table_for_current_skills(conn, skills_dir):
    summary = skill_index.rebuild_index(skills_dir, conn=conn)
    assert summary["indexed"] == 2

    rows = reflect_db.get_skills(conn=conn)
    assert len(rows) == 2
    by_name = {r["name"]: r for r in rows}

    recall = by_name["reflect:recall"]
    assert recall["path"] == str(skills_dir / "recall" / "SKILL.md")
    assert recall["tags"] == ["reflect:recall", "prior learnings"]
    assert recall["summary"].startswith("Retrieve relevant prior learnings")
    assert recall["last_refreshed_at"]  # ISO timestamp present
    assert recall["mtime"] > 0

    monitor = by_name["tmux-monitor"]  # name falls back to dirname
    assert monitor["tags"] == ["tmux", "monitoring"]


def test_rebuild_indexes_namespaced_two_level_skills(conn, tmp_path):
    base = tmp_path / "skills"
    _write_skill(base / "myplugin", "nested", name="myplugin:nested")
    summary = skill_index.rebuild_index(base, conn=conn)
    assert summary["indexed"] == 1
    assert reflect_db.get_skill_by_name("myplugin:nested", conn=conn) is not None


def test_rebuild_prunes_uninstalled_skills(conn, skills_dir):
    skill_index.rebuild_index(skills_dir, conn=conn)
    (skills_dir / "tmux-monitor" / "SKILL.md").unlink()
    summary = skill_index.rebuild_index(skills_dir, conn=conn)
    assert summary["removed"] == 1
    names = {r["name"] for r in reflect_db.get_skills(conn=conn)}
    assert names == {"reflect:recall"}


def test_rebuild_is_idempotent(conn, skills_dir):
    skill_index.rebuild_index(skills_dir, conn=conn)
    skill_index.rebuild_index(skills_dir, conn=conn)
    assert len(reflect_db.get_skills(conn=conn)) == 2


def test_malformed_skill_md_still_indexed_by_dirname(conn, tmp_path):
    base = tmp_path / "skills"
    bad = base / "broken"
    bad.mkdir(parents=True)
    (bad / "SKILL.md").write_text("no frontmatter at all\n", encoding="utf-8")
    skill_index.rebuild_index(base, conn=conn)
    row = reflect_db.get_skill_by_name("broken", conn=conn)
    assert row is not None
    assert row["tags"] == []
    assert row["summary"] == ""


# ---------- acceptance 2: cheap rebuild on mtime change ----------

def test_refresh_noop_when_nothing_changed(conn, skills_dir, monkeypatch):
    skill_index.rebuild_index(skills_dir, conn=conn)

    def _boom(path):  # parsing an unchanged skill = not cheap
        raise AssertionError(f"re-parsed unchanged skill: {path}")

    monkeypatch.setattr(skill_index, "parse_skill_md", _boom)
    summary = skill_index.refresh_if_stale(skills_dir, conn=conn)
    assert summary == {"added": 0, "changed": 0, "removed": 0, "unchanged": 2}


def test_refresh_reparses_only_the_mtime_changed_skill(conn, skills_dir, monkeypatch):
    skill_index.rebuild_index(skills_dir, conn=conn)

    target = skills_dir / "recall" / "SKILL.md"
    target.write_text(
        textwrap.dedent(
            """\
            ---
            name: reflect:recall
            description: |
              Updated summary line.
            triggers:
              - new-trigger
            ---
            """
        ),
        encoding="utf-8",
    )
    os.utime(target, (target.stat().st_atime, target.stat().st_mtime + 10))

    parsed: list[str] = []
    real_parse = skill_index.parse_skill_md
    monkeypatch.setattr(
        skill_index,
        "parse_skill_md",
        lambda p: (parsed.append(str(p)), real_parse(p))[1],
    )

    summary = skill_index.refresh_if_stale(skills_dir, conn=conn)
    assert summary["changed"] == 1
    assert summary["unchanged"] == 1
    assert parsed == [str(target)], "only the changed SKILL.md may be re-read"

    row = reflect_db.get_skill_by_name("reflect:recall", conn=conn)
    assert row["summary"] == "Updated summary line."
    assert row["tags"] == ["new-trigger"]


def test_refresh_picks_up_new_skill(conn, skills_dir):
    skill_index.rebuild_index(skills_dir, conn=conn)
    _write_skill(skills_dir, "newborn", name="newborn", triggers=["fresh"])
    summary = skill_index.refresh_if_stale(skills_dir, conn=conn)
    assert summary["added"] == 1
    assert reflect_db.get_skill_by_name("newborn", conn=conn) is not None


def test_refresh_prunes_deleted_skill(conn, skills_dir):
    skill_index.rebuild_index(skills_dir, conn=conn)
    (skills_dir / "tmux-monitor" / "SKILL.md").unlink()
    summary = skill_index.refresh_if_stale(skills_dir, conn=conn)
    assert summary["removed"] == 1
    assert reflect_db.get_skill_by_name("tmux-monitor", conn=conn) is None


def test_scan_is_stat_only(skills_dir, monkeypatch):
    """The staleness half never opens a file — stat() only."""
    def _boom(*a, **k):
        raise AssertionError("scan must not read file contents")

    monkeypatch.setattr(Path, "read_text", _boom)
    files = skill_index.scan_skill_files(skills_dir)
    assert len(files) == 2
    assert all(mtime > 0 for mtime in files.values())


# ---------- supporting surface: query matching + upsert semantics ----------

def test_match_skills_ranks_trigger_hits_first(conn, skills_dir):
    skill_index.rebuild_index(skills_dir, conn=conn)
    hits = skill_index.match_skills("watch tmux sessions", conn=conn)
    assert hits and hits[0]["name"] == "tmux-monitor"


def test_match_skills_empty_for_unrelated_query(conn, skills_dir):
    skill_index.rebuild_index(skills_dir, conn=conn)
    assert skill_index.match_skills("quantum chromodynamics lattice", conn=conn) == []


def test_upsert_skill_updates_in_place(conn):
    reflect_db.upsert_skill("s", "/tmp/s/SKILL.md", tags=["a"], summary="v1", mtime=1.0, conn=conn)
    reflect_db.upsert_skill("s", "/tmp/s/SKILL.md", tags=["b"], summary="v2", mtime=2.0, conn=conn)
    rows = reflect_db.get_skills(conn=conn)
    assert len(rows) == 1
    assert rows[0]["summary"] == "v2"
    assert rows[0]["tags"] == ["b"]
    assert rows[0]["mtime"] == 2.0


def test_skills_table_created_on_existing_db_migration(tmp_path):
    """Pre-R20 DBs gain the skills table via _migrate_schema."""
    import sqlite3

    db_file = tmp_path / "old.db"
    raw = sqlite3.connect(str(db_file))
    # A realistic pre-R20 schema: everything except the skills table.
    pre_r20_ddl = reflect_db._SCHEMA_DDL.replace(reflect_db._SKILLS_DDL, "")
    assert "CREATE TABLE IF NOT EXISTS skills" not in pre_r20_ddl
    raw.executescript(pre_r20_ddl)
    raw.close()

    conn = reflect_db.init_db(db_file)
    try:
        tables = {
            r[0]
            for r in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            ).fetchall()
        }
        assert "skills" in tables
    finally:
        reflect_db.close_all()


def test_cli_rebuild_and_list(tmp_path, skills_dir):
    """End-to-end CLI smoke: rebuild against an isolated DB, then list."""
    env = {
        **os.environ,
        "REFLECT_DB_PATH": str(tmp_path / "cli.db"),
        "REFLECT_SKILLS_DIR": str(skills_dir),
    }
    script = str(SCRIPTS / "skill_index.py")
    r = subprocess.run(
        [sys.executable, script, "rebuild"],
        capture_output=True, text=True, timeout=60, env=env, cwd=str(SCRIPTS),
    )
    assert r.returncode == 0, r.stderr
    assert json.loads(r.stdout)["indexed"] == 2

    r = subprocess.run(
        [sys.executable, script, "list"],
        capture_output=True, text=True, timeout=60, env=env, cwd=str(SCRIPTS),
    )
    assert r.returncode == 0, r.stderr
    assert "reflect:recall" in r.stdout
    assert "tmux-monitor" in r.stdout


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
