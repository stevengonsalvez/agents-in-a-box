# ABOUTME: Regression tests for port R2 — engine-side cross-encoder rerank.
# ABOUTME: Pins model caching under ~/.reflect/models, the load-once singleton,
# ABOUTME: and the `reflect rerank` CLI contract (silent degrade on slim builds).
"""Port R2: cross-encoder rerank, engine side.

Acceptance bullets pinned here:
  - model auto-downloads on first run, cached thereafter
      → the loader points HF/sentence-transformers caches at
        $REFLECT_STATE_DIR/models BEFORE import, creates the dir, and the
        model object is constructed exactly once per process (singleton).
  - p95 recall latency < 300ms with CE active
      → with a loaded model, scoring one CE batch (20 candidates) must be
        sub-300ms; verified against the REAL model when sentence-transformers
        is installed (graph extra / eval venv), skipped on the slim dev env.

sentence-transformers is not in the dev extra, so most tests inject a fake
module — that also keeps the contract pinned independent of network access.
"""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import time
import types
from pathlib import Path

import pytest

from reflect_kb.recall import cross_encoder as ce_mod
from reflect_kb.recall.cross_encoder import (
    DEFAULT_CE_MODEL,
    CrossEncoderReranker,
    cross_encoder_available,
    get_reranker,
    models_dir,
)

HAS_ST = importlib.util.find_spec("sentence_transformers") is not None


# --- fake sentence_transformers -------------------------------------------

class FakeCrossEncoder:
    """Stands in for sentence_transformers.CrossEncoder. Scores by text length
    so ordering assertions are deterministic."""

    instances: list["FakeCrossEncoder"] = []
    reject_cache_folder = False

    def __init__(self, model_name, **kwargs):
        if self.reject_cache_folder and "cache_folder" in kwargs:
            raise TypeError(
                "CrossEncoder.__init__() got an unexpected keyword argument "
                "'cache_folder'"
            )
        self.model_name = model_name
        self.kwargs = kwargs
        FakeCrossEncoder.instances.append(self)

    def predict(self, pairs, batch_size=32):
        self.last_batch_size = batch_size
        return [float(len(text)) for _query, text in pairs]


@pytest.fixture()
def fake_st(monkeypatch, tmp_path):
    """Install a fake sentence_transformers module + sandbox the cache env."""
    FakeCrossEncoder.instances = []
    FakeCrossEncoder.reject_cache_folder = False
    module = types.ModuleType("sentence_transformers")
    module.CrossEncoder = FakeCrossEncoder
    monkeypatch.setitem(sys.modules, "sentence_transformers", module)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.delenv("HF_HOME", raising=False)
    monkeypatch.delenv("SENTENCE_TRANSFORMERS_HOME", raising=False)
    monkeypatch.setattr(ce_mod, "_RERANKER", None)
    return module


# --- model cache / auto-download contract ----------------------------------

def test_models_dir_under_reflect_state(monkeypatch, tmp_path):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    assert models_dir() == tmp_path / "models"


def test_load_points_hf_caches_at_models_dir(fake_st, tmp_path):
    """Auto-download destination contract: the loader creates the models dir
    and points every HF cache env var at it BEFORE importing the library."""
    reranker = CrossEncoderReranker()
    reranker.score("q", ["candidate text"])
    expected = str(tmp_path / "state" / "models")
    assert Path(expected).is_dir()
    assert os.environ["HF_HOME"] == expected
    assert os.environ["SENTENCE_TRANSFORMERS_HOME"] == expected
    # newer builds also get the explicit kwarg
    assert FakeCrossEncoder.instances[0].kwargs.get("cache_folder") == expected


def test_user_hf_home_wins(fake_st, monkeypatch, tmp_path):
    """setdefault semantics: an explicit user-level HF_HOME is respected."""
    monkeypatch.setenv("HF_HOME", str(tmp_path / "custom"))
    CrossEncoderReranker().score("q", ["x"])
    assert os.environ["HF_HOME"] == str(tmp_path / "custom")


def test_cache_folder_kwarg_fallback_for_old_st(fake_st):
    """sentence-transformers <= 2.x rejects cache_folder — the loader must
    retry without it (HF_HOME env carries the cache dir instead)."""
    FakeCrossEncoder.reject_cache_folder = True
    scores = CrossEncoderReranker().score("q", ["abc"])
    assert scores == [3.0]
    assert "cache_folder" not in FakeCrossEncoder.instances[-1].kwargs


def test_model_loaded_once_across_calls(fake_st):
    """'cached thereafter': repeated scoring must not reconstruct the model."""
    reranker = get_reranker()
    reranker.score("q1", ["a", "bb"])
    reranker.score("q2", ["ccc"])
    get_reranker().score("q3", ["dddd"])
    assert len(FakeCrossEncoder.instances) == 1


