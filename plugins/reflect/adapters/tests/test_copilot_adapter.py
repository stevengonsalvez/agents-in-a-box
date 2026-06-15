"""Tests for the Copilot adapter (plugins/reflect/adapters/copilot).

Copilot grew a native hook system (GA Feb 2026), so this adapter wires
full hook parity — but in **Copilot's own drop-in format**, which differs
from the Claude/Codex shape on every axis (camelCase events, flat arrays,
``version:1``, ``timeoutSec``, one owned file). These tests mirror the
Codex adapter's hook coverage retargeted to that native format and assert
the JSON is genuinely copilot-native, NOT claude-shaped.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
ADAPTER_DIR = HERE.parent / "copilot"
ADAPTER = ADAPTER_DIR / "copilot_adapter.py"

sys.path.insert(0, str(ADAPTER_DIR))

import copilot_adapter  # noqa: E402


def _hooks_path(home: Path) -> Path:
    return home / ".copilot" / "hooks" / "reflect.json"


@pytest.fixture(autouse=True)
def _sanity():
    assert ADAPTER.exists(), f"missing adapter script at {ADAPTER}"


# --- pointer / skill-deploy tests (kept + adapted) -----------------------

def test_find_plugin_root_resolves_to_reflect_dir():
    root = copilot_adapter.find_plugin_root()
    assert (root / "skills").is_dir()
    assert (root / "adapters").is_dir()
    assert root.name == "reflect"


def test_dry_run_reports_actions_without_touching_home(tmp_path):
    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--dry-run", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    assert "dry-run" in result.stdout
    assert "pointer:" in result.stdout
    assert "recall" in result.stdout
    # Native-format hint visible in the plan, but nothing written.
    assert "copilot-native" in result.stdout
    assert "camelCase events" in result.stdout
    assert not (tmp_path / ".copilot").exists()


def test_install_writes_pointer_files_under_dot_copilot(tmp_path):
    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr

    skills_root = tmp_path / ".copilot" / "skills"
    assert skills_root.is_dir()

    recall = skills_root / "recall" / "SKILL.md"
    reflect = skills_root / "reflect" / "SKILL.md"
    assert recall.exists()
    assert reflect.exists()

    body = recall.read_text(encoding="utf-8")
    assert copilot_adapter.POINTER_MANAGED_BY in body
    assert "name: reflect:recall" in body
    # Adapter must not have leaked into Claude/Codex dirs.
    assert not (tmp_path / ".claude").exists()
    assert not (tmp_path / ".codex").exists()


def test_install_syncs_recall_hooks_and_scripts(tmp_path):
    """Copilot needs the hook script files physically present so the
    command in reflect.json resolves at runtime. Validate the recall hook
    and the plugin-level reflect hooks both land under ~/.copilot/skills/."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    recall_hook = (
        tmp_path / ".copilot" / "skills" / "recall" / "hooks"
        / "session_start_recall.py"
    )
    assert recall_hook.exists()

    precompact = (
        tmp_path / ".copilot" / "skills" / "reflect" / "hooks"
        / "precompact_reflect.py"
    )
    assert precompact.exists()

    drain = (
        tmp_path / ".copilot" / "skills" / "reflect" / "hooks"
        / "reflect-drain-bg.sh"
    )
    assert drain.exists()


def test_install_deploys_new_hook_scripts_on_disk(tmp_path):
    """The capture hook scripts must physically exist under
    ~/.copilot/skills/ — otherwise the commands in reflect.json reference
    non-existent paths and the harness silently skips them."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    copilot_dir = tmp_path / ".copilot"
    expected = [
        copilot_dir / "skills" / "recall" / "hooks" / "user_prompt_submit_recall.py",
        copilot_dir / "skills" / "reflect" / "hooks" / "posttooluse_minilearning.py",
        copilot_dir / "skills" / "reflect" / "hooks" / "stop_reflect.py",
        # The cross-harness stdin helper must land too (under the umbrella).
        copilot_dir / "skills" / "reflect" / "scripts" / "hook_input.py",
    ]
    for path in expected:
        assert path.exists(), f"missing on disk: {path}"


# --- native-format hook tests --------------------------------------------

def test_install_writes_reflect_json_with_all_six_hooks(tmp_path):
    """Default install wires all 6 reflect hooks across the 5 copilot
    events into ~/.copilot/hooks/reflect.json."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    hooks_path = _hooks_path(tmp_path)
    assert hooks_path.exists()
    cfg = json.loads(hooks_path.read_text())
    copilot_dir = tmp_path / ".copilot"

    def cmds(event: str) -> list[str]:
        return [e["command"] for e in cfg["hooks"][event]]

    assert copilot_adapter._render_recall_hook_command(copilot_dir) in cmds("sessionStart")
    assert copilot_adapter._render_drain_hook_command(copilot_dir) in cmds("sessionStart")
    assert copilot_adapter._render_precompact_hook_command(copilot_dir) in cmds("preCompact")
    assert copilot_adapter._render_posttooluse_minilearning_command(copilot_dir) in cmds("postToolUse")
    assert copilot_adapter._render_stop_reflect_command(copilot_dir) in cmds("agentStop")
    assert copilot_adapter._render_user_prompt_recall_command(copilot_dir) in cmds("userPromptSubmitted")

    # No surviving template placeholders in the persisted file.
    text = hooks_path.read_text()
    assert "{{" not in text
    assert "{home_tool_dir}" not in text


