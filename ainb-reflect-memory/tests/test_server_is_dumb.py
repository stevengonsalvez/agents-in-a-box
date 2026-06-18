# ABOUTME: Proves the Postgres storage adapters do NO LLM/embedding work.
# ABOUTME: Pure source scan — no DB, no nano_graphrag import — so it always runs.

from __future__ import annotations

import pathlib

import pytest

# The nano-graphrag storage adapters: the "server-side" persistence layer.
_PKG = (
    pathlib.Path(__file__).resolve().parent.parent / "src" / "ainb_reflect_memory" / "nanographrag"
)
_ADAPTER_FILES = ["_conn.py", "kv.py", "vectors.py", "graph.py", "__init__.py"]

# Provider / model imports that would mean the adapter itself is doing LLM or
# embedding work. The embedding function is INJECTED by nano-graphrag; the
# adapter must never construct its own.
_FORBIDDEN_IMPORTS = [
    "import openai",
    "from openai",
    "import anthropic",
    "from anthropic",
    "import cohere",
    "import google.generativeai",
    "import sentence_transformers",
    "from sentence_transformers",
    "SentenceTransformer",
    "import torch",
    "import tiktoken",
    "AutoModel",
]


@pytest.mark.parametrize("fname", _ADAPTER_FILES)
def test_adapter_source_has_no_llm_or_embedding_imports(fname: str) -> None:
    src = (_PKG / fname).read_text()
    for token in _FORBIDDEN_IMPORTS:
        assert token not in src, f"{fname} must not contain `{token}` (server stays dumb)"


def test_vector_adapter_uses_only_injected_embedding_func() -> None:
    """PgVectorStorage must embed via the injected embedding_func, never build
    its own model — so embedding stays client-side and the DB only stores/ANNs."""
    src = (_PKG / "vectors.py").read_text()
    # It calls the injected function...
    assert "self.embedding_func(" in src
    # ...and never instantiates an embedding model or calls a provider API.
    for bad in ("SentenceTransformer(", ".embeddings.create(", ".encode(", "load_model"):
        assert bad not in src, f"vectors.py must not contain `{bad}`"


def test_no_provider_env_vars_referenced() -> None:
    """No adapter should read an LLM/embedding provider key from the env."""
    for fname in _ADAPTER_FILES:
        src = (_PKG / fname).read_text()
        for key in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "COHERE_API_KEY"):
            assert key not in src, f"{fname} must not reference {key}"
