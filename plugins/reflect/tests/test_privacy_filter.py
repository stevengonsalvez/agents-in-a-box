# ABOUTME: Regression tests for port M6 — privacy tag stripping at the LLM-prompt boundary.
# ABOUTME: Pins every M6 acceptance bullet: span removal, strip-list, fail-closed, cascade + hook wiring.
"""Port M6 (claude-mem `tag-stripping.ts`): <private> content must never reach
an LLM-bound payload — the cascade slice and the armed mini-learning payload."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
sys.path.insert(0, str(PLUGIN_ROOT / "scripts"))

from privacy_filter import PRIVATE_MARKER, STRIP_TAGS, strip_private  # noqa: E402
import reflect_cascade  # noqa: E402


# ---------- unit: the filter itself ----------

def test_private_span_removed_and_marked():
    out = strip_private("before <private>api key sk-123</private> after")
    assert "sk-123" not in out
    assert PRIVATE_MARKER in out
    assert out.startswith("before ") and out.endswith(" after")


def test_multiple_disjoint_spans():
    out = strip_private(
        "<private>one</private> keep <private>two</private> keep2"
    )
    assert "one" not in out and "two" not in out
    assert out.count(PRIVATE_MARKER) == 2
    assert "keep" in out and "keep2" in out


def test_unclosed_private_fails_closed():
    # No closing tag → strip to end of text, never leak.
    out = strip_private("intro <private>secret leaks to end of file")
    assert "secret" not in out
    assert out.startswith("intro ")


def test_case_insensitive_and_attributes():
    out = strip_private('x <PRIVATE reason="pii">secret</PRIVATE> y')
    assert "secret" not in out


def test_machine_wrapper_tags_removed_silently():
    for tag in STRIP_TAGS:
        if tag == "private":
            continue
        out = strip_private(f"a <{tag}>machine context</{tag}> b")
        assert "machine context" not in out, tag
        assert PRIVATE_MARKER not in out, tag  # silent removal, no marker


def test_plain_text_untouched():
    text = "no tags here, just a normal correction: never use foo"
    assert strip_private(text) == text


def test_empty_and_tagless_fast_path():
    assert strip_private("") == ""
    assert strip_private("a < b and c > d") == "a < b and c > d"


# ---------- integration: cascade slice (the drain LLM payload) ----------

def _write_transcript(path: Path, turns):
    with open(path, "w") as fh:
        for role, text in turns:
            fh.write(json.dumps({"message": {"role": role, "content": text}}) + "\n")
    return path


def test_cascade_slice_never_contains_private_content(tmp_path):
    transcript = _write_transcript(
        tmp_path / "t.jsonl",
        [
            ("user", "set up the db connection"),
            ("assistant", "using the connection string"),
            # correction signal (HIGH) with an embedded private span
            ("user",
             "no, never hardcode it — root cause was the env "
             "<private>password hunter2 at prod-db.internal</private> leaking"),
        ],
    )
    prep = reflect_cascade.prepare(transcript, out_path=str(tmp_path / "slice.txt"))
    assert prep.action == "reflect", prep.reason
    slice_text = Path(prep.slice_path).read_text()
    assert "hunter2" not in slice_text
    assert "prod-db.internal" not in slice_text
    assert PRIVATE_MARKER in slice_text
    # The correction itself survives — only the private span is elided.
    assert "never hardcode" in slice_text


# ---------- integration: armed mini-learning payload ----------

def test_armed_payload_strips_private(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    hook = PLUGIN_ROOT / "hooks" / "posttooluse_minilearning.py"
    import subprocess
    event = {
        "session_id": "sess-m6-test",
        "tool": "Bash",
        "tool_input": "psql <private>postgres://user:hunter2@prod</private> -c 'select 1'",
        "tool_response": {"exit_code": 1, "stderr": "auth failed <private>hunter2</private>"},
    }
    r = subprocess.run(
        [sys.executable, str(hook)],
        input=json.dumps(event), capture_output=True, text=True,
        env={**__import__("os").environ, "REFLECT_STATE_DIR": str(tmp_path)},
        timeout=20,
    )
    assert r.returncode == 0
    armed = tmp_path / "armed" / "sess-m6-test.json"
    assert armed.exists(), r.stderr
    payload = armed.read_text()
    assert "hunter2" not in payload
    assert "prod" not in payload or "postgres://" not in payload


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
