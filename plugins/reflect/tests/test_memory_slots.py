# ABOUTME: Regression tests for port A1 — pinned editable memory slots
# ABOUTME: (agent-curated scratchpads). Pins: 8 default slots seeded on init,
# ABOUTME: agent append/replace/get/delete via the skill's CLI surface, hard
# ABOUTME: size caps, the Stop hook's deterministic TODO auto-append, and the
# ABOUTME: Tier-0 SessionStart inject (slots before skills and recall results).
"""Port A1: pinned editable memory slots (agentmemory slots shape).

Acceptance criteria pinned here:
  1. 8 default slots created on init
  2. agent can append/replace via skill calls (reflect_db CLI surface)
  3. stop hook auto-appends TODOs to pending_items without LLM
  4. SessionStart includes slots before any recall results
  5. size cap enforced

Plus the design invariants:
  - project slots shadow same-named global slots on read
  - read-only slots reject agent edits and are skipped by auto-writers
  - slot-delete empties the slot but keeps the named row (fixed vocabulary)
  - everything is behind the REFLECT_SLOTS opt-in flag (off by default)
  - hook paths stay silent-fail (exit 0, no stdout pollution)
"""

from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
SESSION_HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
STOP_HOOK = PLUGIN_ROOT / "hooks" / "stop_reflect.py"
SLOTS_SKILL = PLUGIN_ROOT / "skills" / "slots" / "SKILL.md"
sys.path.insert(0, str(SCRIPTS))

import reflect_db  # noqa: E402

FAKE_LEARNING = "- prior learning about playwright flake retries [lrn-fake-1]"
PROJECT = "playwright"


@pytest.fixture
def conn(tmp_path):
    """Fresh isolated DB per test."""
    connection = reflect_db.init_db(tmp_path / "reflect.db")
    yield connection
    reflect_db.close_all()


# --- Acceptance 1: 8 default slots created on init ---------------------------


def test_eight_default_slots_seeded_on_init(conn):
    created = reflect_db.ensure_default_slots(PROJECT, conn=conn)
    assert created == 8
    rows = conn.execute("SELECT * FROM slots").fetchall()
    assert len(rows) == 8
    names = {r["name"] for r in rows}
    assert names == {
        "persona", "user_preferences", "tool_guidelines", "project_context",
        "guidance", "pending_items", "session_patterns", "self_notes",
    }
    # Global slots live in the '' bucket; project slots under the project id.
    by_name = {r["name"]: r for r in rows}
    for global_name in ("persona", "user_preferences", "tool_guidelines"):
        assert by_name[global_name]["project_id"] == ""
        assert by_name[global_name]["scope"] == "global"
    for project_name in ("project_context", "guidance", "pending_items",
                         "session_patterns", "self_notes"):
        assert by_name[project_name]["project_id"] == PROJECT
        assert by_name[project_name]["scope"] == "project"


def test_seeding_is_idempotent_and_preserves_edits(conn):
    reflect_db.ensure_default_slots(PROJECT, conn=conn)
    assert reflect_db.slot_append(
        "guidance", "keep focus", project_id=PROJECT, conn=conn,
    )["ok"]
    assert reflect_db.ensure_default_slots(PROJECT, conn=conn) == 0
    slot = reflect_db.get_slot("guidance", project_id=PROJECT, conn=conn)
    assert slot["content"] == "keep focus"


def test_project_slot_shadows_global_on_read(conn):
    reflect_db.ensure_default_slots(PROJECT, conn=conn)
    # A second project gets its own project-scope rows; global rows shared.
    assert reflect_db.ensure_default_slots("other", conn=conn) == 5
    reflect_db.slot_replace("guidance", "for playwright", project_id=PROJECT, conn=conn)
    reflect_db.slot_replace("guidance", "for other", project_id="other", conn=conn)
    assert reflect_db.get_slot("guidance", project_id=PROJECT, conn=conn)["content"] == "for playwright"
    assert reflect_db.get_slot("guidance", project_id="other", conn=conn)["content"] == "for other"
    # Global slot readable from any project.
    reflect_db.slot_replace("persona", "terse colleague", project_id=PROJECT, conn=conn)
    assert reflect_db.get_slot("persona", project_id="other", conn=conn)["content"] == "terse colleague"


