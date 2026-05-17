"""Tests for the shared adapter base (:mod:`base`).

Most adapter behaviour is exercised end-to-end through the per-harness
test suites; these tests pin down the lower-level helpers that previously
contained subtle bugs (frontmatter parsing in particular).
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
ADAPTERS_DIR = HERE.parent
sys.path.insert(0, str(ADAPTERS_DIR))

import base  # noqa: E402


# --- parse_skill_frontmatter --------------------------------------------


def test_parse_returns_empty_dict_for_no_frontmatter():
    assert base.parse_skill_frontmatter("body only, no fm\n") == {}
    assert base.parse_skill_frontmatter("") == {}


def test_parse_simple_single_line_keys():
    text = "---\nname: foo\ndescription: bar\n---\nbody\n"
    fm = base.parse_skill_frontmatter(text)
    assert fm == {"name": "foo", "description": "bar"}


def test_parse_handles_multiline_block_description():
    """Regression: the previous parser silently dropped multi-line
    ``description: |`` blocks because it required the value on the same
    line as the key. ``yaml.safe_load`` handles them correctly."""
    text = (
        "---\n"
        "name: foo\n"
        "description: |\n"
        "  Line one of the description.\n"
        "  Line two of the description.\n"
        "---\n"
        "body\n"
    )
    fm = base.parse_skill_frontmatter(text)
    assert fm["name"] == "foo"
    desc = fm["description"]
    assert "Line one of the description." in desc
    assert "Line two of the description." in desc


def test_parse_handles_body_horizontal_rule():
    """Regression: the previous parser used ``text.split('---', 2)`` which
    misinterpreted any body-level ``---`` rule as the end of frontmatter
    and silently truncated it. Real frontmatter terminates at a
    line-anchored ``---``, so a body rule must be left alone."""
    text = (
        "---\n"
        "name: foo\n"
        "description: bar\n"
        "---\n"
        "Some prose.\n"
        "\n"
        "---\n"  # Markdown horizontal rule in body — must not confuse parser
        "\n"
        "More prose.\n"
    )
    fm = base.parse_skill_frontmatter(text)
    assert fm == {"name": "foo", "description": "bar"}


def test_parse_returns_empty_on_unterminated_frontmatter():
    """A document that opens with ``---`` but never closes is malformed.
    The parser must not raise; it returns ``{}`` and lets the adapter
    fall back to default name/description."""
    text = "---\nname: foo\ndescription: bar\n"  # no closing ---
    assert base.parse_skill_frontmatter(text) == {}


def test_parse_returns_empty_on_invalid_yaml():
    """Malformed YAML inside the frontmatter must not crash the install."""
    text = "---\nname: : :\n  - [unterminated\n---\nbody\n"
    assert base.parse_skill_frontmatter(text) == {}


def test_parse_handles_quoted_values_with_colons():
    """Description containing ``:`` (e.g. ``Foo: bar baz``) must round-trip
    through YAML quoting rather than getting truncated."""
    text = (
        "---\n"
        "name: foo\n"
        'description: "When to use: things"\n'
        "---\n"
    )
    fm = base.parse_skill_frontmatter(text)
    assert fm["description"] == "When to use: things"


# --- AdapterBase pointer body uses parser correctly ---------------------


class _DummyAdapter(base.AdapterBase):
    POINTER_MANAGED_BY = "test/dummy"
    HARNESS_DIR = ".dummy"
    HARNESS_LABEL = "Dummy"


def test_pointer_body_collapses_multiline_description(tmp_path):
    """The pointer's frontmatter is single-line YAML; multi-line upstream
    descriptions must be flattened so the generated file stays valid
    one-line YAML and remains greppable."""
    src_dir = tmp_path / "skills" / "thing"
    src_dir.mkdir(parents=True)
    src = src_dir / "SKILL.md"
    src.write_text(
        "---\n"
        "name: thing\n"
        "description: |\n"
        "  First sentence.\n"
        "  Second sentence.\n"
        "---\n"
        "body\n"
    )

    adapter = _DummyAdapter(__file__)
    body = adapter._pointer_body(src)

    # Single line for description, both sentences captured, no embedded \n.
    desc_line = next(
        line for line in body.splitlines() if line.startswith("description:")
    )
    assert "First sentence." in desc_line
    assert "Second sentence." in desc_line
    assert "\n" not in desc_line  # collapsed


def test_pointer_body_falls_back_when_frontmatter_invalid(tmp_path):
    src_dir = tmp_path / "skills" / "thing"
    src_dir.mkdir(parents=True)
    src = src_dir / "SKILL.md"
    src.write_text("no frontmatter here\n")

    adapter = _DummyAdapter(__file__)
    body = adapter._pointer_body(src)
    assert "name: thing" in body  # falls back to directory name
    assert "description: Installed by reflect-kb adapter" in body


def test_pointer_body_handles_body_horizontal_rule(tmp_path):
    """Regression for the old split('---', 2) parser: a body-level ``---``
    rule used to truncate frontmatter parsing. The pointer body must
    still surface the correct upstream description."""
    src_dir = tmp_path / "skills" / "thing"
    src_dir.mkdir(parents=True)
    src = src_dir / "SKILL.md"
    src.write_text(
        "---\n"
        "name: thing\n"
        "description: real-description\n"
        "---\n"
        "Heading\n"
        "\n"
        "---\n"  # Markdown rule in body
        "\n"
        "More content.\n"
    )

    adapter = _DummyAdapter(__file__)
    body = adapter._pointer_body(src)
    assert "description: real-description" in body
