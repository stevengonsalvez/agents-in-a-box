"""Tests for the cross-harness stdin readers (scripts/hook_input.py).

The headline property: snake_case (claude/codex) keys win when present,
camelCase (copilot) keys only fill in when the snake_case key is *absent*,
and presence — not truthiness — decides, so a meaningful falsy value (an
empty ``{}`` tool result) is honoured instead of skipped.
"""

from __future__ import annotations

import sys
from pathlib import Path

# scripts/tests/<this> → scripts/ holds hook_input.py
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import hook_input  # noqa: E402


# --- session id ----------------------------------------------------------

def test_session_id_prefers_snake_case():
    data = {"session_id": "snake", "sessionId": "camel"}
    assert hook_input.get_session_id(data) == "snake"


def test_session_id_falls_back_to_camel_case():
    assert hook_input.get_session_id({"sessionId": "camel"}) == "camel"


def test_session_id_default_when_absent():
    assert hook_input.get_session_id({}) == ""
    assert hook_input.get_session_id({}, "fallback") == "fallback"


# --- transcript path -----------------------------------------------------

def test_transcript_path_snake_then_camel():
    assert hook_input.get_transcript_path({"transcript_path": "/a"}) == "/a"
    assert hook_input.get_transcript_path({"transcriptPath": "/b"}) == "/b"
    assert hook_input.get_transcript_path(
        {"transcript_path": "/a", "transcriptPath": "/b"}
    ) == "/a"


# --- tool name -----------------------------------------------------------

def test_tool_name_legacy_order_then_camel():
    # Legacy: `tool` beats `tool_name` (preserves prior inline behaviour).
    assert hook_input.get_tool_name({"tool": "Bash", "tool_name": "x"}) == "Bash"
    assert hook_input.get_tool_name({"tool_name": "Edit"}) == "Edit"
    assert hook_input.get_tool_name({"toolName": "Write"}) == "Write"


# --- tool response (presence-first is load-bearing here) -----------------

def test_tool_response_empty_dict_is_honored_not_skipped():
    """An empty {} under the snake_case key is a real value — presence,
    not truthiness, must win so we don't fall through to a camelCase key."""
    data = {"tool_response": {}, "toolResult": {"exitCode": 1}}
    # snake_case present (even though falsy {}) → it wins.
    assert hook_input.get_tool_response(data) == {}


def test_tool_response_camel_fallback():
    assert hook_input.get_tool_response({"toolResult": {"exitCode": 2}}) == {"exitCode": 2}


def test_tool_response_legacy_response_key():
    assert hook_input.get_tool_response({"response": {"ok": True}}) == {"ok": True}


def test_tool_response_default_is_empty_dict():
    assert hook_input.get_tool_response({}) == {}


# --- cwd -----------------------------------------------------------------

def test_cwd_shared_key():
    assert hook_input.get_cwd({"cwd": "/work"}) == "/work"
    assert hook_input.get_cwd({}, "/fallback") == "/fallback"


# --- non-mapping input ---------------------------------------------------

def test_non_mapping_returns_default():
    assert hook_input.get_session_id([], "d") == "d"  # type: ignore[arg-type]
    assert hook_input.get_session_id(None, "d") == "d"  # type: ignore[arg-type]