# --- Acceptance 2: agent can append/replace via skill calls ------------------


def _cli(tmp_path: Path, *args: str) -> subprocess.CompletedProcess:
    """Invoke the reflect_db CLI exactly as the slots skill documents."""
    return subprocess.run(
        [sys.executable, str(SCRIPTS / "reflect_db.py"), *args,
         "--project", PROJECT],
        capture_output=True,
        text=True,
        env={"REFLECT_DB_PATH": str(tmp_path / "reflect.db"), "PATH": ""},
        timeout=30,
    )


def test_agent_append_and_replace_via_cli(tmp_path):
    r = _cli(tmp_path, "slot-append", "--name", "pending_items",
             "--text", "- wire the retry budget")
    assert r.returncode == 0, r.stderr
    r = _cli(tmp_path, "slot-append", "--name", "pending_items",
             "--text", "- second item")
    assert r.returncode == 0, r.stderr
    r = _cli(tmp_path, "slot-get", "--name", "pending_items")
    assert r.returncode == 0
    slot = json.loads(r.stdout)
    assert slot["content"] == "- wire the retry budget\n- second item"

    r = _cli(tmp_path, "slot-replace", "--name", "pending_items",
             "--content", "- compacted")
    assert r.returncode == 0, r.stderr
    r = _cli(tmp_path, "slot-get", "--name", "pending_items")
    assert json.loads(r.stdout)["content"] == "- compacted"


def test_agent_delete_empties_but_keeps_slot(tmp_path):
    assert _cli(tmp_path, "slot-append", "--name", "self_notes",
                "--text", "- scratch").returncode == 0
    assert _cli(tmp_path, "slot-delete", "--name", "self_notes").returncode == 0
    r = _cli(tmp_path, "slot-get", "--name", "self_notes")
    assert r.returncode == 0  # row survives — fixed vocabulary
    assert json.loads(r.stdout)["content"] == ""


def test_unknown_slot_name_is_an_error(tmp_path):
    r = _cli(tmp_path, "slot-append", "--name", "made_up", "--text", "x")
    assert r.returncode == 1
    assert "not found" in r.stderr


def test_skill_doc_exposes_slot_operations():
    text = SLOTS_SKILL.read_text(encoding="utf-8")
    for op in ("slot-list", "slot-get", "slot-append", "slot-replace", "slot-delete"):
        assert op in text, f"skill doc missing operation: {op}"
    for name in ("persona", "user_preferences", "tool_guidelines", "project_context",
                 "guidance", "pending_items", "session_patterns", "self_notes"):
        assert name in text, f"skill doc missing default slot: {name}"


# --- Acceptance 5: size cap enforced ------------------------------------------


def test_append_over_cap_is_rejected(conn):
    reflect_db.ensure_default_slots(PROJECT, conn=conn)
    result = reflect_db.slot_append(
        "persona", "x" * 1001, project_id=PROJECT, conn=conn,
    )
    assert result["ok"] is False
    assert "size_limit" in result["error"]
    assert reflect_db.get_slot("persona", project_id=PROJECT, conn=conn)["content"] == ""


def test_replace_over_cap_is_rejected(conn):
    reflect_db.ensure_default_slots(PROJECT, conn=conn)
    result = reflect_db.slot_replace(
        "guidance", "y" * 1501, project_id=PROJECT, conn=conn,
    )
    assert result["ok"] is False
    assert "size_limit" in result["error"]


def test_auto_append_truncates_to_cap_keeping_tail(conn):
    reflect_db.ensure_default_slots(PROJECT, conn=conn)
    lines = [f"- item {i:04d} " + "x" * 60 for i in range(60)]
    assert reflect_db.slot_auto_append(
        "pending_items", lines, project_id=PROJECT, conn=conn,
    )
    slot = reflect_db.get_slot("pending_items", project_id=PROJECT, conn=conn)
    assert len(slot["content"]) <= slot["size_limit"]
    assert "item 0059" in slot["content"]  # newest entries survive


