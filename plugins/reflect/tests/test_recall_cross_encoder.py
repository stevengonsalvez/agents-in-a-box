# ABOUTME: Regression tests for port R2 — cross-encoder rerank step in recall.py.
# ABOUTME: Pins CE-primary ordering (legacy formula degrades to a multiplicative
# ABOUTME: modifier), the 20-candidate batch cap, silent degrade, and CE-score caching.
"""Port R2: cross-encoder rerank, plugin side.

Pipeline position: rrf_fuse → fetch_ce_scores (`reflect rerank` subprocess)
→ rerank(ce_scores=...). With CE scores the sigmoid(logit) is the PRIMARY
sort key and the legacy confidence × recency × tags × proof formula becomes
a multiplicative modifier on it. Without CE (slim build, legacy CLI, any
failure, RECALL_CROSS_ENCODER=0) ordering is byte-identical to pre-R2.

Acceptance bullets pinned here:
  - existing formula degrades to a multiplicative modifier
  - candidates are CE-scored in one batch of ≤ 20
  - CE scores are cached per query (cache hits never re-invoke the model)
  - p95 latency: the plugin-side post-processing (sigmoid × formula sort) is
    sub-300ms even for 100 candidates (model-side latency pinned engine-side)
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL = PLUGIN_ROOT / "skills" / "recall" / "scripts" / "recall.py"
sys.path.insert(0, str(RECALL.parent))

import importlib  # noqa: E402

recall_mod = importlib.import_module("recall")
from recall import (  # noqa: E402
    CE_CANDIDATES,
    CE_UNSCORED,
    Learning,
    _ce_sigmoid,
    _coerce_ce_scores,
    _learning_key,
    rerank,
)


def _lrn(name: str, confidence: str = "medium", text: str = "body") -> Learning:
    return Learning(chunk_text=text, frontmatter={"name": name, "confidence": confidence})


# ---------- unit: sigmoid ----------

def test_sigmoid_bounds_and_monotonicity():
    assert 0.0 < _ce_sigmoid(-12.0) < 0.001
    assert 0.999 < _ce_sigmoid(12.0) < 1.0
    assert _ce_sigmoid(0.0) == pytest.approx(0.5)
    assert _ce_sigmoid(-2.0) < _ce_sigmoid(0.0) < _ce_sigmoid(2.0)
    # extreme logits must not overflow
    assert _ce_sigmoid(-100_000.0) == 0.0
    assert _ce_sigmoid(100_000.0) == 1.0


# ---------- unit: rerank with CE scores ----------

def test_ce_score_is_primary_sort_key():
    """High CE + weak formula must beat low CE + strong formula."""
    strong_formula = _lrn("strong", confidence="high")  # formula 1.0
    weak_formula = _lrn("weak", confidence="low")       # formula 0.4
    ce = {"strong": -8.0, "weak": 8.0}
    out = rerank([strong_formula, weak_formula], ce_scores=ce)
    assert [x.id for x in out] == ["weak", "strong"]


def test_without_ce_legacy_order_unchanged():
    strong = _lrn("strong", confidence="high")
    weak = _lrn("weak", confidence="low")
    out = rerank([weak, strong], ce_scores=None)
    assert [x.id for x in out] == ["strong", "weak"]


def test_formula_is_multiplicative_modifier_on_ce_ties():
    """Equal CE scores → ordering must reduce to exactly the legacy formula."""
    docs = [
        _lrn("low", confidence="low"),
        _lrn("high", confidence="high"),
        _lrn("mid", confidence="medium"),
    ]
    ce = {d.id: 3.0 for d in docs}
    with_ce = [x.id for x in rerank(list(docs), ce_scores=ce)]
    legacy = [x.id for x in rerank(list(docs), ce_scores=None)]
    assert with_ce == legacy == ["high", "mid", "low"]


def test_unscored_tail_sorts_below_scored_by_formula():
    scored_low = _lrn("scored-low", confidence="low")
    tail_high = _lrn("tail-high", confidence="high")
    tail_mid = _lrn("tail-mid", confidence="medium")
    ce = {"scored-low": -6.0}  # sigmoid(-6) ≈ 0.0025 — still >> CE_UNSCORED
    out = rerank([tail_mid, scored_low, tail_high], ce_scores=ce)
    assert [x.id for x in out] == ["scored-low", "tail-high", "tail-mid"]
    assert CE_UNSCORED < _ce_sigmoid(-6.0)


def test_coerce_ce_scores_shapes():
    assert _coerce_ce_scores({"a": 1, "b": -2.5}) == {"a": 1.0, "b": -2.5}
    assert _coerce_ce_scores(None) is None
    assert _coerce_ce_scores("junk") is None
    assert _coerce_ce_scores({}) is None
    assert _coerce_ce_scores({"a": "not-a-number"}) is None
    assert _coerce_ce_scores([1, 2]) is None


def test_rerank_latency_for_100_candidates_under_300ms():
    docs = [_lrn(f"d{i}", text="x" * 500) for i in range(100)]
    ce = {d.id: float(i % 7) - 3.0 for i, d in enumerate(docs)}
    start = time.monotonic()
    rerank(docs, query_tags=["redis", "pool"], ce_scores=ce)
    elapsed_ms = (time.monotonic() - start) * 1000
    assert elapsed_ms < 300, f"plugin-side rerank took {elapsed_ms:.1f}ms"


# ---------- e2e: fake reflect CLI ----------

FAKE_CLI = """#!/usr/bin/env python3
import json, os, sys
cmd = sys.argv[1] if len(sys.argv) > 1 else "?"
with open(os.environ["FAKE_CALLS_LOG"], "a") as f:
    f.write(cmd + "\\n")
