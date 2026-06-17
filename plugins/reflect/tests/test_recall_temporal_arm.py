# ABOUTME: Regression tests for port R5 — the temporal retrieval arm. Pins
# ABOUTME: the two acceptance bullets: (1) date-free queries get ZERO hits
# ABOUTME: from the arm (no false boost), (2) the arm integrates into RRF
# ABOUTME: cleanly — plus window filtering, date coalescing, and the wiring.
"""Port R5: temporal retrieval arm (Hindsight retrieve_temporal_combined).

A 4th parallel arm that scans the local learnings corpus for notes whose
coalesced timestamp falls inside the R6-parsed date window and feeds them
into the RRF fusion alongside the vector/BM25/graph arms.

Acceptance bullets pinned here:
  1. arm returns 0 hits on date-free queries (no false boost)
  2. integrates into RRF cleanly
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from datetime import datetime, timedelta
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
RECALL = RECALL_SCRIPTS / "recall.py"
sys.path.insert(0, str(RECALL_SCRIPTS))

import recall as recall_mod  # noqa: E402
from recall import (  # noqa: E402
    Learning,
    fetch_temporal,
    learning_timestamp,
    rrf_fuse,
)
from temporal_extraction import TemporalRange, extract_temporal_constraint  # noqa: E402

# Wednesday 2026-06-10 — fixed reference so relative phrases are deterministic.
REF = datetime(2026, 6, 10, 12, 0, 0)


def _write_learning(
    root: Path, name: str, created: str, body: str = "body text"
) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    path = root / f"{name}.md"
    path.write_text(
        f"---\nid: {name}\nconfidence: high\ncreated: {created}\n---\n{body}\n"
    )
    return path


def _range(start: datetime, end: datetime, text: str = "last week") -> TemporalRange:
    return TemporalRange(start=start, end=end, confidence=0.9, matched_text=text)


@pytest.fixture()
def corpus(tmp_path, monkeypatch):
    """A tiny learnings corpus with one in-window and one out-of-window note,
    wired in as the arm's scan root."""
    root = tmp_path / "documents"
    _write_learning(root, "in-window", "2026-06-03T10:00:00Z",
                    "redis pool exhaustion fix")
    _write_learning(root, "out-of-window", "2025-01-01T10:00:00Z",
                    "ancient redis note")
    monkeypatch.setattr(recall_mod, "QMD_DOCS_ROOT", root)
    monkeypatch.delenv("GLOBAL_LEARNINGS_PATH", raising=False)
    return root


# Window covering 2026-06-01..2026-06-07 ("last week" relative to REF).
WINDOW = _range(datetime(2026, 6, 1), datetime(2026, 6, 7, 23, 59, 59))


# ---------- acceptance bullet 1: 0 hits on date-free queries ----------

def test_date_free_query_extracts_no_range():
    assert extract_temporal_constraint("redis pool exhaustion", REF) is None


def test_arm_returns_empty_without_temporal_signal(corpus):
    # None range == date-free query: the arm contributes NOTHING even though
    # the corpus has dated learnings sitting right there.
    assert fetch_temporal(None, 10, "redis pool exhaustion") == []


def test_arm_disabled_by_env_flag(corpus, monkeypatch):
    monkeypatch.setattr(recall_mod, "TEMPORAL_ARM_ENABLED", False)
    assert fetch_temporal(WINDOW, 10, "redis") == []


def test_arm_returns_empty_when_corpus_missing(tmp_path, monkeypatch):
    monkeypatch.setattr(recall_mod, "QMD_DOCS_ROOT", tmp_path / "nope")
    assert fetch_temporal(WINDOW, 10, "redis") == []


def test_arm_empty_on_nonpositive_limit(corpus):
    assert fetch_temporal(WINDOW, 0, "redis") == []


def test_arm_honours_global_learnings_path_override(corpus, tmp_path, monkeypatch):
    # Isolation contract: a sandboxed KB (eval harness) redirects the scan
    # root via GLOBAL_LEARNINGS_PATH — the "live" corpus must NOT leak in.
    sandbox = tmp_path / "sandbox-kb"
    _write_learning(sandbox / "documents", "sandbox-note",
                    "2026-06-04T10:00:00Z", "redis sandbox")
    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(sandbox))
    ids = [lrn.id for lrn in fetch_temporal(WINDOW, 10, "redis last week")]
    assert ids == ["sandbox-note"]  # live corpus ("in-window") not scanned


