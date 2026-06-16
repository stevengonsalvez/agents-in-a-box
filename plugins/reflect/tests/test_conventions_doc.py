# ABOUTME: Regression tests for port O2 — auto-refreshing conventions doc per
# ABOUTME: project (hindsight mental_models refresh shape). Pins the
# ABOUTME: conventions_docs table, the deterministic CONVENTIONS.md generator,
# ABOUTME: the cascade trigger that regenerates the doc when in-scope
# ABOUTME: observations change, the R14-shaped staleness check, and the
# ABOUTME: SessionStart 1-line summary + path inject (stale docs never inject).
"""Port O2: auto-refreshing conventions doc per project.

Acceptance criteria pinned here:
  1. CONVENTIONS.md regenerates when an in-scope observation updates
  2. SessionStart injects a 1-line summary + path, not the whole doc
  3. stale conventions doc DOES NOT inject (uses the R14 staleness check)

In-process tests cover the table, the generator, the cascade trigger, and
the staleness semantics. The SessionStart hook is exercised as a subprocess
(the way the harness runs it) with a fully synthetic environment — tmp HOME,
tmp reflect.db, tmp state dir, fake ``uv`` standing in for the learnings
tier — so no test ever touches the real ~/.reflect or knowledge base.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
sys.path.insert(0, str(SCRIPTS))

import conventions_generator  # noqa: E402
import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402

FAKE_LEARNING = "- prior learning about playwright flake retries [lrn-fake-1]"
CONVENTION_TEXT = "Team prefers uv over pip for all Python dependency management"


# --- In-process fixtures ------------------------------------------------------


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB + state dir per test, wired as the module defaults."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    yield connection
    reflect_db.close_all()


def _doc_file(project_id: str) -> Path:
    return conventions_generator.doc_path_for(project_id)


# --- Schema -------------------------------------------------------------------


def test_fresh_db_has_conventions_docs_table(conn):
    tables = {
        r[0]
        for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        ).fetchall()
    }
    assert "conventions_docs" in tables


def test_reinit_adds_conventions_docs_to_pre_o2_db(tmp_path):
    """A pre-O2 DB (no conventions_docs table) gains it on the next init."""
    db_file = tmp_path / "old.db"
    connection = reflect_db.init_db(db_file)
    with connection:
        connection.execute("DROP TABLE conventions_docs")
    reflect_db.close_all()
    connection = reflect_db.init_db(db_file)
    try:
        reflect_db.upsert_conventions_doc("demo", content="x", conn=connection)
        assert reflect_db.get_conventions_doc("demo", conn=connection) is not None
    finally:
        reflect_db.close_all()


# --- Generator: doc file + row ------------------------------------------------


def test_generate_writes_doc_file_and_registers_row(conn):
    reflect_db.add_observation(
        CONVENTION_TEXT, category="Tooling", scope="project", conn=conn,
    )
    result = conventions_generator.generate_conventions_doc("demo", conn=conn)

    path = Path(result["doc_path"])
    assert path == _doc_file("demo")
    assert path.is_file()
    body = path.read_text(encoding="utf-8")
    assert "# Conventions — demo" in body
    assert CONVENTION_TEXT in body
    assert "## Tooling" in body
    assert "_(evidence ×1)_" in body

    row = reflect_db.get_conventions_doc("demo", conn=conn)
    assert row is not None
    assert row["observation_count"] == 1
    assert row["is_stale"] == 0
    assert row["doc_path"] == str(path)
    assert row["content"] == body
    # default scope tags: the project's own bucket + the generic drain bucket
    assert row["scope_tags"] == ["demo", "project"]


def test_generate_includes_global_scope_observations(conn):
    reflect_db.add_observation(
        "Always write conventional commits", scope="global", conn=conn,
    )
    result = conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert result["observation_count"] == 1
    assert "conventional commits" in _doc_file("demo").read_text(encoding="utf-8")


def test_doc_path_sanitizes_hostile_project_ids(conn):
    path = conventions_generator.doc_path_for("../../evil project")
    assert ".." not in path.parts
    assert path.name == "CONVENTIONS.md"
    assert path.is_relative_to(conventions_generator.conventions_dir())


# --- Acceptance 1: doc regenerates when an in-scope observation updates -------


def test_cascade_create_regenerates_registered_doc(conn):
    """A new observation landing via the cascade regenerates the doc."""
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    before = reflect_db.get_conventions_doc("demo", conn=conn)
    assert CONVENTION_TEXT not in _doc_file("demo").read_text(encoding="utf-8")

    time.sleep(0.01)
    summary = reflect_cascade.execute_observation_actions(
        [{"action": "CREATE", "content": CONVENTION_TEXT, "category": "Tooling"}]
    )
    assert summary["created"] == 1
    assert summary["conventions_refreshed"] >= 1

    body = _doc_file("demo").read_text(encoding="utf-8")
    assert CONVENTION_TEXT in body
    after = reflect_db.get_conventions_doc("demo", conn=conn)
    assert after["last_refreshed_at"] > before["last_refreshed_at"]


def test_cascade_update_regenerates_doc_with_new_evidence_count(conn):
    oid = reflect_db.add_observation(
        CONVENTION_TEXT, category="Tooling", scope="project", conn=conn,
    )
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert "_(evidence ×1)_" in _doc_file("demo").read_text(encoding="utf-8")

    summary = reflect_cascade.execute_observation_actions(
        [{
            "action": "UPDATE",
            "target_id": oid,
            "source_correction_ids": ["corr-1"],
            "reason": "seen again",
        }]
    )
    assert summary["updated"] == 1
    assert summary["conventions_refreshed"] >= 1
    assert "_(evidence ×2)_" in _doc_file("demo").read_text(encoding="utf-8")


def test_cascade_delete_drops_retired_convention_from_doc(conn):
    keep = "Keep using ruff for linting"
    drop = "Use the legacy build script"
    reflect_db.add_observation(keep, scope="project", conn=conn)
    oid = reflect_db.add_observation(drop, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert drop in _doc_file("demo").read_text(encoding="utf-8")

    summary = reflect_cascade.execute_observation_actions(
        [{"action": "DELETE", "target_id": oid, "reason": "no longer holds"}]
    )
    assert summary["deleted"] == 1
    body = _doc_file("demo").read_text(encoding="utf-8")
    assert drop not in body
    assert keep in body


def test_cascade_bootstraps_doc_for_unregistered_scope(conn):
    """First observe pass for a scope brings its conventions doc into being."""
    assert reflect_db.get_conventions_docs(conn=conn) == []
    summary = reflect_cascade.execute_observation_actions(
        [{"action": "CREATE", "content": CONVENTION_TEXT}]
    )
    assert summary["conventions_refreshed"] == 1
    row = reflect_db.get_conventions_doc("project", conn=conn)
    assert row is not None
    assert Path(row["doc_path"]).is_file()


def test_direct_observation_edit_flips_staleness_and_refresh_catches_up(conn):
    """Writes that bypass the cascade trigger are caught by the computed
    (R14-shaped) check, and refresh_if_stale regenerates."""
    oid = reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is False

    time.sleep(0.01)
    reflect_db.add_observation_evidence(oid, ["corr-9"], conn=conn)
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is True

    assert conventions_generator.refresh_if_stale("demo", conn=conn) is True
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is False
    assert "_(evidence ×2)_" in _doc_file("demo").read_text(encoding="utf-8")


# --- Staleness semantics (R14 shape) -------------------------------------------


def test_compute_staleness_is_none_for_unregistered_project(conn):
    assert reflect_db.compute_conventions_is_stale("ghost", conn=conn) is None


def test_global_observation_stales_a_project_doc(conn):
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    time.sleep(0.01)
    reflect_db.add_observation("Never force-push to main", scope="global", conn=conn)
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is True


def test_out_of_scope_observation_does_not_stale_doc(conn):
    reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    time.sleep(0.01)
    reflect_db.add_observation("Other project's habit", scope="otherproj", conn=conn)
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is False


def test_marked_stale_doc_reports_stale_and_upsert_clears_it(conn):
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert reflect_db.mark_conventions_docs_stale(["demo"], conn=conn) == 1
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is True
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert reflect_db.compute_conventions_is_stale("demo", conn=conn) is False


# --- session_inject_line --------------------------------------------------------


def test_inject_line_carries_summary_and_path_not_doc_body(conn):
    reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    line = conventions_generator.session_inject_line("demo", conn=conn)
    assert str(_doc_file("demo")) in line
    assert "1 convention(s)" in line
    assert CONVENTION_TEXT not in line  # never the doc body
    assert len(line.splitlines()) <= 2  # header + 1 summary line


def test_inject_line_empty_for_doc_with_zero_observations(conn):
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    assert conventions_generator.session_inject_line("demo", conn=conn) == ""


def test_inject_line_falls_back_to_generic_project_doc(conn):
    reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("project", conn=conn)
    line = conventions_generator.session_inject_line("someproj", conn=conn)
    assert str(_doc_file("project")) in line


def test_inject_line_empty_when_doc_file_deleted(conn):
    reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    _doc_file("demo").unlink()
    assert conventions_generator.session_inject_line("demo", conn=conn) == ""


def test_inject_line_empty_for_stale_doc(conn):
    reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)
    reflect_db.mark_conventions_docs_stale(["demo"], conn=conn)
    assert conventions_generator.session_inject_line("demo", conn=conn) == ""


# --- Symlink (opt-in materialization) -------------------------------------------


def test_symlink_into_project_links_and_never_clobbers(tmp_path, conn):
    reflect_db.add_observation(CONVENTION_TEXT, scope="project", conn=conn)
    conventions_generator.generate_conventions_doc("demo", conn=conn)

    root = tmp_path / "repo"
    root.mkdir()
    assert conventions_generator.symlink_into_project("demo", root, conn=conn)
    link = root / "CONVENTIONS.md"
    assert link.is_symlink()
    assert CONVENTION_TEXT in link.read_text(encoding="utf-8")
    # idempotent re-link
    assert conventions_generator.symlink_into_project("demo", root, conn=conn)

    # a real file is never clobbered
    other = tmp_path / "repo2"
    other.mkdir()
    (other / "CONVENTIONS.md").write_text("hand-written", encoding="utf-8")
    assert not conventions_generator.symlink_into_project("demo", other, conn=conn)
    assert (other / "CONVENTIONS.md").read_text(encoding="utf-8") == "hand-written"


# --- SessionStart hook (subprocess, acceptance 2 + 3) ----------------------------


@pytest.fixture
def sandbox(tmp_path):
    """Isolated world: project dir, state dir, empty PATH dir (no git/uv)."""
    (tmp_path / "home").mkdir()
    (tmp_path / "playwright").mkdir()       # CLAUDE_PROJECT_DIR; name = project id
    (tmp_path / "skills").mkdir()
    (tmp_path / "emptybin").mkdir()
    return tmp_path


def _env(sandbox: Path, *, flag: str | None = "1", uv_bin: Path | None = None,
         extra: dict[str, str] | None = None) -> dict[str, str]:
    """Minimal hook environment — no real uv/git/~/.reflect can leak in."""
    path = str(uv_bin) if uv_bin else str(sandbox / "emptybin")
    env = {
        "PATH": path,
        "HOME": str(sandbox / "home"),
        "REFLECT_STATE_DIR": str(sandbox / "state"),
        "REFLECT_DB_PATH": str(sandbox / "reflect.db"),
        "REFLECT_SKILLS_DIR": str(sandbox / "skills"),
        "CLAUDE_PROJECT_DIR": str(sandbox / "playwright"),
    }
    if flag is not None:
        env["REFLECT_TIERED_INJECT"] = flag
    if extra:
        env.update(extra)
    return env


def _fake_uv(bin_dir: Path, output: str = FAKE_LEARNING) -> Path:
    """Stand-in ``uv`` printing canned learnings markdown (the lower tier)."""
    bin_dir.mkdir(parents=True, exist_ok=True)
    uv = bin_dir / "uv"
    uv.write_text(f"#!/bin/sh\necho '{output}'\n", encoding="utf-8")
    uv.chmod(0o755)
    return uv


def _seed(sandbox: Path, code: str) -> str:
    """Run *code* in a subprocess against the sandbox DB/state (keeps the
    test process's module caches out of the hook's world)."""
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(sandbox / "home"),
        "REFLECT_STATE_DIR": str(sandbox / "state"),
        "REFLECT_DB_PATH": str(sandbox / "reflect.db"),
    }
    preamble = f"import sys\nsys.path.insert(0, {str(SCRIPTS)!r})\n"
    result = subprocess.run(
        [sys.executable, "-c", preamble + code],
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    assert result.returncode == 0, f"seed failed: {result.stderr!r}"
    return result.stdout.strip()


def _seed_fresh_doc(sandbox: Path) -> str:
    """Observation + generated doc for project 'playwright'. Returns the oid."""
    return _seed(
        sandbox,
        "import reflect_db, conventions_generator\n"
        f"oid = reflect_db.add_observation({CONVENTION_TEXT!r},"
        " category='Tooling', scope='project')\n"
        "conventions_generator.generate_conventions_doc('playwright')\n"
        "print(oid)\n",
    )


def _run_hook(env: dict[str, str]) -> str:
    """Run the hook, assert the silent-fail contract, return the context."""
    result = subprocess.run(
        [sys.executable, str(HOOK)],
        input="{}",
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    assert result.returncode == 0, (
        f"hook exited non-zero:\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    parsed = json.loads(result.stdout)
    out = parsed["hookSpecificOutput"]
    assert out["hookEventName"] == "SessionStart"
    return out["additionalContext"]


def test_session_start_injects_one_line_summary_and_path(sandbox):
    """Acceptance 2: SessionStart injects a 1-line summary + path — not the
    doc body — and the pointer never suppresses the learnings tier."""
    _seed_fresh_doc(sandbox)
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, uv_bin=uv_dir))

    doc_path = sandbox / "state" / "conventions" / "playwright" / "CONVENTIONS.md"
    assert "Project conventions" in ctx, f"conventions pointer missing: {ctx!r}"
    assert str(doc_path) in ctx, f"doc path missing from inject: {ctx!r}"
    assert "1 convention(s)" in ctx
    assert CONVENTION_TEXT not in ctx, f"doc body leaked into inject: {ctx!r}"
    assert "lrn-fake-1" in ctx, f"ambient pointer suppressed lower tier: {ctx!r}"


def test_stale_conventions_doc_does_not_inject(sandbox):
    """Acceptance 3: an in-scope observation changed after the doc was last
    refreshed → the R14-shaped check flags it stale → no pointer injects."""
    oid = _seed_fresh_doc(sandbox)
    _seed(
        sandbox,
        "import reflect_db\n"
        f"reflect_db.add_observation_evidence({oid!r}, ['corr-1'])\n",
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, uv_bin=uv_dir))

    assert "Project conventions" not in ctx, f"stale doc injected: {ctx!r}"
    assert "CONVENTIONS.md" not in ctx
    assert "lrn-fake-1" in ctx  # lower tiers unaffected


def test_conventions_tier_off_without_tiered_inject_flag(sandbox):
    """The conventions pointer rides the R10 opt-in flag (off by default)."""
    _seed_fresh_doc(sandbox)
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, flag=None, uv_bin=uv_dir))

    assert "Project conventions" not in ctx
    assert "lrn-fake-1" in ctx


def test_hook_symlinks_doc_into_project_root_when_opted_in(sandbox):
    """REFLECT_CONVENTIONS_SYMLINK=1 materializes <project>/CONVENTIONS.md."""
    _seed_fresh_doc(sandbox)
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)
    link = sandbox / "playwright" / "CONVENTIONS.md"

    # default: no symlink is written into the user's repo
    _run_hook(_env(sandbox, uv_bin=uv_dir))
    assert not link.exists()

    ctx = _run_hook(
        _env(sandbox, uv_bin=uv_dir, extra={"REFLECT_CONVENTIONS_SYMLINK": "1"})
    )
    assert "Project conventions" in ctx
    assert link.is_symlink()
    assert CONVENTION_TEXT in link.read_text(encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
