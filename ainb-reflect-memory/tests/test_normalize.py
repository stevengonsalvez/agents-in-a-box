# ABOUTME: Unit tests for content normalization + dedupe hashing.
# ABOUTME: No database required — pins the client-side idempotency contract.

from __future__ import annotations

import pytest

from ainb_reflect_memory.normalize import content_hash, normalize_content


def test_normalize_collapses_whitespace_case_and_unicode() -> None:
    # Surrounding + internal whitespace runs, mixed case, and stray newlines
    # all fold away; "  Fixed   the BUG\n" and "fixed the bug" must collapse.
    assert normalize_content("  Fixed   the BUG\n") == "fixed the bug"
    assert normalize_content("fixed the bug") == "fixed the bug"
    assert normalize_content("a\t\tb\nc") == "a b c"


def test_normalize_is_idempotent() -> None:
    once = normalize_content("  Hello   World  ")
    assert normalize_content(once) == once


def test_normalize_preserves_genuinely_different_text() -> None:
    assert normalize_content("fix the bug") != normalize_content("fix the test")


def test_normalize_rejects_non_string() -> None:
    with pytest.raises(TypeError):
        normalize_content(123)  # type: ignore[arg-type]


def test_content_hash_is_stable_and_hex_sha256() -> None:
    h = content_hash("the auth token expiry uses a strict less-than check")
    # SHA-256 hex digest is always 64 lowercase hex chars.
    assert len(h) == 64
    assert all(c in "0123456789abcdef" for c in h)
    # Stable across calls.
    assert h == content_hash("the auth token expiry uses a strict less-than check")


def test_content_hash_collapses_equivalent_inputs() -> None:
    # Same normalized content => same hash => the (workspace_id, content_hash)
    # unique constraint dedupes them to one row.
    assert content_hash("  Fixed THE bug\n") == content_hash("fixed the bug")


def test_content_hash_differs_for_different_content() -> None:
    assert content_hash("fix the bug") != content_hash("fix the test")