def test_reflect_json_is_copilot_native_not_claude_shaped(tmp_path):
    """The drop-in must be Copilot-native, not the Claude/Codex two-level
    shape. Assert: version:1, flat arrays per event, camelCase event keys,
    timeoutSec (not timeout), and the ABSENCE of the claude shape (no
    ``matcher`` key, no nested ``hooks`` inside entries)."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    cfg = json.loads(_hooks_path(tmp_path).read_text())

    # version:1 at top level.
    assert cfg["version"] == 1

    # camelCase event keys (NOT the PascalCase claude/codex names).
    events = set(cfg["hooks"].keys())
    assert events == {
        "sessionStart", "preCompact", "postToolUse",
        "agentStop", "userPromptSubmitted",
    }
    for pascal in ("SessionStart", "PreCompact", "PostToolUse", "Stop", "UserPromptSubmit"):
        assert pascal not in cfg["hooks"]

    for event, entries in cfg["hooks"].items():
        # Flat array of command entries.
        assert isinstance(entries, list), event
        for entry in entries:
            assert entry["type"] == "command", entry
            assert "command" in entry, entry
            # NOT the claude two-level shape.
            assert "matcher" not in entry, f"{event} entry has claude matcher"
            assert "hooks" not in entry, f"{event} entry has nested hooks (claude shape)"
            # Copilot uses timeoutSec, never the claude `timeout` field.
            assert "timeout" not in entry, f"{event} entry uses claude `timeout`"

    # The drain entry carries timeoutSec.
    drain_entries = [
        e for e in cfg["hooks"]["sessionStart"] if "nohup" in e["command"]
    ]
    assert drain_entries, "drain entry missing"
    assert drain_entries[0]["timeoutSec"] == 5


def test_recall_commands_set_reflect_harness_env(tmp_path):
    """The uv-run hook commands must set REFLECT_HARNESS=copilot so the
    recall hooks emit the copilot additionalContext envelope and the stdin
    readers pick camelCase keys. The drain (a subshell) must NOT carry the
    prefix (invalid shell syntax before a subshell)."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    cfg = json.loads(_hooks_path(tmp_path).read_text())

    for event in ("preCompact", "postToolUse", "agentStop", "userPromptSubmitted"):
        for entry in cfg["hooks"][event]:
            assert entry["command"].startswith("REFLECT_HARNESS=copilot uv run "), entry

    ss = {("drain" if "nohup" in e["command"] else "recall"): e["command"]
          for e in cfg["hooks"]["sessionStart"]}
    assert ss["recall"].startswith("REFLECT_HARNESS=copilot uv run ")
    # Drain is a subshell — env prefix would be invalid there.
    assert not ss["drain"].startswith("REFLECT_HARNESS=")
    assert ss["drain"].startswith("(nohup ")


def test_install_no_hooks_flag_skips_reflect_json(tmp_path):
    subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-hooks"],
        check=True, capture_output=True,
    )
    assert (tmp_path / ".copilot" / "skills" / "recall" / "SKILL.md").exists()
    assert not _hooks_path(tmp_path).exists()


