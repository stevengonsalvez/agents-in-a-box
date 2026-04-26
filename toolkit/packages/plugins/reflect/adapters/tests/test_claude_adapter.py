"""Tests for the Claude Code adapter (toolkit/packages/plugins/reflect/adapters/claude).

The test runs the adapter against a temp HOME so it exercises the real
install path without touching the invoking user's ~/.claude.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
ADAPTER_DIR = HERE.parent / "claude"
ADAPTER = ADAPTER_DIR / "claude_adapter.py"

# Make ``claude_adapter`` importable regardless of where pytest runs from.
sys.path.insert(0, str(ADAPTER_DIR))

import claude_adapter  # noqa: E402


@pytest.fixture(autouse=True)
def _sanity():
    assert ADAPTER.exists(), f"missing adapter script at {ADAPTER}"


def test_find_plugin_root_resolves_to_reflect_dir():
    root = claude_adapter.find_plugin_root()
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
    # HOME must remain untouched
    assert not (tmp_path / ".claude").exists()


def test_install_writes_pointer_files_and_hook(tmp_path):
    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr

    skills_root = tmp_path / ".claude" / "skills"
    assert skills_root.is_dir()

    # recall + reflect must both land (they're the headline skills).
    recall = skills_root / "recall" / "SKILL.md"
    reflect = skills_root / "reflect" / "SKILL.md"
    assert recall.exists()
    assert reflect.exists()

    recall_body = recall.read_text(encoding="utf-8")
    assert claude_adapter.POINTER_MANAGED_BY in recall_body
    assert "source:" in recall_body
    # Name preserved from upstream frontmatter (reflect:recall).
    assert "name: reflect:recall" in recall_body

    # Hook merged into settings.json
    settings_text = (tmp_path / ".claude" / "settings.json").read_text()
    settings = json.loads(settings_text)
    commands = [
        h["command"]
        for entry in settings["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    expected = claude_adapter._render_session_start_hook_command(
        tmp_path / ".claude"
    )
    assert expected in commands

    # Critical: the {{HOME_TOOL_DIR}} placeholder MUST be substituted at
    # adapter-install time. If it survives into settings.json the recall
    # hook becomes a literal "uv run {{HOME_TOOL_DIR}}/..." command which
    # silently fails on every session start.
    assert "{{HOME_TOOL_DIR}}" not in settings_text
    assert "{{" not in settings_text
    # Body of the rendered command must reference the resolved Claude home.
    assert str(tmp_path / ".claude") in expected


def test_install_is_idempotent_and_preserves_existing_hooks(tmp_path):
    # Pre-seed settings with an unrelated existing hook — adapter must not
    # clobber it.
    claude_dir = tmp_path / ".claude"
    claude_dir.mkdir()
    existing = {
        "hooks": {
            "SessionStart": [
                {
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "echo existing"}],
                }
            ]
        }
    }
    (claude_dir / "settings.json").write_text(json.dumps(existing))

    for _ in range(2):  # Run twice; second run is a no-op
        subprocess.run(
            [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
            check=True, capture_output=True,
        )

    settings = json.loads((claude_dir / "settings.json").read_text())
    commands = [
        h["command"]
        for entry in settings["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    # Existing hook survived, adapter hook was added exactly once
    assert "echo existing" in commands
    expected = claude_adapter._render_session_start_hook_command(claude_dir)
    assert commands.count(expected) == 1


def test_install_no_hooks_flag_leaves_settings_alone(tmp_path):
    subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-hooks"],
        check=True, capture_output=True,
    )
    assert (tmp_path / ".claude" / "skills" / "recall" / "SKILL.md").exists()
    assert not (tmp_path / ".claude" / "settings.json").exists()


def test_uninstall_removes_only_managed_pointers(tmp_path):
    # First install, then drop an unmanaged user file into the same dir.
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )
    user_file = tmp_path / ".claude" / "skills" / "recall" / "user-note.md"
    user_file.write_text("hand-written", encoding="utf-8")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "uninstall", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr

    # Our pointer gone, user's file preserved
    assert not (tmp_path / ".claude" / "skills" / "recall" / "SKILL.md").exists()
    assert user_file.exists()

    # Hook block cleaned up (no reflect hook remaining)
    settings = json.loads((tmp_path / ".claude" / "settings.json").read_text())
    commands = [
        h["command"]
        for entry in settings.get("hooks", {}).get("SessionStart", [])
        for h in entry["hooks"]
    ]
    expected = claude_adapter._render_session_start_hook_command(
        tmp_path / ".claude"
    )
    assert expected not in commands


def test_install_refuses_to_overwrite_non_pointer_skill_marker(tmp_path):
    """A hand-written SKILL.md that lacks the managed_by sentinel must be
    LEFT ALONE. The previous behaviour silently replaced these files,
    which silently destroyed user state. Default install now refuses; the
    user needs ``--force`` to opt into clobbering."""
    claude_dir = tmp_path / ".claude"
    (claude_dir / "skills" / "recall").mkdir(parents=True)
    handwritten = "---\nname: user-handwritten\n---\nbody\n"
    target = claude_dir / "skills" / "recall" / "SKILL.md"
    target.write_text(handwritten, encoding="utf-8")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-hooks"],
        capture_output=True, text=True,
    )
    # Refusal exits non-zero so CI / scripts can detect it.
    assert result.returncode != 0, result.stdout
    assert "refused to overwrite non-pointer file" in result.stdout
    # Hand-written file untouched.
    assert target.read_text(encoding="utf-8") == handwritten


def test_install_force_replaces_non_pointer_skill_marker(tmp_path):
    """With ``--force`` the adapter explicitly replaces the foreign file,
    reports the replacement, and exits cleanly."""
    claude_dir = tmp_path / ".claude"
    (claude_dir / "skills" / "recall").mkdir(parents=True)
    target = claude_dir / "skills" / "recall" / "SKILL.md"
    target.write_text(
        "---\nname: user-handwritten\n---\nbody\n", encoding="utf-8"
    )

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install",
         "--home", str(tmp_path), "--no-hooks", "--force"],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    assert "replaced non-pointer file" in result.stdout
    body = target.read_text(encoding="utf-8")
    assert claude_adapter.POINTER_MANAGED_BY in body


def test_install_substitutes_home_tool_dir_placeholder(tmp_path):
    """Regression: the {{HOME_TOOL_DIR}} marker is a *bootstrap-time* template
    placeholder, but the adapter runs at install time. It must substitute the
    resolved Claude home itself rather than persisting the literal token."""
    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    settings_text = (tmp_path / ".claude" / "settings.json").read_text()
    assert "{{HOME_TOOL_DIR}}" not in settings_text
    assert "{{" not in settings_text  # No surviving curly templates of any kind

    settings = json.loads(settings_text)
    commands = [
        h["command"]
        for entry in settings["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    rendered = claude_adapter._render_session_start_hook_command(
        tmp_path / ".claude"
    )
    assert rendered in commands
    # The rendered command must point at the *resolved* claude home, not at
    # the user's actual ~/.claude — otherwise tests would taint real state.
    assert str(tmp_path / ".claude") in rendered


def test_install_cleans_up_legacy_unsubstituted_hook(tmp_path):
    """If a previous (buggy) install left a literal {{HOME_TOOL_DIR}} entry in
    settings.json, the adapter should remove it during the next install and
    replace it with the correctly-rendered command."""
    claude_dir = tmp_path / ".claude"
    claude_dir.mkdir()
    legacy = "uv run {{HOME_TOOL_DIR}}/skills/recall/hooks/session_start_recall.py"
    (claude_dir / "settings.json").write_text(json.dumps({
        "hooks": {
            "SessionStart": [
                {
                    "matcher": "",
                    "hooks": [{"type": "command", "command": legacy}],
                }
            ]
        }
    }))

    subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        check=True, capture_output=True,
    )

    settings_text = (claude_dir / "settings.json").read_text()
    assert "{{HOME_TOOL_DIR}}" not in settings_text
    settings = json.loads(settings_text)
    commands = [
        h["command"]
        for entry in settings["hooks"]["SessionStart"]
        for h in entry["hooks"]
    ]
    assert legacy not in commands
    rendered = claude_adapter._render_session_start_hook_command(claude_dir)
    assert commands.count(rendered) == 1


def test_install_errors_on_corrupt_settings_json(tmp_path):
    claude_dir = tmp_path / ".claude"
    claude_dir.mkdir()
    (claude_dir / "settings.json").write_text("{this is not json")

    result = subprocess.run(
        [sys.executable, str(ADAPTER), "install", "--home", str(tmp_path)],
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    # Error surfaces as an unhandled exception from _merge_session_start_hook;
    # users shouldn't lose their hand-edited settings.
    assert "settings.json" in result.stderr.lower()
