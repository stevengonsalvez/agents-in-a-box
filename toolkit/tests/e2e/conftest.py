"""E2E layer conftest: isolation fixtures."""

import os
import subprocess
import pytest
from pathlib import Path
from unittest.mock import MagicMock


@pytest.fixture
def full_learnings_home(tmp_path):
    """A fully initialized LEARNINGS_HOME with git repo."""
    home = tmp_path / "learnings"
    (home / "documents" / "learnings").mkdir(parents=True)
    (home / "documents" / "episodes").mkdir(parents=True)
    (home / "documents" / "clusters").mkdir(parents=True)
    (home / "nano_graphrag_cache").mkdir(parents=True)

    # Init git
    subprocess.run(["git", "init", "--quiet"], cwd=str(home), check=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=str(home), check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=str(home), check=True)

    gitignore = home / ".gitignore"
    gitignore.write_text("nano_graphrag_cache/\n.venv/\n__pycache__/\n*.pyc\n")
    subprocess.run(["git", "add", "."], cwd=str(home), check=True)
    subprocess.run(["git", "commit", "-m", "init", "--quiet"], cwd=str(home), check=True)

    old_env = os.environ.get("LEARNINGS_HOME")
    os.environ["LEARNINGS_HOME"] = str(home)
    yield home
    if old_env is not None:
        os.environ["LEARNINGS_HOME"] = old_env
    else:
        os.environ.pop("LEARNINGS_HOME", None)


@pytest.fixture
def isolated_env(tmp_path, monkeypatch):
    """Full isolation: redirect HOME, CLAUDE_PROJECT_DIR, REFLECT_STATE_DIR, LEARNINGS_HOME.

    All filesystem writes go to tmp_path — zero pollution of real ~/ or project.
    Returns a dict with all the paths for assertions.
    """
    fake_home = tmp_path / "home"
    project_dir = tmp_path / "project"
    state_dir = tmp_path / "state"
    learnings_home = tmp_path / "learnings"

    # Create directory structures
    fake_home.mkdir()
    project_dir.mkdir()
    (project_dir / ".git").mkdir()  # Fake git repo marker
    state_dir.mkdir()
    (learnings_home / "documents" / "learnings").mkdir(parents=True)
    (learnings_home / "documents" / "episodes").mkdir(parents=True)
    (learnings_home / "documents" / "clusters").mkdir(parents=True)
    (learnings_home / "nano_graphrag_cache").mkdir(parents=True)

    # Init git in learnings home
    subprocess.run(["git", "init", "--quiet"], cwd=str(learnings_home), check=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=str(learnings_home), check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=str(learnings_home), check=True)
    gitignore = learnings_home / ".gitignore"
    gitignore.write_text("nano_graphrag_cache/\n")
    subprocess.run(["git", "add", "."], cwd=str(learnings_home), check=True)
    subprocess.run(["git", "commit", "-m", "init", "--quiet"], cwd=str(learnings_home), check=True)

    # Set env vars
    monkeypatch.setenv("HOME", str(fake_home))
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(project_dir))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(state_dir))
    monkeypatch.setenv("LEARNINGS_HOME", str(learnings_home))

    return {
        "home": fake_home,
        "project_dir": project_dir,
        "state_dir": state_dir,
        "learnings_home": learnings_home,
        "tmp_path": tmp_path,
    }


@pytest.fixture
def mock_graph_engine():
    """Mock graph engine for E2E tests that don't need real GraphRAG."""
    engine = MagicMock()
    engine.insert_document = MagicMock()
    engine.search = MagicMock(return_value="")
    engine.get_stats = MagicMock(return_value={
        "cache_exists": True,
        "entity_count": 0,
        "relationship_count": 0,
    })
    return engine