def test_read_only_slot_rejects_edits_and_auto_writes(conn):
    reflect_db.ensure_default_slots(PROJECT, conn=conn)
    with conn:
        conn.execute(
            "UPDATE slots SET read_only = 1 WHERE name = 'guidance'",
        )
    assert reflect_db.slot_append("guidance", "nope", project_id=PROJECT, conn=conn)["ok"] is False
    assert reflect_db.slot_replace("guidance", "nope", project_id=PROJECT, conn=conn)["ok"] is False
    assert reflect_db.slot_delete("guidance", project_id=PROJECT, conn=conn)["ok"] is False
    assert reflect_db.slot_auto_append("guidance", ["- x"], project_id=PROJECT, conn=conn) is False
    assert reflect_db.slot_auto_replace("guidance", "x", project_id=PROJECT, conn=conn) is False


# --- Acceptance 3: stop hook auto-appends TODOs (no LLM) ----------------------


def _transcript(path: Path) -> Path:
    """Synthetic Claude transcript: TodoWrite, TODO chatter, Bash, Edit, error."""
    records = [
        {"message": {"role": "assistant", "content": [
            {"type": "tool_use", "name": "TodoWrite", "input": {"todos": [
                {"content": "wire retry budget into drain loop", "status": "pending"},
                {"content": "ship the already-finished thing", "status": "completed"},
            ]}},
        ]}},
        {"message": {"role": "assistant", "content": [
            {"type": "text", "text": "Done for now.\nTODO: fix the flaky retry test"},
            {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
            {"type": "tool_use", "name": "Bash", "input": {"command": "pytest"}},
            {"type": "tool_use", "name": "Edit", "input": {"file_path": "/repo/src/app.py"}},
        ]}},
        {"message": {"role": "user", "content": [
            {"type": "tool_result", "is_error": True, "content": "boom"},
        ]}},
    ]
    path.write_text("\n".join(json.dumps(r) for r in records), encoding="utf-8")
    return path


def _run_stop_hook(tmp_path: Path, *, flag: str | None = "1") -> subprocess.CompletedProcess:
    projdir = tmp_path / PROJECT
    projdir.mkdir(exist_ok=True)
    transcript = _transcript(tmp_path / "transcript.jsonl")
    env = {
        "PATH": "",  # no git → project id falls back to cwd basename
        "HOME": str(tmp_path),
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "REFLECT_DB_PATH": str(tmp_path / "reflect.db"),
    }
    if flag is not None:
        env["REFLECT_SLOTS"] = flag
    payload = json.dumps({
        "session_id": "sess-a1",
        "transcript_path": str(transcript),
        "cwd": str(projdir),
    })
    return subprocess.run(
        [sys.executable, str(STOP_HOOK)],
        input=payload,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )


def _slot_content(tmp_path: Path, name: str) -> str:
    db = sqlite3.connect(tmp_path / "reflect.db")
    try:
        row = db.execute(
            "SELECT content FROM slots WHERE name = ? AND project_id = ?",
            (name, PROJECT),
        ).fetchone()
        return row[0] if row else ""
    finally:
        db.close()


def test_stop_hook_appends_todos_to_pending_items(tmp_path):
    r = _run_stop_hook(tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == ""  # Stop hook contract: empty stdout

    pending = _slot_content(tmp_path, "pending_items")
    assert "wire retry budget into drain loop" in pending
    assert "TODO: fix the flaky retry test" in pending
    assert "already-finished thing" not in pending  # completed todos excluded


def test_stop_hook_counts_patterns_and_records_files(tmp_path):
    _run_stop_hook(tmp_path)
    patterns = _slot_content(tmp_path, "session_patterns")
    assert "commands: 2" in patterns
    assert "errors: 1" in patterns
    context = _slot_content(tmp_path, "project_context")
    assert "/repo/src/app.py" in context


def test_stop_hook_slot_reflect_is_idempotent(tmp_path):
    _run_stop_hook(tmp_path)
    first = _slot_content(tmp_path, "pending_items")
    _run_stop_hook(tmp_path)
    assert _slot_content(tmp_path, "pending_items") == first


def test_stop_hook_flag_off_writes_nothing(tmp_path):
    r = _run_stop_hook(tmp_path, flag=None)
    assert r.returncode == 0
    assert not (tmp_path / "reflect.db").exists()


# --- Acceptance 4: SessionStart includes slots before recall results ----------


def _fake_uv(bin_dir: Path) -> Path:
    bin_dir.mkdir(parents=True, exist_ok=True)
    uv = bin_dir / "uv"
    uv.write_text(f"#!/bin/sh\necho '{FAKE_LEARNING}'\n", encoding="utf-8")
    uv.chmod(0o755)
    return bin_dir


def _seed_slots(tmp_path: Path) -> None:
    connection = reflect_db.init_db(tmp_path / "reflect.db")
    try:
        reflect_db.ensure_default_slots(PROJECT, conn=connection)
        assert reflect_db.slot_replace(
            "guidance", "Focus on flake retries.", project_id=PROJECT,
            conn=connection,
        )["ok"]
    finally:
        reflect_db.close_all()


def _run_session_hook(tmp_path: Path, *, flag: str | None = "1",
                      extra: dict[str, str] | None = None) -> str:
    projdir = tmp_path / PROJECT
    projdir.mkdir(exist_ok=True)
    (tmp_path / "home").mkdir(exist_ok=True)
    env = {
        "PATH": str(_fake_uv(tmp_path / "uvbin")),  # fake uv; no git
        "HOME": str(tmp_path / "home"),
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "REFLECT_DB_PATH": str(tmp_path / "reflect.db"),
        "REFLECT_SKILLS_DIR": str(tmp_path / "skills"),
        "CLAUDE_PROJECT_DIR": str(projdir),
    }
    if flag is not None:
        env["REFLECT_SLOTS"] = flag
    if extra:
        env.update(extra)
    result = subprocess.run(
        [sys.executable, str(SESSION_HOOK)],
        input="{}",
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    assert result.returncode == 0, (
        f"hook exited non-zero:\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    out = json.loads(result.stdout)["hookSpecificOutput"]
    assert out["hookEventName"] == "SessionStart"
    return out["additionalContext"]


def test_session_start_injects_slots_before_recall_results(tmp_path):
    _seed_slots(tmp_path)
    ctx = _run_session_hook(tmp_path)
    assert "Focus on flake retries." in ctx, f"slot content missing: {ctx!r}"
    assert "lrn-fake-1" in ctx, f"recall results missing: {ctx!r}"
    assert ctx.index("Focus on flake retries.") < ctx.index("lrn-fake-1"), (
        f"slots must inject BEFORE recall results: {ctx!r}"
    )


def test_session_start_slots_inject_before_skills_tier(tmp_path):
    _seed_slots(tmp_path)
    skill_dir = tmp_path / "skills" / "webapp-testing"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        "---\nname: webapp-testing\ndescription: |\n"
        "  Drive Playwright browser tests for webapps.\n"
        "tags:\n  - playwright\n---\n\n# webapp-testing\nbody\n",
        encoding="utf-8",
    )
    ctx = _run_session_hook(tmp_path, extra={"REFLECT_TIERED_INJECT": "1"})
    assert "Focus on flake retries." in ctx
    assert "webapp-testing" in ctx
    assert ctx.index("Focus on flake retries.") < ctx.index("webapp-testing"), (
        f"slots (Tier-0) must precede the skills tier: {ctx!r}"
    )


def test_session_start_empty_slots_change_nothing(tmp_path):
    # Flag on, but no slot has content → plain recall inject, no slot block.
    ctx = _run_session_hook(tmp_path)
    assert "lrn-fake-1" in ctx
    assert "Memory slots" not in ctx


def test_session_start_flag_off_by_default(tmp_path):
    _seed_slots(tmp_path)
    ctx = _run_session_hook(tmp_path, flag=None)
    assert "lrn-fake-1" in ctx
    assert "Focus on flake retries." not in ctx


def test_session_start_db_failure_degrades_silently(tmp_path):
    ctx = _run_session_hook(
        tmp_path,
        extra={"REFLECT_DB_PATH": "/dev/null/nope/reflect.db"},
    )
    assert "lrn-fake-1" in ctx  # recall still injected, exit 0


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
