# ABOUTME: Regression tests for port R3 — MMR diversity selection in recall.py.
# ABOUTME: Pins ≤1 near-dup in the top-3, λ tunability (--mmr-lambda /
# ABOUTME: RECALL_MMR_LAMBDA), the --no-mmr escape hatch, and silent degrade.
"""Port R3: MMR diversity step, plugin side.

Pipeline position: rerank → filter_by_confidence → OOD gate → mmr_select
(replaces the plain ``[:limit]`` slice). Embeddings come from `reflect
embed` (all-mpnet-base-v2, the index's embedding space), fetched
concurrently with the R2 cross-encoder call and cached per query.

Acceptance bullets pinned here:
  - with 5 near-dup learnings in corpus, top-3 inject contains at most 1
  - λ config-tunable (--mmr-lambda flag and RECALL_MMR_LAMBDA env)
  - disabled by --no-mmr flag for benchmarking (plus RECALL_MMR=0 env gate)
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL = PLUGIN_ROOT / "skills" / "recall" / "scripts" / "recall.py"
sys.path.insert(0, str(RECALL.parent))

from recall import (  # noqa: E402
    MMR_CANDIDATES,
    MMR_LAMBDA,
    Learning,
    _coerce_embeddings,
    _cosine,
    _learning_key,
    mmr_select,
    rerank,
    rerank_with_scores,
)


def _lrn(name: str, confidence: str = "high", text: str = "body") -> Learning:
    return Learning(chunk_text=text, frontmatter={"name": name, "confidence": confidence})


# Realistic mpnet-scale geometry: query-relevance cosines cluster in a
# narrow band (~0.5-0.6) while near-dup pairwise similarity is ~1.0 —
# that gap is exactly what MMR exploits.
QUERY_VEC = [1.0, 0.0, 0.0]
DUP_VEC = [0.6, 0.8, 0.0]        # rel 0.60; dup↔dup sim 1.0
ALT1_VEC = [0.55, 0.0, 0.8352]   # rel 0.55; sim to dup ≈ 0.33
ALT2_VEC = [0.5, -0.866, 0.0]    # rel 0.50; sim to dup ≈ -0.39


def _corpus() -> tuple[list[Learning], tuple[list[float], dict[str, list[float]]]]:
    """5 near-dups ranked above 2 complementary learnings (post-rerank order)."""
    learnings = [_lrn(f"dup-{i}") for i in range(5)] + [_lrn("alt-1"), _lrn("alt-2")]
    docs = {_learning_key(l): list(DUP_VEC) for l in learnings[:5]}
    docs["alt-1"] = list(ALT1_VEC)
    docs["alt-2"] = list(ALT2_VEC)
    return learnings, (list(QUERY_VEC), docs)


# ---------- unit: cosine ----------

def test_cosine_basics():
    assert _cosine([1.0, 0.0], [0.0, 1.0]) == pytest.approx(0.0)
    assert _cosine([0.6, 0.8], [0.6, 0.8]) == pytest.approx(1.0)
    assert _cosine([3.0, 4.0], [0.6, 0.8]) == pytest.approx(1.0)  # norm-guarded
    assert _cosine([0.0, 0.0], [1.0, 0.0]) == 0.0  # zero vector never crashes


# ---------- unit: embedding coercion ----------

def test_coerce_embeddings_shapes():
    good = {"query": [1.0, 0.0], "docs": {"a": [0.5, 0.5], "b": [1, 0]}}
    coerced = _coerce_embeddings(good)
    assert coerced is not None
    qv, docs = coerced
    assert qv == [1.0, 0.0]
    assert docs == {"a": [0.5, 0.5], "b": [1.0, 0.0]}
    assert _coerce_embeddings(None) is None
    assert _coerce_embeddings("junk") is None
    assert _coerce_embeddings({}) is None
    assert _coerce_embeddings({"query": [1.0], "docs": {}}) is None
    assert _coerce_embeddings({"query": None, "docs": {"a": [1.0]}}) is None
    assert _coerce_embeddings({"query": [1.0], "docs": {"a": "junk"}}) is None
    # dimension mismatch would silently corrupt cosines — must reject
    assert _coerce_embeddings({"query": [1.0, 0.0], "docs": {"a": [1.0]}}) is None
    assert _coerce_embeddings({"query": [1.0], "docs": {"a": [True]}}) is None


# ---------- unit: mmr_select ----------

def test_top3_contains_at_most_one_dup():
    """ACCEPTANCE: 5 near-dups in the corpus → top-3 keeps at most 1."""
    learnings, emb = _corpus()
    out = mmr_select(learnings, emb, k=3)
    dups = [l for l in out if l.id.startswith("dup-")]
    assert len(out) == 3
    assert len(dups) <= 1
    assert out[0].id == "dup-0"  # top-1 from the rerank is preserved


def test_complementary_learnings_fill_later_slots():
    learnings, emb = _corpus()
    out = mmr_select(learnings, emb, k=3)
    assert {l.id for l in out} == {"dup-0", "alt-1", "alt-2"}


def test_top1_preserved_even_when_query_cosine_disagrees():
    """First pick is the reranked head, NOT the highest query-cosine."""
    learnings, (qv, docs) = _corpus()
    reordered = [learnings[5]] + learnings[:5] + [learnings[6]]  # alt-1 first
    out = mmr_select(reordered, (qv, docs), k=3)
    assert out[0].id == "alt-1"


def test_lambda_one_is_pure_relevance():
    """ACCEPTANCE (λ tunable): λ=1.0 disables the diversity penalty."""
    learnings, emb = _corpus()
    out = mmr_select(learnings, emb, k=3, lam=1.0)
    assert [l.id for l in out] == ["dup-0", "dup-1", "dup-2"]


def test_lambda_zero_is_pure_diversity():
    learnings, emb = _corpus()
    out = mmr_select(learnings, emb, k=3, lam=0.0)
    dups = [l for l in out if l.id.startswith("dup-")]
    assert len(dups) == 1  # only the pinned top-1


def test_lambda_clamped_to_unit_interval():
    learnings, emb = _corpus()
    assert [l.id for l in mmr_select(learnings, emb, 3, lam=5.0)] == \
        [l.id for l in mmr_select(learnings, emb, 3, lam=1.0)]
    assert [l.id for l in mmr_select(learnings, emb, 3, lam=-2.0)] == \
        [l.id for l in mmr_select(learnings, emb, 3, lam=0.0)]


def test_default_lambda_is_module_config():
    learnings, emb = _corpus()
    assert [l.id for l in mmr_select(learnings, emb, 3)] == \
        [l.id for l in mmr_select(learnings, emb, 3, lam=MMR_LAMBDA)]
    assert MMR_LAMBDA == pytest.approx(0.7)


def test_degrades_to_slice_without_embeddings():
    learnings, _ = _corpus()
    assert mmr_select(learnings, None, 3) == learnings[:3]


def test_degrades_when_top1_unembedded():
    learnings, (qv, docs) = _corpus()
    del docs["dup-0"]
    assert mmr_select(learnings, (qv, docs), 3) == learnings[:3]


def test_unembedded_tail_fills_remaining_slots_in_order():
    a, b, c, d = _lrn("a"), _lrn("b"), _lrn("c"), _lrn("d")
    emb = (list(QUERY_VEC), {"a": list(DUP_VEC), "b": list(ALT1_VEC)})
    out = mmr_select([a, b, c, d], emb, k=4)
    assert [l.id for l in out] == ["a", "b", "c", "d"]


def test_k_edges():
    learnings, emb = _corpus()
    assert mmr_select(learnings, emb, 0) == []
    assert mmr_select([], emb, 3) == []
    assert len(mmr_select(learnings, emb, 100)) == len(learnings)
    assert mmr_select(learnings, emb, 1) == [learnings[0]]


# ---------- unit: rel(d,q) comes from the rerank, not the bi-encoder ----------

def test_rel_scores_preserve_rerank_signal():
    """rel(d,q) must be the rerank's score (CE + recency blend), not the raw
    query-cosine — otherwise MMR resurrects superseded/demoted candidates.
    Here C has a slightly lower query-cosine than B, but the rerank scored
    it far higher (e.g. newer convention): with rel_scores C must win slot 2."""
    a, b, c = _lrn("a"), _lrn("b"), _lrn("c")
    emb = (list(QUERY_VEC), {
        "a": list(DUP_VEC),    # pinned top-1
        "b": list(ALT2_VEC),   # cos(q,b)=0.50, sim(a,b)≈-0.39 → cosine-rel favourite
        "c": list(ALT1_VEC),   # cos(q,c)=0.55, sim(a,c)≈0.33
    })
    scores = {"a": 1.0, "b": 0.2, "c": 1.0}
    with_scores = mmr_select([a, b, c], emb, k=2, rel_scores=scores)
    without_scores = mmr_select([a, b, c], emb, k=2)
    assert [l.id for l in with_scores] == ["a", "c"]
    assert [l.id for l in without_scores] == ["a", "b"]  # cosine fallback


def test_rel_scores_normalized_by_window_max():
    """Scale invariance: multiplying all rerank scores by a constant must
    not change the selection (rel is score/max, not the raw score)."""
    learnings, emb = _corpus()
    scores = {_learning_key(l): 0.9 for l in learnings[:5]}
    scores.update({"alt-1": 0.8, "alt-2": 0.75})
    small = mmr_select(learnings, emb, 3, rel_scores=scores)
    big = mmr_select(learnings, emb, 3,
                     rel_scores={k: v * 1000 for k, v in scores.items()})
    assert [l.id for l in small] == [l.id for l in big]


def test_rel_scores_nonpositive_falls_back_to_cosine():
    learnings, emb = _corpus()
    zeros = {_learning_key(l): 0.0 for l in learnings}
    assert [l.id for l in mmr_select(learnings, emb, 3, rel_scores=zeros)] == \
        [l.id for l in mmr_select(learnings, emb, 3)]


def test_rerank_with_scores_matches_rerank():
    docs = [
        _lrn("low", confidence="low"),
        _lrn("high", confidence="high"),
        _lrn("mid", confidence="medium"),
    ]
    ordered, scores = rerank_with_scores(list(docs))
    assert [l.id for l in ordered] == [l.id for l in rerank(list(docs))]
    assert set(scores) == {"low", "mid", "high"}
    assert scores["high"] > scores["mid"] > scores["low"]
    # returned list is sorted by exactly these scores
    assert [l.id for l in ordered] == sorted(
        scores, key=lambda k: scores[k], reverse=True
    )


# ---------- e2e: fake reflect CLI ----------

FAKE_CLI = """#!/usr/bin/env python3
import json, os, sys
cmd = sys.argv[1] if len(sys.argv) > 1 else "?"
with open(os.environ["FAKE_CALLS_LOG"], "a") as f:
    f.write(cmd + "\\n")