if cmd == "search":
    n = int(os.environ.get("FAKE_SEARCH_DOCS", "0"))
    if n:
        chunks = [
            "---\\nname: doc-%02d\\nconfidence: medium\\n---\\nbody %d" % (i, i)
            for i in range(n)
        ]
    else:
        chunks = [
            "---\\nname: formula-king\\nconfidence: high\\n---\\nbody formula",
            "---\\nname: ce-king\\nconfidence: low\\n---\\nbody ce",
        ]
    print(json.dumps({"context": "--New Chunk--".join(chunks)}))
elif cmd == "rerank":
    behavior = os.environ.get("FAKE_RERANK_BEHAVIOR", "ok")
    if behavior == "usage":
        sys.stderr.write("Error: No such command 'rerank'\\n")
        sys.exit(2)
    if behavior == "exit3":
        sys.exit(3)
    payload = json.load(sys.stdin)
    ids = [c["id"] for c in payload["candidates"]]
    with open(os.environ["FAKE_RERANK_LOG"], "a") as f:
        f.write(json.dumps(ids) + "\\n")
    if behavior == "unavailable":
        print(json.dumps({"available": False, "error": "slim build"}))
        sys.exit(0)
    scores = {i: (8.0 if i == "ce-king" else -8.0) for i in ids}
    print(json.dumps({"available": True, "model": "fake", "scores": scores}))
"""


@pytest.fixture()
def fake_reflect(tmp_path):
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text(FAKE_CLI)
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    calls = tmp_path / "calls.log"
    rerank_log = tmp_path / "rerank.log"
    return script.parent, calls, rerank_log


def _run_recall(bin_dir, tmp_path, calls, rerank_log, *args, env_extra=None, cache=False):
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:/usr/bin:/bin",  # no real reflect/qmd
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "RECALL_GRAPH_ARM": "0",  # single search arm keeps fusion deterministic
        "FAKE_CALLS_LOG": str(calls),
        "FAKE_RERANK_LOG": str(rerank_log),
        **(env_extra or {}),
    }
    cache_args = [] if cache else ["--no-cache"]
    return subprocess.run(
        [sys.executable, str(RECALL), "redis pool exhaustion",
         "--format", "json", *cache_args, *args],
        capture_output=True, text=True, timeout=60, env=env,
    )


def _ids(proc):
    return [x["id"] for x in json.loads(proc.stdout)["results"]]


def test_e2e_ce_reorders_results(fake_reflect, tmp_path):
    bin_dir, calls, rerank_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, rerank_log)
    assert r.returncode == 0, r.stderr
    assert _ids(r) == ["ce-king", "formula-king"]  # CE primary, not confidence
    assert "rerank" in calls.read_text().split()


def test_e2e_disabled_by_env(fake_reflect, tmp_path):
    bin_dir, calls, rerank_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, rerank_log,
                    env_extra={"RECALL_CROSS_ENCODER": "0"})
    assert r.returncode == 0
    assert "rerank" not in calls.read_text().split()
    assert _ids(r) == ["formula-king", "ce-king"]  # legacy formula order


def test_e2e_rerank_failure_is_nonfatal(fake_reflect, tmp_path):
    """Booster contract: a crashing rerank subcommand must not kill recall."""
    bin_dir, calls, rerank_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, rerank_log,
                    env_extra={"FAKE_RERANK_BEHAVIOR": "exit3"})
    assert r.returncode == 0
    assert _ids(r) == ["formula-king", "ce-king"]  # degraded to formula


def test_e2e_unavailable_slim_build_degrades(fake_reflect, tmp_path):
    bin_dir, calls, rerank_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, rerank_log,
                    env_extra={"FAKE_RERANK_BEHAVIOR": "unavailable"})
    assert r.returncode == 0
    assert _ids(r) == ["formula-king", "ce-king"]


def test_e2e_legacy_cli_without_subcommand_degrades(fake_reflect, tmp_path):
    bin_dir, calls, rerank_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, rerank_log,
                    env_extra={"FAKE_RERANK_BEHAVIOR": "usage"})
    assert r.returncode == 0
    assert _ids(r) == ["formula-king", "ce-king"]


def test_e2e_candidates_capped_at_one_batch(fake_reflect, tmp_path):
    bin_dir, calls, rerank_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, rerank_log,
                    "--limit", "30", env_extra={"FAKE_SEARCH_DOCS": "30"})
    assert r.returncode == 0, r.stderr
    sent = json.loads(rerank_log.read_text().splitlines()[0])
    assert len(sent) == CE_CANDIDATES == 20


def test_e2e_cache_stores_and_reuses_ce_scores(fake_reflect, tmp_path):
    """Cache hits must reuse the cached CE scores — same order, zero new
    subprocess calls (neither search nor rerank)."""
    bin_dir, calls, rerank_log = fake_reflect
    r1 = _run_recall(bin_dir, tmp_path, calls, rerank_log, cache=True)
    assert r1.returncode == 0, r1.stderr
    assert _ids(r1) == ["ce-king", "formula-king"]
    calls_after_first = calls.read_text().split()
    assert calls_after_first.count("rerank") == 1

    r2 = _run_recall(bin_dir, tmp_path, calls, rerank_log, cache=True)
    assert r2.returncode == 0, r2.stderr
    assert _ids(r2) == ["ce-king", "formula-king"]  # CE order preserved
    assert calls.read_text().split() == calls_after_first  # no new CLI calls


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
