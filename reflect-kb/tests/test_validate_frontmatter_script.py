"""Tests for the standalone frontmatter validator (pre-commit entry point).

Ensures the script:
  1. passes the known-good v4 sample in tests/samples/
  2. rejects a sample missing a required field (confidence)
  3. rejects a file with no frontmatter block at all
  4. accepts a directory argument and recurses into it
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "validate_frontmatter.py"
SAMPLES = REPO_ROOT / "tests" / "samples"
GOOD = SAMPLES / "tokio-runtime-nested-panic.md"
BAD = SAMPLES / "invalid-missing-confidence.md"


def _run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
    )


@pytest.fixture(autouse=True)
def _guard_samples_exist():
    """Catches accidental sample removal before a misleading test failure."""
    assert GOOD.exists(), f"expected fixture missing: {GOOD}"
    assert BAD.exists(), f"expected fixture missing: {BAD}"


def test_valid_sample_passes():
    result = _run(str(GOOD))
    assert result.returncode == 0, result.stderr


def test_missing_required_field_fails():
    result = _run(str(BAD))
    assert result.returncode == 1
    # The JSON Schema error references the missing property name; accept
    # any mention so we don't over-fit to the exact jsonschema phrasing.
    assert "confidence" in result.stderr.lower()


def test_no_frontmatter_fails(tmp_path: Path):
    orphan = tmp_path / "no-frontmatter.md"
    orphan.write_text("# just a heading, no yaml block here\n", encoding="utf-8")
    result = _run(str(orphan))
    assert result.returncode == 1
    assert "no YAML frontmatter" in result.stderr


def test_directory_argument_recurses():
    # SAMPLES contains both GOOD and BAD — directory mode must see them both
    # and still fail because of BAD.
    result = _run(str(SAMPLES))
    assert result.returncode == 1
    assert str(BAD) in result.stderr
    # GOOD should not appear in the errors
    assert str(GOOD) not in result.stderr


def test_non_markdown_paths_are_ignored(tmp_path: Path):
    random = tmp_path / "not-a-learning.txt"
    random.write_text("ignore me", encoding="utf-8")
    result = _run(str(random))
    assert result.returncode == 0, result.stderr


def test_missing_schema_errors_with_code_2(tmp_path: Path):
    missing_schema = tmp_path / "does-not-exist.json"
    result = _run(str(GOOD), "--schema", str(missing_schema))
    assert result.returncode == 2
    assert "schema not found" in result.stderr