# ---------- window filtering & date coalescing ----------

def test_arm_filters_by_window(corpus):
    hits = fetch_temporal(WINDOW, 10, "redis pool exhaustion last week")
    ids = [lrn.id for lrn in hits]
    assert ids == ["in-window"]  # out-of-window note excluded


def test_arm_respects_limit(corpus):
    for i in range(5):
        _write_learning(corpus, f"extra-{i}", "2026-06-04T10:00:00Z", "redis")
    hits = fetch_temporal(WINDOW, 3, "redis last week")
    assert len(hits) == 3


def test_undatable_learning_is_invisible(corpus):
    (corpus / "no-date.md").write_text("---\nid: no-date\n---\nredis stuff\n")
    hits = fetch_temporal(WINDOW, 10, "redis last week")
    assert "no-date" not in [lrn.id for lrn in hits]


def test_timestamp_coalescing_order():
    # frontmatter archived wins over updated_at/created; bare yaml dates and
    # tz-aware datetimes both coerce to naive datetimes.
    lrn = Learning(
        chunk_text="x",
        frontmatter={
            "archived": "2026-06-02T08:00:00Z",
            "updated_at": "2026-05-01T00:00:00",
            "created": "2026-01-01T00:00:00",
        },
    )
    assert learning_timestamp(lrn) == datetime(2026, 6, 2, 8, 0, 0)

    lrn2 = Learning(chunk_text="x", frontmatter={"updated_at": "2026-06-03"})
    assert learning_timestamp(lrn2) == datetime(2026, 6, 3)

    # body <!-- archived --> header is the last fallback (R8's recency field)
    lrn3 = Learning(chunk_text="x", frontmatter={}, archived_at="2026-06-04T09:30:00")
    assert learning_timestamp(lrn3) == datetime(2026, 6, 4, 9, 30)

    assert learning_timestamp(Learning(chunk_text="x", frontmatter={})) is None
    assert learning_timestamp(
        Learning(chunk_text="x", frontmatter={"created": "not-a-date"})
    ) is None


def test_yaml_parsed_datetime_objects_accepted():
    # yaml.safe_load hands back real datetime/date objects for unquoted ISO
    # values — the coalescer must take them as-is, tz dropped.
    import yaml

    fm = yaml.safe_load("created: 2026-06-03T10:00:00Z\n")
    lrn = Learning(chunk_text="x", frontmatter=fm)
    assert learning_timestamp(lrn) == datetime(2026, 6, 3, 10, 0, 0)


def test_arm_ranks_topical_overlap_first(corpus):
    # Two in-window notes: the one matching the (date-stripped) query terms
    # must outrank the unrelated one — Hindsight's similarity-first pool.
    _write_learning(corpus, "unrelated", "2026-06-04T10:00:00Z",
                    "kubernetes ingress timeout")
    hits = fetch_temporal(WINDOW, 10, "redis pool exhaustion last week")
    ids = [lrn.id for lrn in hits]
    assert ids[0] == "in-window"
    assert "unrelated" in ids  # still in-window, just ranked below


# ---------- acceptance bullet 2: integrates into RRF cleanly ----------

def _mk(name: str) -> Learning:
    return Learning(chunk_text=f"body {name}", frontmatter={"id": name})


def test_empty_temporal_arm_is_rrf_noop():
    vector = [_mk("a"), _mk("b")]
    qmd = [_mk("b"), _mk("c")]
    without = rrf_fuse([vector, qmd, []])
    with_empty_temporal = rrf_fuse([vector, qmd, [], []])
    assert [l.id for l in without] == [l.id for l in with_empty_temporal]


def test_temporal_hits_fuse_and_dedup():
    vector = [_mk("a"), _mk("b")]
    temporal = [_mk("b"), _mk("t")]
    fused = rrf_fuse([vector, [], [], temporal])
    ids = [l.id for l in fused]
    assert ids.count("b") == 1  # dedup by learning key
    assert "t" in ids  # temporal-only hit surfaces
    # "b" appears in two arms → summed RRF score → outranks single-arm hits
    assert ids[0] == "b"