ALT_VECS = {"alt-1": [0.55, 0.0, 0.8352], "alt-2": [0.5, -0.866, 0.0]}
DUP_VEC = [0.6, 0.8, 0.0]
if cmd == "search":
    n = int(os.environ.get("FAKE_SEARCH_DOCS", "0"))
    if n:
        chunks = [
            "---\\nname: doc-%02d\\nconfidence: high\\n---\\nbody %d" % (i, i)
            for i in range(n)
        ]
    else:
        chunks = [
            "---\\nname: dup-%d\\nconfidence: high\\n---\\nredis pool exhaustion fix %d" % (i, i)
            for i in range(5)
        ] + [
            "---\\nname: alt-1\\nconfidence: high\\n---\\nredis pool timeout tuning",
            "---\\nname: alt-2\\nconfidence: high\\n---\\nredis pool cluster failover",
        ]
    print(json.dumps({"context": "--New Chunk--".join(chunks)}))
elif cmd == "rerank":
    payload = json.load(sys.stdin)
    ids = [c["id"] for c in payload["candidates"]]
    scores = {i: (8.0 if i.startswith("dup-") else 2.0) for i in ids}
    print(json.dumps({"available": True, "model": "fake", "scores": scores}))
elif cmd == "embed":
    behavior = os.environ.get("FAKE_EMBED_BEHAVIOR", "ok")
    if behavior == "usage":
        sys.stderr.write("Error: No such command 'embed'\\n")
        sys.exit(2)
    if behavior == "exit3":
        sys.exit(3)
    payload = json.load(sys.stdin)
    ids = [c["id"] for c in payload["candidates"]]
    with open(os.environ["FAKE_EMBED_LOG"], "a") as f:
        f.write(json.dumps(ids) + "\\n")
    if behavior == "unavailable":
        print(json.dumps({"available": False, "error": "slim build"}))
        sys.exit(0)
    embs = {i: ALT_VECS.get(i, DUP_VEC) for i in ids}
    print(json.dumps({"available": True, "model": "fake-mpnet",
                      "query_embedding": [1.0, 0.0, 0.0], "embeddings": embs}))
