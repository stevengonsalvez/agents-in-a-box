"""Tests for transcript distillation (~30x compression, no LLM)."""

from __future__ import annotations

import json

from reflect_kb.issues.distill import NOISE_TYPES, distill, distill_file, distill_text


def _line(obj: dict) -> str:
    return json.dumps(obj)


def _user(text: str, uuid: str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee") -> str:
    return _line(
        {
            "uuid": uuid,
            "timestamp": "2026-06-14T10:00:00.123Z",
            "message": {"role": "user", "content": text},
        }
    )


def _assistant_text(text: str) -> str:
    return _line(
        {
            "uuid": "11111111-2222-3333-4444-555555555555",
            "timestamp": "2026-06-14T10:01:00Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        }
    )


def _assistant_tool(name: str, inp: dict) -> str:
    return _line(
        {
            "message": {
                "role": "assistant",
                "content": [{"type": "tool_use", "name": name, "input": inp}],
            }
        }
    )


def _tool_error(payload: str) -> str:
    return _line(
        {
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "is_error": True, "content": payload}],
            }
        }
    )


def _tool_success(payload: str) -> str:
    return _line(
        {
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "is_error": False, "content": payload}],
            }
        }
    )


def test_keeps_user_assistant_tool_and_error_lines():
    lines = [
        _user("please fix the failing test"),
        _assistant_text("Looking into it now."),
        _assistant_tool("Bash", {"command": "cargo test " + "x" * 500}),
        _tool_error("thread 'main' panicked at lib.rs"),
    ]
    md, stats = distill(lines)

    assert "USER: please fix the failing test" in md
    assert "ASSIST: Looking into it now." in md
    assert "TOOL: Bash | cargo test" in md
    assert "ERROR: thread 'main' panicked" in md
    assert stats.kept_user == 1
    assert stats.kept_assist == 1
    assert stats.kept_tool_use == 1
    assert stats.kept_error == 1


def test_drops_noise_types_and_successful_tool_results():
    lines = [_line({"type": t}) for t in NOISE_TYPES]
    lines.append(_tool_success("ok, 42 files scanned, big output " + "z" * 2000))
    md, stats = distill(lines)

    assert stats.dropped_noise == len(NOISE_TYPES)
    # No USER/TOOL/ASSIST/ERROR rows survive — only the header.
    assert stats.kept_total == 0
    assert "big output" not in md


def test_heartbeat_and_skill_load_classification():
    lines = [
        _user("[HEARTBEAT] still alive"),
        _user("Base directory for this skill: /x/reflect\nmore"),
    ]
    md, stats = distill(lines)
    assert "HEARTBEAT" in md
    assert "SKILL_LOAD:" in md
    assert stats.kept_heartbeat == 1
    assert stats.kept_skill_load == 1


def test_bash_command_is_clipped():
    long_cmd = "echo " + "a" * 1000
    md, _ = distill([_assistant_tool("Bash", {"command": long_cmd})])
    # The clipped command must be far shorter than the raw 1000-char command.
    tool_line = [ln for ln in md.splitlines() if "TOOL: Bash" in ln][0]
    assert len(tool_line) < 300
    assert "…" in tool_line


def test_compression_ratio_is_reported_and_real():
    # Build a transcript dominated by successful tool-result noise.
    noise = "\n".join(_tool_success("x" * 3000) for _ in range(40))
    signal = "\n".join([_user("fix bug"), _assistant_text("done")])
    raw = noise + "\n" + signal
    md, stats = distill_text(raw)
    assert stats.src_bytes > stats.dst_bytes
    assert stats.compression > 5.0  # heavy noise -> big ratio


def test_distill_file_writes_output(tmp_path):
    src = tmp_path / "t.jsonl"
    src.write_text(_user("hello") + "\n" + _assistant_text("hi"), encoding="utf-8")
    dst = tmp_path / "out" / "t.md"
    stats = distill_file(src, dst)
    assert dst.exists()
    assert "USER: hello" in dst.read_text()
    assert stats.compression >= 0.0


def test_malformed_lines_are_skipped():
    lines = ["not json", "", _user("real line"), "{bad json"]
    md, stats = distill(lines)
    assert stats.kept_user == 1
    assert "USER: real line" in md
