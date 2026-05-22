"""Tests for the Codex adapter (plugins/reflect/adapters/codex)."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
ADAPTER_DIR = HERE.parent / "codex"
ADAPTER = ADAPTER_DIR / "codex_adapter.py"

sys.path.insert(0, str(ADAPTER_DIR))

import codex_adapter  # noqa: E402


@pytest.fixture(autouse=True)
def _sanity():
    assert ADAPTER.exists(), f"missing adapter script at {ADAPTER}"


def test_find_plugin_root_resolves_to_reflect_dir():
    root = codex_adapter.find_plugin_root()
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
    assert not (tmp_path / ".codex").exists()


def test_install_writes_pointer_files_under_dot_codex(tmp_path):
    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr

    skills_root = tmp_path / ".codex" / "skills"
    assert skills_root.is_dir()

    recall = skills_root / "recall" / "SKILL.md"
    reflect = skills_root / "reflect" / "SKILL.md"
    assert recall.exists()
    assert reflect.exists()

    body = recall.read_text(encoding="utf-8")
    assert codex_adapter.POINTER_MANAGED_BY in body
    assert "name: reflect:recall" in body
    # Codex uses hooks.json, not settings.json
    assert not (tmp_path / ".codex" / "settings.json").exists()
    # And it must not have leaked into a Claude dir.
    assert not (tmp_path / ".claude").exists()


def test_install_syncs_recall_hooks_and_scripts(tmp_path):
    """Codex needs the hook script files physically present so the hook
    command in hooks.json resolves at runtime. Validate the recall hook
    and the plugin-level reflect hook both land under ~/.codex/skills/."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    recall_hook = (
        tmp_path / ".codex" / "skills" / "recall" / "hooks"
        / "session_start_recall.py"
    )
    assert recall_hook.exists()
    assert recall_hook.read_text().startswith("#!/usr/bin/env -S uv run")

    precompact = (
        tmp_path / ".codex" / "skills" / "reflect" / "hooks"
        / "precompact_reflect.py"
    )
    assert precompact.exists()

    drain = (
        tmp_path / ".codex" / "skills" / "reflect" / "hooks"
        / "reflect-drain-bg.sh"
    )
    assert drain.exists()


def test_install_writes_hooks_json_with_all_three_entries(tmp_path):
    """Default install wires SessionStart-recall, SessionStart-drain, and
    PreCompact-reflect into ~/.codex/hooks.json."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    hooks_path = tmp_path / ".codex" / "hooks.json"
    assert hooks_path.exists()
    cfg = json.loads(hooks_path.read_text())
    ss_cmds = [
        h["command"]
        for entry in cfg["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    pc_cmds = [
        h["command"]
        for entry in cfg["hooks"]["PreCompact"]
        for h in entry["hooks"]
    ]
    codex_dir = tmp_path / ".codex"
    assert codex_adapter._render_recall_hook_command(codex_dir) in ss_cmds
    assert codex_adapter._render_drain_hook_command(codex_dir) in ss_cmds
    assert codex_adapter._render_precompact_hook_command(codex_dir) in pc_cmds

    # No surviving template placeholders in the persisted file.
    text = hooks_path.read_text()
    assert "{{" not in text
    assert "{home_tool_dir}" not in text


def test_install_writes_new_hooks_for_3_6_0(tmp_path):
    """3.6.0 adds UserPromptSubmit, PostToolUse, and Stop hooks.
    Verify they all land in ~/.codex/hooks.json under the right events."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    cfg = json.loads((tmp_path / ".codex" / "hooks.json").read_text())
    codex_dir = tmp_path / ".codex"

    ups_cmds = [
        h["command"]
        for entry in cfg["hooks"]["UserPromptSubmit"]
        for h in entry["hooks"]
    ]
    ptu_cmds = [
        h["command"]
        for entry in cfg["hooks"]["PostToolUse"]
        for h in entry["hooks"]
    ]
    stop_cmds = [
        h["command"]
        for entry in cfg["hooks"]["Stop"]
        for h in entry["hooks"]
    ]
    assert codex_adapter._render_user_prompt_recall_command(codex_dir) in ups_cmds
    assert codex_adapter._render_posttooluse_minilearning_command(codex_dir) in ptu_cmds
    assert codex_adapter._render_stop_reflect_command(codex_dir) in stop_cmds


