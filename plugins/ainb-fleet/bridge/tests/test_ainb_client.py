"""ainb client pure logic: cwd slug + JSONL end-of-turn reply extraction."""

from __future__ import annotations

import json

from ainb_phone_bridge.ainb_client import (
    _assistant_text_from_row,
    _scan_new_rows_for_turn_end,
    cwd_to_project_slug,
    send_keys,
    wait_for_reply,
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


def test_partial_line_is_not_consumed_then_completed(tmp_path):
    """A reply written as a partial (no newline yet) is captured once completed.

    Regression: previously the offset advanced to EOF on the first read, so the
    completed line was never re-read and the reply was silently lost.
    """
    path = tmp_path / "t.jsonl"
    row = {
        "type": "assistant",
        "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "answer"}]},
    }
    blob = json.dumps(row)
    # First write lands WITHOUT a trailing newline (a partial flush).
    with path.open("w", encoding="utf-8") as fh:
        fh.write(blob)

    offset, reply = _scan_new_rows_for_turn_end(path, 0)
    # Nothing consumed: no complete line yet, offset held at the start.
    assert reply is None
    assert offset == 0

    # The writer finishes the line (adds the newline).
    with path.open("a", encoding="utf-8") as fh:
        fh.write("\n")

    offset, reply = _scan_new_rows_for_turn_end(path, offset)
    assert reply == "answer"
    assert offset == path.stat().st_size


def test_scan_holds_offset_across_partial_then_next_turn(tmp_path):
    """After a completed line, a fresh partial line is still held for next poll."""
    path = tmp_path / "t.jsonl"
    done = {
        "type": "assistant",
        "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "first"}]},
    }
    with path.open("w", encoding="utf-8") as fh:
        fh.write(json.dumps(done) + "\n")
        fh.write('{"partial": tr')  # half-written next line, no newline

    offset, reply = _scan_new_rows_for_turn_end(path, 0)
    assert reply == "first"
    # Offset stops at the newline, leaving the partial for the next read.
    assert offset == len(json.dumps(done)) + 1


# --- send_keys terminates flags with `--` ---------------------------------


def test_send_keys_leading_dash_payload(monkeypatch):
    """A payload starting with '-' must not be parsed as a tmux flag."""
    calls: list[list[str]] = []

    class R:
        returncode = 0

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return R()

    import ainb_phone_bridge.ainb_client as client

    monkeypatch.setattr(client.subprocess, "run", fake_run)

    payload = "-rf important note"
    assert send_keys("sess", payload) is True

    literal_call = calls[0]
    # The literal send must place `--` immediately before the payload.
    assert literal_call == ["tmux", "send-keys", "-t", "sess", "-l", "--", payload]


# --- wait_for_reply follows transcript rotation ---------------------------


def _end_turn_blob(text: str) -> str:
    return (
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": text}],
                },
            }
        )
        + "\n"
    )


def test_wait_for_reply_follows_rotated_transcript(tmp_path, monkeypatch):
    """End-of-turn lands in a NEW *.jsonl after rotation; it must still be read.

    Regression: wait_for_reply only re-resolved the transcript while path was
    None, so a rotation (resume / compaction) left the reply in a file it never
    read -> false timeout.
    """
    import ainb_phone_bridge.ainb_client as client

    old = tmp_path / "old.jsonl"
    new = tmp_path / "new.jsonl"
    # The original transcript exists but never gets the end-of-turn row.
    old.write_text("", encoding="utf-8")

    # Drive resolution: first poll -> old file, then a rotation to the new file
    # which holds the reply.
    resolutions = iter([old, new, new])

    def fake_latest(_cwd):
        try:
            return next(resolutions)
        except StopIteration:
            return new

    monkeypatch.setattr(client, "latest_transcript_for_cwd", fake_latest)
    monkeypatch.setattr(client.time, "sleep", lambda _s: new.write_text(_end_turn_blob("rotated")))

    reply = wait_for_reply("/cwd", start_offset=0, transcript=old, timeout=5.0, poll_interval=0.01)
    assert reply == "rotated"
