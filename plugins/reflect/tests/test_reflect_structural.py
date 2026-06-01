"""Tests for W5 structural pieces: graphml_repair + regate_backlog + surfacer no-op."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
SURFACER = PLUGIN_ROOT / "hooks" / "sessionstart_drain_reflections.py"
sys.path.insert(0, str(SCRIPTS))

import graphml_repair  # noqa: E402
import regate_backlog  # noqa: E402

_VALID_GRAPHML = (
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<graphml xmlns="http://graphml.graphdrawing.org/xmlns">\n'
    '  <graph edgedefault="undirected">\n'
    '    <node id="n1"/>\n'
    '    <node id="n2"/>\n'
    '    <edge source="n1" target="n2"/>\n'
    '  </graph>\n'
    '</graphml>\n'
)


# ── graphml_repair ───────────────────────────────────────────────────────────

def test_valid_graphml_passes(tmp_path):
    g = tmp_path / "g.graphml"
    g.write_text(_VALID_GRAPHML)
    assert graphml_repair.is_valid(g)
    assert graphml_repair.repair(g, quiet=True) is True


def test_doubled_close_tag_is_repaired(tmp_path):
    # The exact incident shape: a second </graph></graphml> block appended.
    g = tmp_path / "g.graphml"
    g.write_text(_VALID_GRAPHML + "  </graph>\n</graphml>\n")
    assert not graphml_repair.is_valid(g)            # corrupt as written
    assert graphml_repair.repair(g, quiet=True) is True
    assert graphml_repair.is_valid(g)                # repaired
    assert (tmp_path / "g.graphml.corrupt.bak").exists()  # original backed up


def test_unrepairable_corruption_restores_original(tmp_path):
    g = tmp_path / "g.graphml"
    broken = "<graphml><graph><node id='x'"  # truncated, no close tag
    g.write_text(broken)
    assert graphml_repair.repair(g, quiet=True) is False
    assert g.read_text() == broken  # original preserved, not made worse


def test_repair_text_noop_when_clean():
    assert graphml_repair.repair_text(_VALID_GRAPHML) is None  # nothing trailing


# ── regate_backlog ───────────────────────────────────────────────────────────

def _queue(sd: Path, transcripts: list[str]) -> None:
    sd.mkdir(parents=True, exist_ok=True)
    lines = [json.dumps({"ts": "t", "session_id": f"s{i}", "transcript_path": tp,
                         "trigger": "stop", "cwd": "/"})
             for i, tp in enumerate(transcripts)]
    (sd / "pending_reflections.jsonl").write_text("\n".join(lines) + "\n")


def test_regate_drops_worthless_and_dedups(tmp_path):
    # one signal-bearing, one reflect-on-reflect, one clean, plus a dup of #1
    sig = tmp_path / "sig.jsonl"
    sig.write_text(json.dumps({"message": {"role": "user",
                  "content": "No, never use var. The root cause was a missing index."}}) + "\n")
    ror = tmp_path / "ror.jsonl"
    ror.write_text(json.dumps({"message": {"role": "user",
                  "content": "<command-name>reflect</command-name> Process the transcript at: /x"}}) + "\n")
    clean = tmp_path / "clean.jsonl"
    clean.write_text(json.dumps({"message": {"role": "user",
                  "content": "Morning, summarize the document."}}) + "\n")

    sd = tmp_path / "state"
    _queue(sd, [str(sig), str(ror), str(clean), str(sig)])  # sig appears twice

    res = regate_backlog.regate(sd, dry_run=False)
    assert res["total"] == 4
    assert res["kept"] == 1                      # only the unique signal-bearing one
    assert res["dropped"] == 3
    # survivor queue has exactly the signal transcript
    survivors = [json.loads(l) for l in
                 (sd / "pending_reflections.jsonl").read_text().splitlines() if l.strip()]
    assert len(survivors) == 1
    assert survivors[0]["transcript_path"] == str(sig)
    # original archived
    assert (sd / "pending_reflections.jsonl.pre-regate").exists()


def test_regate_dry_run_does_not_modify(tmp_path):
    sd = tmp_path / "state"
    _queue(sd, ["/nonexistent/a.jsonl"])
    before = (sd / "pending_reflections.jsonl").read_text()
    regate_backlog.regate(sd, dry_run=True)
    assert (sd / "pending_reflections.jsonl").read_text() == before
    assert not (sd / "pending_reflections.jsonl.pre-regate").exists()


# ── surfacer retired ─────────────────────────────────────────────────────────

def test_surfacer_is_retired_noop(tmp_path):
    """Retired surfacer must exit 0 without injecting additionalContext, even
    when a queue exists (it must NOT surface it — bg drainer is sole consumer)."""
    state = tmp_path / "state"
    state.mkdir()
    (state / "pending_reflections.jsonl").write_text(
        json.dumps({"transcript_path": "/x.jsonl", "session_id": "s"}) + "\n"
    )
    import os
    r = subprocess.run([sys.executable, str(SURFACER)], input="{}", text=True,
                       capture_output=True, timeout=30,
                       env={**os.environ, "REFLECT_STATE_DIR": str(state)})
    assert r.returncode == 0
    assert "additionalContext" not in r.stdout  # no longer surfaces the queue


# ── reflect_synthesis clustering ─────────────────────────────────────────────

import reflect_synthesis  # noqa: E402


def _doc(title, tags=None):
    return reflect_synthesis.Doc(path=Path(f"/{title}.md"), title=title, tags=tags or [])


def test_synthesis_clusters_near_dupes():
    docs = [
        _doc("Supabase RLS policy misconfiguration on profiles"),
        _doc("Supabase RLS policy misconfigured for profiles table"),
        _doc("Tailwind responsive grid breakpoint trick"),
    ]
    clusters = reflect_synthesis.cluster(docs, threshold=0.4)
    assert len(clusters) == 1            # the two RLS notes group
    assert len(clusters[0]) == 2
    titles = {d.title for d in clusters[0]}
    assert all("rls" in t.lower() for t in titles)


def test_synthesis_ignores_unrelated():
    docs = [_doc("Postgres offset reserved keyword"), _doc("React hook dependency array")]
    assert reflect_synthesis.cluster(docs, threshold=0.5) == []


def test_synthesis_uses_tag_overlap():
    docs = [
        _doc("Edge function cold start", tags=["supabase", "edge", "performance"]),
        _doc("Function boot latency", tags=["supabase", "edge", "performance"]),
    ]
    clusters = reflect_synthesis.cluster(docs, threshold=0.4)
    assert len(clusters) == 1  # grouped via shared tags despite different titles


# ── launchd plist validity ───────────────────────────────────────────────────

def test_launchd_plists_are_valid():
    import plistlib
    launchd = PLUGIN_ROOT / "launchd"
    for name in ("com.reflect.drain.plist", "com.reflect.synthesis.plist"):
        p = launchd / name
        assert p.exists(), f"missing {name}"
        with open(p, "rb") as fh:
            data = plistlib.load(fh)
        assert data.get("Label", "").startswith("com.reflect.")
        assert "{{PLUGIN_ROOT}}" in p.read_text()  # template placeholder present


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