def test_install_deploys_new_hook_scripts_on_disk(tmp_path):
    """The three new hook script files must physically exist under
    ~/.codex/skills/ — otherwise the hook commands in hooks.json
    reference non-existent paths and the harness silently skips them."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    codex_dir = tmp_path / ".codex"
    expected = [
        codex_dir / "skills" / "recall" / "hooks" / "user_prompt_submit_recall.py",
        codex_dir / "skills" / "reflect" / "hooks" / "posttooluse_minilearning.py",
        codex_dir / "skills" / "reflect" / "hooks" / "stop_reflect.py",
    ]
    for path in expected:
        assert path.exists(), f"missing on disk: {path}"


def test_uninstall_removes_new_hooks_too(tmp_path):
    """Uninstall must strip all five reflect-managed events
    (SessionStart, PreCompact, UserPromptSubmit, PostToolUse, Stop)."""
    # Pre-seed with an unrelated entry under each event to verify it survives.
    codex_dir = tmp_path / ".codex"
    codex_dir.mkdir()
    (codex_dir / "hooks.json").write_text(json.dumps({
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "echo other-ups"}]}
            ],
            "PostToolUse": [
                {"hooks": [{"type": "command", "command": "echo other-ptu"}]}
            ],
            "Stop": [
                {"hooks": [{"type": "command", "command": "echo other-stop"}]}
            ],
        }
    }))
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    cfg = json.loads((codex_dir / "hooks.json").read_text())
    # Map each event to (seeded-unrelated-cmd, reflect-render-fn)
    spec = {
        "UserPromptSubmit": ("echo other-ups", codex_adapter._render_user_prompt_recall_command),
        "PostToolUse":      ("echo other-ptu", codex_adapter._render_posttooluse_minilearning_command),
        "Stop":             ("echo other-stop", codex_adapter._render_stop_reflect_command),
    }
    for event, (other_cmd, render_fn) in spec.items():
        cmds = [
            h["command"]
            for entry in cfg.get("hooks", {}).get(event, [])
            for h in entry["hooks"]
        ]
        assert other_cmd in cmds, f"unrelated {event} hook lost! cmds={cmds}"
        # Reflect entry should be gone
        assert render_fn(codex_dir) not in cmds


def test_install_no_hooks_flag_skips_hooks_json(tmp_path):
    subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-hooks"],
        check=True, capture_output=True,
    )
    assert (tmp_path / ".codex" / "skills" / "recall" / "SKILL.md").exists()
    assert not (tmp_path / ".codex" / "hooks.json").exists()


def test_install_no_bg_drain_omits_drain_hook(tmp_path):
    """--no-bg-drain wires only SessionStart-recall + PreCompact, not the
    drain shell script (useful on codex-only machines without claude)."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-bg-drain"],
        check=True, capture_output=True,
    )
    cfg = json.loads((tmp_path / ".codex" / "hooks.json").read_text())
    ss_cmds = [
        h["command"]
        for entry in cfg["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    codex_dir = tmp_path / ".codex"
    assert codex_adapter._render_recall_hook_command(codex_dir) in ss_cmds
    assert codex_adapter._render_drain_hook_command(codex_dir) not in ss_cmds
    # PreCompact still wired
    pc_cmds = [
        h["command"]
        for entry in cfg["hooks"]["PreCompact"]
        for h in entry["hooks"]
    ]
    assert codex_adapter._render_precompact_hook_command(codex_dir) in pc_cmds


def test_install_preserves_existing_unrelated_hooks_json(tmp_path):
    """Adapter must merge into an existing hooks.json without nuking
    unrelated entries already wired by other tools (eg. superset)."""
    codex_dir = tmp_path / ".codex"
    codex_dir.mkdir()
    existing = {
        "hooks": {
            "SessionStart": [
                {"hooks": [{"type": "command", "command": "echo superset-ss"}]}
            ],
            "Stop": [
                {"hooks": [{"type": "command", "command": "echo superset-stop"}]}
            ],
        }
    }
    (codex_dir / "hooks.json").write_text(json.dumps(existing))

    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    cfg = json.loads((codex_dir / "hooks.json").read_text())
    all_ss_cmds = [
        h["command"]
        for entry in cfg["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    all_stop_cmds = [
        h["command"]
        for entry in cfg["hooks"]["Stop"]
        for h in entry["hooks"]
    ]
    # Pre-existing unrelated entries survived
    assert "echo superset-ss" in all_ss_cmds
    assert "echo superset-stop" in all_stop_cmds
    # And our reflect entries landed
    assert codex_adapter._render_recall_hook_command(codex_dir) in all_ss_cmds


def test_install_idempotent_hooks_json(tmp_path):
    """Two installs in a row: hook entries should appear exactly once."""
    for _ in range(2):
        subprocess.run(
            [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
            check=True, capture_output=True,
        )
    cfg = json.loads((tmp_path / ".codex" / "hooks.json").read_text())
    codex_dir = tmp_path / ".codex"
    ss_cmds = [
        h["command"]
        for entry in cfg["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    assert ss_cmds.count(codex_adapter._render_recall_hook_command(codex_dir)) == 1
    assert ss_cmds.count(codex_adapter._render_drain_hook_command(codex_dir)) == 1


def test_install_cleans_up_legacy_unsubstituted_hook(tmp_path):
    """Legacy {{HOME_TOOL_DIR}} hooks from earlier installs should be
    swept out and replaced with the correctly-rendered command."""
    codex_dir = tmp_path / ".codex"
    codex_dir.mkdir()
    legacy = (
        "uv run {{HOME_TOOL_DIR}}/skills/recall/hooks/session_start_recall.py"
    )
    (codex_dir / "hooks.json").write_text(json.dumps({
        "hooks": {
            "SessionStart": [
                {"matcher": "", "hooks": [{"type": "command", "command": legacy}]}
            ]
        }
    }))

    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    cfg_text = (codex_dir / "hooks.json").read_text()
    assert "{{HOME_TOOL_DIR}}" not in cfg_text
    cfg = json.loads(cfg_text)
    ss_cmds = [
        h["command"]
        for entry in cfg["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    assert legacy not in ss_cmds
    assert ss_cmds.count(codex_adapter._render_recall_hook_command(codex_dir)) == 1


def test_install_errors_on_corrupt_hooks_json(tmp_path):
    codex_dir = tmp_path / ".codex"
    codex_dir.mkdir()
    (codex_dir / "hooks.json").write_text("{ not valid json")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "hooks.json" in result.stderr.lower()


def test_uninstall_removes_hook_entries(tmp_path):
    """Uninstall strips reflect hook entries but leaves unrelated ones."""
    codex_dir = tmp_path / ".codex"
    codex_dir.mkdir()
    (codex_dir / "hooks.json").write_text(json.dumps({
        "hooks": {
            "SessionStart": [
                {"hooks": [{"type": "command", "command": "echo unrelated"}]}
            ]
        }
    }))
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    cfg = json.loads((codex_dir / "hooks.json").read_text())
    ss_cmds = [
        h["command"]
        for entry in cfg.get("hooks", {}).get("SessionStart", [])
        for h in entry["hooks"]
    ]
    assert "echo unrelated" in ss_cmds
    assert codex_adapter._render_recall_hook_command(codex_dir) not in ss_cmds
    # PreCompact had only our entries → block dropped entirely
    assert "PreCompact" not in cfg.get("hooks", {})


def test_install_is_idempotent(tmp_path):
    for _ in range(2):
        subprocess.run(
            [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
            check=True, capture_output=True,
        )
    # Same set of pointer files; no error.
    pointers = list((tmp_path / ".codex" / "skills").rglob("SKILL.md"))
    assert len(pointers) >= 2  # recall + reflect at minimum


def test_install_is_idempotent_and_preserves_pre_seeded_user_files(tmp_path):
    """Re-running install must not destroy pre-existing user state under
    ~/.codex/skills/. Codex has no hook system to test the
    "preserve existing hooks" half (Claude does), so we cover the
    sibling-file half: a hand-written file inside an *adapter-managed*
    skill dir must survive multiple install cycles."""
    # First install creates the managed pointer + dir.
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    user_sibling = tmp_path / ".codex" / "skills" / "recall" / "user-note.md"
    user_sibling.write_text("hand-written sibling", encoding="utf-8")

    # Second install must be a no-op for user state.
    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True, text=True,
    )
    # Still exactly one managed pointer per skill, no duplicates created.
    pointers = list((tmp_path / ".codex" / "skills").rglob("SKILL.md"))
    assert len(pointers) >= 2
    # User's sibling file untouched.
    assert user_sibling.read_text(encoding="utf-8") == "hand-written sibling"
    # Adapter reported writing/keeping pointers — never anything destructive.
    assert "refused to overwrite" not in result.stdout


def test_uninstall_removes_only_managed_pointers(tmp_path):
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    user_file = tmp_path / ".codex" / "skills" / "recall" / "user-note.md"
    user_file.write_text("hand-written", encoding="utf-8")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr

    assert not (tmp_path / ".codex" / "skills" / "recall" / "SKILL.md").exists()
    assert user_file.exists()


def test_install_refuses_to_overwrite_non_pointer_skill_marker(tmp_path):
    """Sentinel-aware skip: hand-written SKILL.md siblings must NOT be
    silently replaced. Default install refuses and exits non-zero."""
    codex_dir = tmp_path / ".codex"
    (codex_dir / "skills" / "recall").mkdir(parents=True)
    handwritten = "---\nname: user-handwritten\n---\nbody\n"
    target = codex_dir / "skills" / "recall" / "SKILL.md"
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
    codex_dir = tmp_path / ".codex"
    (codex_dir / "skills" / "recall").mkdir(parents=True)
    target = codex_dir / "skills" / "recall" / "SKILL.md"
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
    assert codex_adapter.POINTER_MANAGED_BY in body
