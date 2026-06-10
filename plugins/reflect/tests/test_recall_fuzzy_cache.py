# ABOUTME: Regression tests for port R9 — the fuzzy cache tier. Pins the two
# ABOUTME: acceptance bullets: (1) fuzzy hits log ≥30% on a sample session of
# ABOUTME: query variants, (2) TTL is still respected (a fuzzy match can never
# ABOUTME: resurrect an expired payload) — plus the Jaccard/index mechanics.
"""Port R9: fuzzy cache tier (ByteRover query-executor Tier 0/1).

Before re-running the retrieval arms, the exact-hash cache miss falls back to
a Jaccard-similarity scan over a token-set sidecar index
(~/.reflect/recall_cache/index.json) — near-identical query variants reuse
the prior cached result.

Acceptance bullets pinned here:
  1. fuzzy hit logs ~30%+ on a sample session (recall_log.jsonl cache_tier)
  2. TTL still respected
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

import recall as recall_mod  # noqa: E402
from recall import (  # noqa: E402
    cache_path,
    fuzzy_read_cache,
    jaccard_similarity,
    query_token_set,
    read_cache_index,
    update_cache_index,
    write_cache,
)


# ---------- jaccard + tokenization mechanics ----------

def test_jaccard_identical_sets():
    assert jaccard_similarity({"a", "b"}, {"a", "b"}) == 1.0


def test_jaccard_disjoint_sets():
    assert jaccard_similarity({"a"}, {"b"}) == 0.0


def test_jaccard_both_empty_is_identical():
    # ByteRover jaccardSimilarity shape: two empty sets are identical.
    assert jaccard_similarity(set(), set()) == 1.0


def test_jaccard_one_empty_is_zero():
    assert jaccard_similarity({"a"}, set()) == 0.0


def test_jaccard_partial_overlap():
    # {a,b,c} vs {a,b,c,d}: 3 / 4 = 0.75
    assert jaccard_similarity({"a", "b", "c"}, {"a", "b", "c", "d"}) == 0.75


def test_token_set_filters_stopwords_and_short_tokens():
    toks = query_token_set("how does the redis connection pooling work")
    assert "redis" in toks and "connection" in toks and "pooling" in toks
    assert "how" not in toks and "the" not in toks and "does" not in toks


def test_token_set_word_order_invariant():
    a = query_token_set("tmux kill-server destroys sessions")
    b = query_token_set("destroys tmux kill-server sessions")
    assert a == b and jaccard_similarity(a, b) == 1.0


# ---------- index + fuzzy lookup mechanics ----------

@pytest.fixture()
def state(tmp_path, monkeypatch):
    """Isolated reflect state dir; KB-mtime invalidation neutralized so the
    machine's live ~/.learnings can't flip cache validity mid-test."""
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setattr(recall_mod, "kb_last_modified", lambda: 0.0)
    return tmp_path / "state"


def _seed(query: str, mode: str = "naive", limit: int = 20) -> Path:
    """Write a cache payload + index entry the way recall()'s miss path does."""
    cache_file = cache_path(query, mode, limit)
    write_cache(cache_file, {
        "query": query,
        "mode": mode,
        "fetched_at": time.time(),
        "ce_scores": None,
        "embeddings": None,
        "results": [{
            "chunk_text": "---\nid: seeded\nconfidence: high\n---\nbody",
            "frontmatter": {"id": "seeded", "confidence": "high"},
            "archived_at": None,
        }],
    })
    update_cache_index(query, mode, limit, cache_file)
    return cache_file


def test_fuzzy_hit_on_word_order_variant(state):
    _seed("tmux kill-server destroys sessions")
    payload = fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    )
    assert payload is not None
    assert payload["results"][0]["frontmatter"]["id"] == "seeded"


def test_fuzzy_hit_on_stopword_variant(state):
    _seed("tmux kill-server destroys sessions")
    payload = fuzzy_read_cache(
        "how does the tmux kill-server destroys sessions", "naive", 20, 3600
    )
    assert payload is not None


def test_fuzzy_miss_below_threshold(state):
    # {redis, connection, pooling} vs {redis, connection, pooling, timeout}:
    # Jaccard 0.75 < 0.85 default threshold → no aliasing.
    _seed("redis connection pooling")
    assert fuzzy_read_cache(
        "redis connection pooling timeout", "naive", 20, 3600
    ) is None


def test_fuzzy_skips_single_token_queries(state):
    # ByteRover guard: < 2 meaningful tokens is too ambiguous to alias.
    _seed("redis")
    assert fuzzy_read_cache("redis", "naive", 20, 3600) is None


def test_fuzzy_requires_same_mode_and_limit(state):
    _seed("tmux kill-server destroys sessions", mode="naive", limit=20)
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "local", 20, 3600
    ) is None
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 10, 3600
    ) is None


def test_fuzzy_skips_stale_version_entries(state):
    _seed("tmux kill-server destroys sessions")
    index = read_cache_index()
    for entry in index.values():
        entry["version"] = "v0-ancient"
    (state / "recall_cache" / "index.json").write_text(json.dumps(index))
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    ) is None


def test_fuzzy_disabled_via_gate(state, monkeypatch):
    _seed("tmux kill-server destroys sessions")
    monkeypatch.setattr(recall_mod, "FUZZY_CACHE_ENABLED", False)
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    ) is None


