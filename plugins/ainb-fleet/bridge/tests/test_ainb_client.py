"""ainb client pure logic: cwd slug + JSONL end-of-turn reply extraction."""

from __future__ import annotations

import json

from ainb_phone_bridge.ainb_client import (
    _assistant_text_from_row,
    _scan_new_rows_for_turn_end,
    cwd_to_project_slug,
)

# --- cwd_to_project_slug (must match the Rust fleet slug) ------------------


def test_slug_basic_path():
    assert cwd_to_project_slug("/Users/foo/d/git/bar") == "-Users-foo-d-git-bar"


def test_slug_collapses_dot_and_underscore():
    assert (
        cwd_to_project_slug("/Users/foo/.agents-in-a-box/foo_bar")
        == "-Users-foo--agents-in-a-box-foo-bar"
    )


def test_slug_preserves_existing_dashes():
    assert cwd_to_project_slug("a-b-c") == "a-b-c"


# --- assistant row text extraction ----------------------------------------


def test_assistant_text_from_row():
    row = {
        "type": "assistant",
        "message": {
            "stop_reason": "end_turn",
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "text", "text": "world"},
            ],
        },
    }
    stop, text = _assistant_text_from_row(row)
    assert stop == "end_turn"
    assert text == "Hello\nworld"


def test_non_assistant_row_ignored():
    assert _assistant_text_from_row({"type": "user", "message": {}}) == (None, None)


def test_tool_use_only_row_has_no_text():
    row = {
        "type": "assistant",
        "message": {
            "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "name": "Bash"}],
        },
    }
    stop, text = _assistant_text_from_row(row)
    assert stop == "tool_use"
    assert text is None


def test_string_content():
    row = {"type": "assistant", "message": {"stop_reason": "end_turn", "content": "hi"}}
    assert _assistant_text_from_row(row) == ("end_turn", "hi")


# --- scan for the last end-of-turn reply ----------------------------------


def _write_jsonl(path, rows):
    with path.open("w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    return path.stat().st_size


def test_scan_returns_last_end_turn(tmp_path):
    path = tmp_path / "t.jsonl"
    rows = [
        {"type": "user", "message": {"content": "go"}},
        {
            "type": "assistant",
            "message": {
                "stop_reason": "tool_use",
                "content": [{"type": "text", "text": "thinking"}],
            },
        },
        {
            "type": "assistant",
            "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "done!"}]},
        },
    ]
    _write_jsonl(path, rows)
    offset, reply = _scan_new_rows_for_turn_end(path, 0)
    assert reply == "done!"
    assert offset == path.stat().st_size


def test_scan_only_reads_after_offset(tmp_path):
    path = tmp_path / "t.jsonl"
    first_size = _write_jsonl(
        path,
        [
            {
                "type": "assistant",
                "message": {
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": "old"}],
                },
            }
        ],
    )
    # Append a new turn after the watermark.
    with path.open("a", encoding="utf-8") as fh:
        fh.write(
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": "fresh"}],
                    },
                }
            )
            + "\n"
        )
    offset, reply = _scan_new_rows_for_turn_end(path, first_size)
    assert reply == "fresh"


def test_scan_no_end_turn_yet(tmp_path):
    path = tmp_path / "t.jsonl"
    _write_jsonl(
        path,
        [
            {
                "type": "assistant",
                "message": {
                    "stop_reason": "tool_use",
                    "content": [{"type": "text", "text": "wip"}],
                },
            }
        ],
    )
    _offset, reply = _scan_new_rows_for_turn_end(path, 0)
    assert reply is None


def test_scan_tolerates_malformed_lines(tmp_path):
    path = tmp_path / "t.jsonl"
    with path.open("w", encoding="utf-8") as fh:
        fh.write("not json\n")
        fh.write(
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": "ok"}],
                    },
                }
            )
            + "\n"
        )
    _offset, reply = _scan_new_rows_for_turn_end(path, 0)
    assert reply == "ok"
