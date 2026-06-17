# ABOUTME: Regression tests for port R3 — engine-side `reflect embed` +
# ABOUTME: LearningsGraphEngine.embed_texts (mpnet vectors for recall's MMR
# ABOUTME: diversity step; silent degrade on slim builds).
"""Port R3: MMR diversity step, engine side.

The plugin's mmr_select needs query + candidate vectors in the SAME
embedding space nano-graphrag indexes with. `reflect embed` exposes
LearningsGraphEngine.embed_texts (all-mpnet-base-v2, unit-normalized)
over the same stdin/stdout JSON contract as `reflect rerank`:

    stdin:  {"candidates": [{"id": "...", "text": "..."}]}
    stdout: {"available": true, "model": "all-mpnet-base-v2",
             "query_embedding": [...], "embeddings": {"<id>": [...]}}

On the slim build (no sentence-transformers) or any failure the command
emits {"available": false, ...} and exits 0 — callers degrade silently.

sentence-transformers is not in the dev extra, so tests inject a fake
module — that also keeps the contract pinned independent of network access.
"""

from __future__ import annotations

import importlib.util
import json
import sys
import types

import pytest

# graph_engine imports the graspologic shim, which needs networkx — a
# [graph]-extra dependency the slim dev env doesn't have. The shim only
# touches `networkx.Graph` at import time, so a stub keeps these tests
# runnable on the slim build (the real package wins when installed).
if importlib.util.find_spec("networkx") is None:  # pragma: no cover
    _nx_stub = types.ModuleType("networkx")
    _nx_stub.Graph = object
    sys.modules.setdefault("networkx", _nx_stub)

from reflect_kb.cli.graph_engine import (
    EMBEDDING_MODEL_NAME,
    GraphEngineError,
    LearningsGraphEngine,
    _MAX_EMBED_CHARS,
)


# --- fake sentence_transformers -------------------------------------------

class FakeSentenceTransformer:
    """Stands in for sentence_transformers.SentenceTransformer. Embeds by
    text length so order assertions are deterministic."""

    instances: list["FakeSentenceTransformer"] = []

    def __init__(self, model_name):
        self.model_name = model_name
        FakeSentenceTransformer.instances.append(self)

    def encode(self, texts, normalize_embeddings=False):
        self.last_texts = list(texts)
        self.last_normalize = normalize_embeddings
        return [[float(len(t)), 1.0] for t in texts]


@pytest.fixture()
def fake_st(monkeypatch):
    FakeSentenceTransformer.instances = []
    module = types.ModuleType("sentence_transformers")
    module.SentenceTransformer = FakeSentenceTransformer
    monkeypatch.setitem(sys.modules, "sentence_transformers", module)
    return module


# --- embed_texts contract ---------------------------------------------------

def test_embed_texts_returns_float_lists_in_order(fake_st, tmp_path):
    engine = LearningsGraphEngine(tmp_path / "cache")
    out = engine.embed_texts(["aa", "b"])
    assert out == [[2.0, 1.0], [1.0, 1.0]]
    assert all(isinstance(x, float) for vec in out for x in vec)


def test_embed_texts_unit_normalizes_with_index_model(fake_st, tmp_path):
    """Same model + normalization as indexing — dot product == cosine."""
    engine = LearningsGraphEngine(tmp_path / "cache")
    engine.embed_texts(["x"])
    model = FakeSentenceTransformer.instances[0]
    assert model.model_name == EMBEDDING_MODEL_NAME == "all-mpnet-base-v2"
    assert model.last_normalize is True


def test_embed_texts_empty_skips_model_load(fake_st, tmp_path):
    engine = LearningsGraphEngine(tmp_path / "cache")
    assert engine.embed_texts([]) == []
    assert FakeSentenceTransformer.instances == []  # no pointless model load


def test_embed_texts_caps_giant_inputs(fake_st, tmp_path):
    engine = LearningsGraphEngine(tmp_path / "cache")
    out = engine.embed_texts(["x" * 100_000])
    model = FakeSentenceTransformer.instances[0]
    assert len(model.last_texts[0]) == _MAX_EMBED_CHARS
    assert out[0][0] == float(_MAX_EMBED_CHARS)