def test_install_no_bg_drain_omits_drain_hook(tmp_path):
    """--no-bg-drain wires sessionStart-recall + the capture hooks but not
    the drain shell script."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-bg-drain"],
        check=True, capture_output=True,
    )
    cfg = json.loads(_hooks_path(tmp_path).read_text())
    copilot_dir = tmp_path / ".copilot"
    ss_cmds = [e["command"] for e in cfg["hooks"]["sessionStart"]]
    assert copilot_adapter._render_recall_hook_command(copilot_dir) in ss_cmds
    assert copilot_adapter._render_drain_hook_command(copilot_dir) not in ss_cmds
    # preCompact still wired.
    pc_cmds = [e["command"] for e in cfg["hooks"]["preCompact"]]
    assert copilot_adapter._render_precompact_hook_command(copilot_dir) in pc_cmds


def test_install_idempotent_reflect_json(tmp_path):
    """Two installs in a row: each hook command appears exactly once."""
    for _ in range(2):
        subprocess.run(
            [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
            check=True, capture_output=True,
        )
    cfg = json.loads(_hooks_path(tmp_path).read_text())
    copilot_dir = tmp_path / ".copilot"
    ss_cmds = [e["command"] for e in cfg["hooks"]["sessionStart"]]
    assert ss_cmds.count(copilot_adapter._render_recall_hook_command(copilot_dir)) == 1
    assert ss_cmds.count(copilot_adapter._render_drain_hook_command(copilot_dir)) == 1
    # Exactly one entry under each single-command event.
    for event in ("preCompact", "postToolUse", "agentStop", "userPromptSubmitted"):
        assert len(cfg["hooks"][event]) == 1, event


def test_install_errors_on_corrupt_reflect_json(tmp_path):
    hooks_dir = tmp_path / ".copilot" / "hooks"
    hooks_dir.mkdir(parents=True)
    (hooks_dir / "reflect.json").write_text("{ not valid json")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "reflect.json" in result.stderr.lower()
    # The corrupt file must NOT have been clobbered.
    assert (hooks_dir / "reflect.json").read_text() == "{ not valid json"


def test_install_preserves_foreign_json_in_hooks_dir(tmp_path):
    """The drop-in dir combines all *.json; we own only reflect.json.
    A foreign sibling (e.g. another tool's hooks) must survive install."""
    hooks_dir = tmp_path / ".copilot" / "hooks"
    hooks_dir.mkdir(parents=True)
    foreign = hooks_dir / "other-tool.json"
    foreign_content = json.dumps({"version": 1, "hooks": {"sessionStart": []}})
    foreign.write_text(foreign_content)

    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    assert _hooks_path(tmp_path).exists()
    assert foreign.read_text() == foreign_content


def test_uninstall_removes_reflect_json_and_leaves_foreign(tmp_path):
    """Uninstall deletes our reflect.json drop-in and leaves any foreign
    *.json siblings in the dir untouched."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    hooks_dir = tmp_path / ".copilot" / "hooks"
    foreign = hooks_dir / "other-tool.json"
    foreign.write_text('{"version":1,"hooks":{}}')

    subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    assert not _hooks_path(tmp_path).exists()
    assert foreign.exists()
    assert foreign.read_text() == '{"version":1,"hooks":{}}'


def test_uninstall_no_hooks_flag_leaves_reflect_json(tmp_path):
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall",
         "--home", str(tmp_path), "--no-hooks"],
        check=True, capture_output=True,
    )
    assert _hooks_path(tmp_path).exists()


# --- existing pointer-mechanic tests (kept) ------------------------------

def test_install_is_idempotent(tmp_path):
    for _ in range(2):
        subprocess.run(
            [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
            check=True, capture_output=True,
        )
    pointers = list((tmp_path / ".copilot" / "skills").rglob("SKILL.md"))
    assert len(pointers) >= 2  # recall + reflect at minimum


def test_install_is_idempotent_and_preserves_pre_seeded_user_files(tmp_path):
    """Re-running install must not destroy pre-existing user state under
    ~/.copilot/skills/. A hand-written file inside an adapter-managed skill
    dir must survive multiple install cycles."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    user_sibling = tmp_path / ".copilot" / "skills" / "recall" / "user-note.md"
    user_sibling.write_text("hand-written sibling", encoding="utf-8")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True, text=True,
    )
    pointers = list((tmp_path / ".copilot" / "skills").rglob("SKILL.md"))
    assert len(pointers) >= 2
    assert user_sibling.read_text(encoding="utf-8") == "hand-written sibling"
    assert "refused to overwrite" not in result.stdout


def test_uninstall_removes_only_managed_pointers(tmp_path):
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    user_file = tmp_path / ".copilot" / "skills" / "recall" / "user-note.md"
    user_file.write_text("hand-written", encoding="utf-8")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr

    assert not (tmp_path / ".copilot" / "skills" / "recall" / "SKILL.md").exists()
    assert user_file.exists()


def test_install_refuses_to_overwrite_non_pointer_skill_marker(tmp_path):
    """Sentinel-aware skip: hand-written SKILL.md siblings must NOT be
    silently replaced. Default install refuses and exits non-zero."""
    copilot_dir = tmp_path / ".copilot"
    (copilot_dir / "skills" / "recall").mkdir(parents=True)
    handwritten = "---\nname: user-handwritten\n---\nbody\n"
    target = copilot_dir / "skills" / "recall" / "SKILL.md"
    target.write_text(handwritten, encoding="utf-8")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode != 0, result.stdout
    assert "refused to overwrite non-pointer file" in result.stdout
    assert target.read_text(encoding="utf-8") == handwritten


def test_install_force_replaces_non_pointer_skill_marker(tmp_path):
    """With ``--force`` the adapter explicitly replaces the foreign file."""
    copilot_dir = tmp_path / ".copilot"
    (copilot_dir / "skills" / "recall").mkdir(parents=True)
    target = copilot_dir / "skills" / "recall" / "SKILL.md"
    target.write_text(
        "---\nname: user-handwritten\n---\nbody\n", encoding="utf-8"
    )

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--force"],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    assert "replaced non-pointer file" in result.stdout
    body = target.read_text(encoding="utf-8")
    assert copilot_adapter.POINTER_MANAGED_BY in body