def test_corrupt_index_degrades_to_exact_only(state):
    _seed("tmux kill-server destroys sessions")
    (state / "recall_cache" / "index.json").write_text("not json {{{")
    assert read_cache_index() == {}
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    ) is None


def test_index_prunes_entries_whose_payload_vanished(state):
    f1 = _seed("tmux kill-server destroys sessions")
    f1.unlink()  # payload gone; entry should be pruned on next update
    _seed("redis connection pooling exhaustion")
    index = read_cache_index()
    assert f1.stem not in index
    assert len(index) == 1


def test_index_capped_to_max_entries(state, monkeypatch):
    monkeypatch.setattr(recall_mod, "FUZZY_INDEX_MAX_ENTRIES", 3)
    for i in range(5):
        _seed(f"distinct query number {i} about topic{i} detail{i}")
    assert len(read_cache_index()) == 3


# ---------- acceptance bullet 2: TTL still respected ----------

def test_fuzzy_never_resurrects_expired_payload(state):
    cache_file = _seed("tmux kill-server destroys sessions")
    # Age the payload 2 hours past a 1-hour TTL.
    old = time.time() - 7200
    os.utime(cache_file, (old, old))
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    ) is None


def test_fuzzy_respects_kb_mtime_invalidation(state, monkeypatch):
    _seed("tmux kill-server destroys sessions")
    # KB written AFTER the cache entry → entry invalid for fuzzy reuse too.
    monkeypatch.setattr(
        recall_mod, "kb_last_modified", lambda: time.time() + 10
    )
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    ) is None


def test_fresh_entry_within_ttl_still_hits(state):
    _seed("tmux kill-server destroys sessions")
    assert fuzzy_read_cache(
        "destroys tmux kill-server sessions", "naive", 20, 3600
    ) is not None


# ---------- acceptance bullet 1: ≥30% fuzzy hits on a sample session ----------

@pytest.fixture()
def fake_reflect(tmp_path):
    """A fake `reflect` CLI returning one chunk per search call."""
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text("""#!/usr/bin/env python3
import json, sys
chunk = "---\\nname: hit-doc\\nconfidence: high\\n---\\ntmux kill-server destroys sessions redis pool exhaustion debugging"
print(json.dumps({"context": chunk}))
""")
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script.parent


def _run_recall(bin_dir: Path, home: Path, state: Path, query: str):
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        # Isolated HOME → kb_last_modified()=0 and no live ~/.learnings leak.
        "HOME": str(home),
        "REFLECT_STATE_DIR": str(state),
        "RECALL_CROSS_ENCODER": "0",
        "RECALL_MMR": "0",
        "RECALL_GRAPH_ARM": "0",
        "RECALL_GAP_LOG": "0",
    }
    return subprocess.run(
        [sys.executable, str(RECALL), query, "--format", "json"],
        capture_output=True, text=True, timeout=60, env=env,
    )


def test_sample_session_fuzzy_hit_rate_at_least_30pct(fake_reflect, tmp_path):
    """A sample session of 6 recalls where half are near-identical variants:
    2 cold misses, 3 fuzzy hits, 1 exact hit → fuzzy rate 50% ≥ 30%."""
    home = tmp_path / "home"
    home.mkdir()
    state = tmp_path / "state"
    session = [
        "tmux kill-server destroys sessions",          # miss (cold)
        "destroys tmux kill-server sessions",          # fuzzy (reorder)
        "the tmux kill-server destroys sessions",      # fuzzy (stopword)
        "redis pool exhaustion debugging",             # miss (new topic)
        "debugging redis pool exhaustion",             # fuzzy (reorder)
        "redis pool exhaustion debugging",             # exact (repeat)
    ]
    for query in session:
        r = _run_recall(fake_reflect, home, state, query)
        assert r.returncode == 0, r.stderr
        assert json.loads(r.stdout)["count"] >= 1, r.stdout

    log = state / "recall_log.jsonl"
    records = [json.loads(line) for line in log.read_text().splitlines()]
    assert len(records) == len(session)
    tiers = [rec.get("cache_tier") for rec in records]
    fuzzy_rate = tiers.count("fuzzy") / len(records)
    assert fuzzy_rate >= 0.30, tiers
    # The tier sequence itself is pinned: cold, fuzzy, fuzzy, cold, fuzzy, exact.
    assert tiers == [None, "fuzzy", "fuzzy", None, "fuzzy", "exact"], tiers


def test_no_cache_flag_bypasses_fuzzy_tier(fake_reflect, tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    state = tmp_path / "state"
    r1 = _run_recall(fake_reflect, home, state, "tmux kill-server destroys sessions")
    assert r1.returncode == 0, r1.stderr
    env = {
        **os.environ,
        "PATH": f"{fake_reflect}:/usr/bin:/bin",
        "HOME": str(home),
        "REFLECT_STATE_DIR": str(state),
        "RECALL_CROSS_ENCODER": "0",
        "RECALL_MMR": "0",
        "RECALL_GRAPH_ARM": "0",
        "RECALL_GAP_LOG": "0",
    }
    r2 = subprocess.run(
        [sys.executable, str(RECALL), "destroys tmux kill-server sessions",
         "--format", "json", "--no-cache"],
        capture_output=True, text=True, timeout=60, env=env,
    )
    assert r2.returncode == 0, r2.stderr
    records = [
        json.loads(line)
        for line in (state / "recall_log.jsonl").read_text().splitlines()
    ]
    assert records[-1]["cached"] is False
    assert records[-1]["cache_tier"] is None


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
