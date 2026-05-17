"""Validate the v4 frontmatter JSON Schema against known-good and known-bad samples."""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest
import yaml
from jsonschema import Draft202012Validator, FormatChecker

REPO_ROOT = Path(__file__).resolve().parents[1]
SCHEMA_PATH = REPO_ROOT / "schemas" / "frontmatter.schema.json"
SAMPLES_DIR = Path(__file__).parent / "samples"

FRONTMATTER_RE = re.compile(r"^---\s*\n(.*?)\n---\s*\n", re.DOTALL)


def _extract_frontmatter(md_path: Path) -> dict:
    """Pull the YAML frontmatter block out of a markdown file."""
    text = md_path.read_text(encoding="utf-8")
    match = FRONTMATTER_RE.match(text)
    if not match:
        raise ValueError(f"No YAML frontmatter found in {md_path}")
    return yaml.safe_load(match.group(1))


@pytest.fixture(scope="module")
def validator() -> Draft202012Validator:
    schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    Draft202012Validator.check_schema(schema)
    return Draft202012Validator(schema, format_checker=FormatChecker())


def test_schema_is_draft_2020_12(validator: Draft202012Validator) -> None:
    assert validator.META_SCHEMA["$id"] == "https://json-schema.org/draft/2020-12/schema"


def test_known_good_sample_validates(validator: Draft202012Validator) -> None:
    """Canonical v4-shape sample from the spec must validate cleanly."""
    frontmatter = _extract_frontmatter(SAMPLES_DIR / "tokio-runtime-nested-panic.md")
    errors = sorted(validator.iter_errors(frontmatter), key=lambda e: e.path)
    assert errors == [], f"unexpected validation errors: {[e.message for e in errors]}"


def test_required_fields_enforced(validator: Draft202012Validator) -> None:
    """title, created, confidence are required by the v4 spec."""
    minimal = {"title": "x", "created": "2026-04-24", "confidence": "high"}
    assert validator.is_valid(minimal)

    for missing in ("title", "created", "confidence"):
        bad = dict(minimal)
        bad.pop(missing)
        assert not validator.is_valid(bad), f"schema allowed missing {missing}"


def test_confidence_enum_is_lowercase_only(validator: Draft202012Validator) -> None:
    """Spec pins confidence to {high, medium, low} — uppercase must reject."""
    for good in ("high", "medium", "low"):
        assert validator.is_valid(
            {"title": "x", "created": "2026-04-24", "confidence": good}
        )
    for bad in ("HIGH", "Medium", "unknown", ""):
        assert not validator.is_valid(
            {"title": "x", "created": "2026-04-24", "confidence": bad}
        ), f"schema allowed confidence={bad!r}"


def test_created_must_be_iso_date(validator: Draft202012Validator) -> None:
    assert not validator.is_valid(
        {"title": "x", "created": "yesterday", "confidence": "high"}
    )


def test_additional_properties_allowed(validator: Draft202012Validator) -> None:
    """Real-world frontmatter carries provenance / ingest metadata; keep it permissive."""
    frontmatter = {
        "title": "x",
        "created": "2026-04-24",
        "confidence": "high",
        "provenance": {"source_tool": "claude", "content_hash": "abc123"},
        "source_project": "biolift",
    }
    assert validator.is_valid(frontmatter)
