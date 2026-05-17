"""Tests for the Copilot adapter (plugins/reflect/adapters/copilot)."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
ADAPTER_DIR = HERE.parent / "copilot"
ADAPTER = ADAPTER_DIR / "copilot_adapter.py"

sys.path.insert(0, str(ADAPTER_DIR))

import copilot_adapter  # noqa: E402


@pytest.fixture(autouse=True)
def _sanity():
    assert ADAPTER.exists(), f"missing adapter script at {ADAPTER}"


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
    ~/.copilot/skills/. Copilot has no hook system to test the
    "preserve existing hooks" half (Claude does), so we cover the
    sibling-file half: a hand-written file inside an *adapter-managed*
    skill dir must survive multiple install cycles."""
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