# ---------- end-to-end wiring (subprocess, fake reflect CLI) ----------

@pytest.fixture()
def fake_reflect(tmp_path):
    """A fake `reflect` CLI returning one fixed chunk for any search."""
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text(
        """#!/usr/bin/env python3
import json
chunk = "---\\nname: from-engine\\nconfidence: high\\n---\\nredis pool body"
print(json.dumps({"context": chunk}))
"""
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script.parent


def _run_recall(bin_dir: Path, home: Path, query: str, *args):
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "HOME": str(home),  # QMD_DOCS_ROOT derives from Path.home()
        "REFLECT_STATE_DIR": str(home / "state"),
        "RECALL_CROSS_ENCODER": "0",
        "RECALL_MMR": "0",
        "RECALL_GAP_LOG": "0",
    }
    env.pop("GLOBAL_LEARNINGS_PATH", None)  # don't let an outer sandbox leak in
    return subprocess.run(
        [sys.executable, str(RECALL), query,
         "--format", "json", "--no-cache", *args],
        capture_output=True, text=True, timeout=60, env=env,
    )


@pytest.fixture()
def home_with_corpus(tmp_path):
    """A $HOME whose ~/.learnings/documents holds one note dated inside
    'last week' (relative to the real clock) and one ancient note."""
    home = tmp_path / "home"
    docs = home / ".learnings" / "documents" / "learnings"
    recent = datetime.now() - timedelta(days=datetime.now().weekday() + 4)
    _write_learning(docs, "corpus-recent",
                    recent.strftime("%Y-%m-%dT12:00:00Z"),
                    "redis pool exhaustion postmortem")
    _write_learning(docs, "corpus-ancient", "2020-01-01T00:00:00Z",
                    "redis pool exhaustion archaeology")
    return home


def test_dated_query_surfaces_in_window_corpus_note(fake_reflect, home_with_corpus):
    r = _run_recall(fake_reflect, home_with_corpus,
                    "redis pool exhaustion last week")
    assert r.returncode == 0, r.stderr
    payload = json.loads(r.stdout)
    ids = [x["id"] for x in payload["results"]]
    assert "corpus-recent" in ids  # temporal arm contributed
    assert "corpus-ancient" not in ids  # outside the window
    assert "from-engine" in ids  # primary arm still fused in
    assert payload["temporal"] is not None  # R6 surface intact


def test_date_free_query_gets_no_temporal_contribution(fake_reflect, home_with_corpus):
    # Acceptance bullet 1, end to end: same corpus, no date phrase → the
    # corpus-scan notes must NOT appear (qmd isn't on PATH, so any corpus
    # hit could only have come from the temporal arm).
    r = _run_recall(fake_reflect, home_with_corpus, "redis pool exhaustion")
    assert r.returncode == 0, r.stderr
    payload = json.loads(r.stdout)
    ids = [x["id"] for x in payload["results"]]
    assert "corpus-recent" not in ids
    assert "corpus-ancient" not in ids
    assert ids == ["from-engine"]
    assert payload["temporal"] is None


def test_arm_env_kill_switch_end_to_end(fake_reflect, home_with_corpus):
    env = {
        **os.environ,
        "PATH": f"{fake_reflect}:/usr/bin:/bin",
        "HOME": str(home_with_corpus),
        "REFLECT_STATE_DIR": str(home_with_corpus / "state"),
        "RECALL_CROSS_ENCODER": "0",
        "RECALL_MMR": "0",
        "RECALL_GAP_LOG": "0",
        "RECALL_TEMPORAL_ARM": "0",
    }
    env.pop("GLOBAL_LEARNINGS_PATH", None)
    r = subprocess.run(
        [sys.executable, str(RECALL), "redis pool exhaustion last week",
         "--format", "json", "--no-cache"],
        capture_output=True, text=True, timeout=60, env=env,
    )
    assert r.returncode == 0, r.stderr
    payload = json.loads(r.stdout)
    ids = [x["id"] for x in payload["results"]]
    assert "corpus-recent" not in ids  # arm off
    assert payload["temporal"] is not None  # extraction (R6) stays on


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