"""

DIVERSIFIED_TOP3 = ["dup-0", "alt-2", "alt-1"]
PLAIN_TOP3 = ["dup-0", "dup-1", "dup-2"]


@pytest.fixture()
def fake_reflect(tmp_path):
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text(FAKE_CLI)
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    calls = tmp_path / "calls.log"
    embed_log = tmp_path / "embed.log"
    return script.parent, calls, embed_log


def _run_recall(bin_dir, tmp_path, calls, embed_log, *args, env_extra=None, cache=False):
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:/usr/bin:/bin",  # no real reflect/qmd
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "RECALL_GRAPH_ARM": "0",  # single search arm keeps fusion deterministic
        "FAKE_CALLS_LOG": str(calls),
        "FAKE_EMBED_LOG": str(embed_log),
    }
    # The host shell must not leak MMR/CE config into the subprocess.
    for var in ("RECALL_MMR", "RECALL_MMR_LAMBDA", "RECALL_CROSS_ENCODER"):
        env.pop(var, None)
    env.update(env_extra or {})
    cache_args = [] if cache else ["--no-cache"]
    return subprocess.run(
        [sys.executable, str(RECALL), "redis pool exhaustion",
         "--format", "json", "--limit", "3", *cache_args, *args],
        capture_output=True, text=True, timeout=60, env=env,
    )


def _ids(proc):
    return [x["id"] for x in json.loads(proc.stdout)["results"]]


def test_e2e_mmr_diversifies_top3(fake_reflect, tmp_path):
    """ACCEPTANCE: 5 near-dups in corpus → top-3 inject contains at most 1."""
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log)
    assert r.returncode == 0, r.stderr
    ids = _ids(r)
    assert ids == DIVERSIFIED_TOP3
    assert sum(1 for i in ids if i.startswith("dup-")) <= 1
    assert calls.read_text().split().count("embed") == 1


def test_e2e_no_mmr_flag_disables(fake_reflect, tmp_path):
    """ACCEPTANCE: --no-mmr restores plain reranked top-k (benchmarking)."""
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log, "--no-mmr")
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3
    assert "embed" not in calls.read_text().split()  # no wasted subprocess


def test_e2e_env_gate_disables(fake_reflect, tmp_path):
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log,
                    env_extra={"RECALL_MMR": "0"})
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3
    assert "embed" not in calls.read_text().split()


def test_e2e_lambda_flag_tunable(fake_reflect, tmp_path):
    """ACCEPTANCE: λ config-tunable — λ=1.0 keeps pure relevance order."""
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log, "--mmr-lambda", "1.0")
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3
    assert "embed" in calls.read_text().split()  # MMR ran, λ neutralised it


def test_e2e_lambda_env_tunable(fake_reflect, tmp_path):
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log,
                    env_extra={"RECALL_MMR_LAMBDA": "1.0"})
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3


def test_e2e_embed_failure_is_nonfatal(fake_reflect, tmp_path):
    """Booster contract: a crashing embed subcommand must not kill recall."""
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log,
                    env_extra={"FAKE_EMBED_BEHAVIOR": "exit3"})
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3


def test_e2e_unavailable_slim_build_degrades(fake_reflect, tmp_path):
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log,
                    env_extra={"FAKE_EMBED_BEHAVIOR": "unavailable"})
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3


def test_e2e_legacy_cli_without_subcommand_degrades(fake_reflect, tmp_path):
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log,
                    env_extra={"FAKE_EMBED_BEHAVIOR": "usage"})
    assert r.returncode == 0, r.stderr
    assert _ids(r) == PLAIN_TOP3


def test_e2e_candidates_capped_at_embed_window(fake_reflect, tmp_path):
    bin_dir, calls, embed_log = fake_reflect
    r = _run_recall(bin_dir, tmp_path, calls, embed_log,
                    "--limit", "30", env_extra={"FAKE_SEARCH_DOCS": "30"})
    assert r.returncode == 0, r.stderr
    sent = json.loads(embed_log.read_text().splitlines()[0])
    assert len(sent) == MMR_CANDIDATES == 20


def test_e2e_cache_stores_and_reuses_embeddings(fake_reflect, tmp_path):
    """Cache hits must reuse the cached embeddings — same diversified order,
    zero new subprocess calls (no search, no rerank, no embed)."""
    bin_dir, calls, embed_log = fake_reflect
    r1 = _run_recall(bin_dir, tmp_path, calls, embed_log, cache=True)
    assert r1.returncode == 0, r1.stderr
    assert _ids(r1) == DIVERSIFIED_TOP3
    calls_after_first = calls.read_text().split()
    assert calls_after_first.count("embed") == 1

    r2 = _run_recall(bin_dir, tmp_path, calls, embed_log, cache=True)
    assert r2.returncode == 0, r2.stderr
    assert _ids(r2) == DIVERSIFIED_TOP3  # MMR order preserved from cache
    assert calls.read_text().split() == calls_after_first  # no new CLI calls


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