def test_model_loaded_once_across_calls(fake_st, tmp_path):
    engine = LearningsGraphEngine(tmp_path / "cache")
    engine.embed_texts(["a"])
    engine.embed_texts(["b", "c"])
    assert len(FakeSentenceTransformer.instances) == 1


def test_embed_texts_slim_build_raises_graph_engine_error(monkeypatch, tmp_path):
    """No sentence-transformers => GraphEngineError (CLI degrades on it)."""
    monkeypatch.setitem(sys.modules, "sentence_transformers", None)
    engine = LearningsGraphEngine(tmp_path / "cache")
    with pytest.raises(GraphEngineError):
        engine.embed_texts(["x"])


# --- `reflect embed` CLI contract -------------------------------------------

class _FakeEngine:
    """embed_texts stub: vector encodes input position + an unrounded float
    so the CLI's 6-decimal rounding is observable."""

    def __init__(self, fail_with: Exception | None = None):
        self.fail_with = fail_with
        self.seen_texts: list[str] | None = None

    def embed_texts(self, texts):
        if self.fail_with is not None:
            raise self.fail_with
        self.seen_texts = list(texts)
        return [[float(i), 0.123456789] for i, _ in enumerate(texts)]


@pytest.fixture()
def cli_env(monkeypatch, tmp_path):
    from reflect_kb import metrics as metrics_mod

    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(tmp_path / "repo"))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setattr(metrics_mod, "METRICS_PATH", tmp_path / "metrics.jsonl")
    from click.testing import CliRunner
    from reflect_kb.cli import learnings_cli

    return CliRunner(), learnings_cli


def _payload(n=2):
    return json.dumps({
        "candidates": [{"id": f"doc-{i}", "text": "t" * (i + 1)} for i in range(n)]
    })


def test_cli_embed_contract(cli_env, monkeypatch):
    runner, lcli = cli_env
    engine = _FakeEngine()
    monkeypatch.setattr(lcli, "_get_graph_engine", lambda: engine)
    result = runner.invoke(lcli.cli, ["embed", "the query"], input=_payload(2))
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    assert data["available"] is True
    assert data["model"] == EMBEDDING_MODEL_NAME
    # query is embedded FIRST, candidates follow in input order
    assert engine.seen_texts == ["the query", "t", "tt"]
    assert data["query_embedding"] == [0.0, 0.123457]  # rounded to 6 decimals
    assert data["embeddings"] == {
        "doc-0": [1.0, 0.123457],
        "doc-1": [2.0, 0.123457],
    }


def test_cli_embed_slim_build_degrades_silently(cli_env, monkeypatch):
    runner, lcli = cli_env
    engine = _FakeEngine(fail_with=GraphEngineError("sentence-transformers not installed"))
    monkeypatch.setattr(lcli, "_get_graph_engine", lambda: engine)
    result = runner.invoke(lcli.cli, ["embed", "q"], input=_payload())
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["available"] is False
    assert "sentence-transformers" in data["error"]


def test_cli_embed_any_engine_failure_degrades(cli_env, monkeypatch):
    runner, lcli = cli_env
    engine = _FakeEngine(fail_with=RuntimeError("model exploded"))
    monkeypatch.setattr(lcli, "_get_graph_engine", lambda: engine)
    result = runner.invoke(lcli.cli, ["embed", "q"], input=_payload())
    assert result.exit_code == 0
    assert json.loads(result.output)["available"] is False


def test_cli_embed_invalid_stdin_is_nonfatal(cli_env):
    runner, lcli = cli_env
    result = runner.invoke(lcli.cli, ["embed", "q"], input="not json {{{")
    assert result.exit_code == 0
    assert json.loads(result.output)["available"] is False


def test_cli_embed_skips_malformed_candidates(cli_env, monkeypatch):
    runner, lcli = cli_env
    engine = _FakeEngine()
    monkeypatch.setattr(lcli, "_get_graph_engine", lambda: engine)
    payload = json.dumps({"candidates": [
        {"id": "good", "text": "x"},
        {"id": "no-text"},
        {"text": "no-id"},
        "junk",
        {"id": "bad-text", "text": 42},
    ]})
    result = runner.invoke(lcli.cli, ["embed", "q"], input=payload)
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["available"] is True
    assert set(data["embeddings"]) == {"good"}


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
