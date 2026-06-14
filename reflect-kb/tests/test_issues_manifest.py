"""Tests for sourcing recent transcripts from the existing reflect queue."""

from __future__ import annotations

import json

from reflect_kb.issues.manifest import gather_transcripts


def _write_queue(tmp_path, entries):
    qf = tmp_path / "pending_reflections.jsonl"
    with open(qf, "w", encoding="utf-8") as fh:
        for e in entries:
            fh.write(json.dumps(e) + "\n")
    return qf


def _touch(tmp_path, name):
    p = tmp_path / name
    p.write_text("{}\n", encoding="utf-8")
    return p


def test_empty_or_missing_queue_returns_empty(tmp_path):
    assert gather_transcripts(queue=tmp_path / "nope.jsonl") == []


def test_reads_existing_transcripts_only(tmp_path):
    present = _touch(tmp_path, "present.jsonl")
    qf = _write_queue(
        tmp_path,
        [
            {"session_id": "s1", "transcript_path": str(present), "trigger": "stop"},
            {
                "session_id": "s2",
                "transcript_path": str(tmp_path / "gone.jsonl"),
                "trigger": "stop",
            },
        ],
    )
    refs = gather_transcripts(queue=qf)
    assert [r.session_id for r in refs] == ["s1"]
    assert refs[0].transcript_path == present.resolve()


def test_dedupes_by_resolved_path_keeping_latest(tmp_path):
    present = _touch(tmp_path, "p.jsonl")
    qf = _write_queue(
        tmp_path,
        [
            {"session_id": "first", "transcript_path": str(present), "trigger": "precompact"},
            {"session_id": "second", "transcript_path": str(present), "trigger": "stop"},
        ],
    )
    refs = gather_transcripts(queue=qf)
    assert len(refs) == 1
    assert refs[0].session_id == "second"  # latest enqueue wins


def test_limit_keeps_most_recent(tmp_path):
    paths = [_touch(tmp_path, f"t{i}.jsonl") for i in range(5)]
    qf = _write_queue(
        tmp_path,
        [
            {"session_id": f"s{i}", "transcript_path": str(p), "trigger": "stop"}
            for i, p in enumerate(paths)
        ],
    )
    refs = gather_transcripts(queue=qf, limit=2)
    assert [r.session_id for r in refs] == ["s3", "s4"]


def test_malformed_lines_skipped(tmp_path):
    present = _touch(tmp_path, "ok.jsonl")
    qf = tmp_path / "pending_reflections.jsonl"
    qf.write_text(
        "not json\n"
        + json.dumps({"session_id": "ok", "transcript_path": str(present), "trigger": "stop"})
        + "\n{bad\n",
        encoding="utf-8",
    )
    refs = gather_transcripts(queue=qf)
    assert [r.session_id for r in refs] == ["ok"]