def test_singleton_swaps_on_model_override(fake_st):
    first = get_reranker()
    same = get_reranker(model_name=DEFAULT_CE_MODEL)
    other = get_reranker(model_name="cross-encoder/other-model")
    assert first is same
    assert other is not first
    assert other.model_name == "cross-encoder/other-model"


# --- scoring contract -------------------------------------------------------

def test_score_returns_floats_in_input_order(fake_st):
    scores = CrossEncoderReranker().score("q", ["aa", "b", "cccc"])
    assert scores == [2.0, 1.0, 4.0]
    assert all(isinstance(s, float) for s in scores)


def test_score_empty_is_noop(fake_st):
    assert CrossEncoderReranker().score("q", []) == []
    assert FakeCrossEncoder.instances == []  # no pointless model load


def test_score_truncates_giant_candidates(fake_st):
    scores = CrossEncoderReranker().score("q", ["x" * 100_000])
    assert scores == [float(ce_mod._MAX_CANDIDATE_CHARS)]


def test_batch_size_passed_through(fake_st):
    reranker = CrossEncoderReranker(batch_size=20)
    reranker.score("q", ["a"] * 25)
    assert FakeCrossEncoder.instances[0].last_batch_size == 20


def test_available_matches_find_spec():
    assert cross_encoder_available() == HAS_ST


# --- `reflect rerank` CLI contract ------------------------------------------

@pytest.fixture()
def cli_env(monkeypatch, tmp_path):
    from reflect_kb import metrics as metrics_mod

    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(tmp_path / "repo"))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setattr(metrics_mod, "METRICS_PATH", tmp_path / "metrics.jsonl")
    from click.testing import CliRunner
    from reflect_kb.cli.learnings_cli import cli

    return CliRunner(), cli


def _payload(n=2):
    return json.dumps({
        "candidates": [{"id": f"doc-{i}", "text": "t" * (i + 1)} for i in range(n)]
    })


def test_cli_rerank_unavailable_degrades_silently(cli_env, monkeypatch):
    runner, cli = cli_env
    monkeypatch.setattr(ce_mod, "cross_encoder_available", lambda: False)
    result = runner.invoke(cli, ["rerank", "query"], input=_payload())
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["available"] is False


def test_cli_rerank_scores_by_id(cli_env, monkeypatch):
    runner, cli = cli_env

    class _Fake:
        model_name = "fake-model"

        def score(self, query, texts):
            return [float(len(t)) for t in texts]

    monkeypatch.setattr(ce_mod, "cross_encoder_available", lambda: True)
    monkeypatch.setattr(ce_mod, "get_reranker", lambda **kw: _Fake())
    result = runner.invoke(cli, ["rerank", "query"], input=_payload(3))
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["available"] is True
    assert data["model"] == "fake-model"
    assert data["scores"] == {"doc-0": 1.0, "doc-1": 2.0, "doc-2": 3.0}


def test_cli_rerank_invalid_stdin_is_nonfatal(cli_env):
    runner, cli = cli_env
    result = runner.invoke(cli, ["rerank", "query"], input="not json {{{")
    assert result.exit_code == 0
    assert json.loads(result.output)["available"] is False


def test_cli_rerank_model_failure_is_nonfatal(cli_env, monkeypatch):
    runner, cli = cli_env

    class _Broken:
        model_name = "broken"

        def score(self, query, texts):
            raise RuntimeError("download failed")

    monkeypatch.setattr(ce_mod, "cross_encoder_available", lambda: True)
    monkeypatch.setattr(ce_mod, "get_reranker", lambda **kw: _Broken())
    result = runner.invoke(cli, ["rerank", "query"], input=_payload())
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["available"] is False
    assert "download failed" in data["error"]


# --- acceptance: scoring latency with the REAL model ------------------------

@pytest.mark.skipif(not HAS_ST, reason="sentence-transformers not installed (slim build)")
def test_real_model_batch_p95_under_300ms():
    """With the model loaded, one CE batch (20 candidates) must stay well
    under the 300ms latency budget (bead: ~50ms per call on CPU)."""
    reranker = get_reranker()
    texts = [f"learning {i}: redis connection pool exhausted under load" for i in range(20)]
    reranker.score("warmup", texts)  # first-inference warmup excluded
    samples = []
    for _ in range(5):
        start = time.monotonic()
        reranker.score("redis pool exhaustion", texts)
        samples.append((time.monotonic() - start) * 1000)
    p95 = sorted(samples)[int(0.95 * (len(samples) - 1))]
    assert p95 < 300, f"p95 {p95:.1f}ms (samples: {[round(s) for s in samples]})"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
