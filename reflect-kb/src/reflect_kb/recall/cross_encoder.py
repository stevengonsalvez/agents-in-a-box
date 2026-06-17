"""Cross-encoder reranking for recall (port R2, Hindsight-inspired).

Bi-encoder cosine and BM25 match SURFACE features of query and candidate
independently. A cross-encoder reads the (query, candidate) pair jointly
and scores actual semantic relevance — this is where reranked recall feels
qualitatively sharper.

Clean-room reimplementation of the idea behind Hindsight's
CrossEncoderReranker (ELv2 — no code copied). Default model is
``cross-encoder/ms-marco-MiniLM-L-6-v2`` (~90MB), auto-downloaded on first
use and persisted under ``~/.reflect/models/`` so subsequent loads are
local-only. The model is loaded ONCE per process (module singleton);
scoring ~20 candidates takes ~50ms on CPU once loaded.

``sentence_transformers`` ships in the ``[graph]`` extra only — the slim
build doesn't have it. Callers must check :func:`cross_encoder_available`
(or catch the resulting failure) and degrade silently: the cross-encoder
is a booster, never a blocker.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
from typing import Sequence

DEFAULT_CE_MODEL = os.environ.get(
    "REFLECT_CE_MODEL", "cross-encoder/ms-marco-MiniLM-L-6-v2"
)
DEFAULT_BATCH_SIZE = 20
# CE models truncate at 512 tokens anyway; cap the payload so giant chunks
# don't waste tokenizer time.
_MAX_CANDIDATE_CHARS = 2000


def models_dir() -> Path:
    """Persistent model cache: ``$REFLECT_STATE_DIR/models`` (default
    ``~/.reflect/models``)."""
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / "models"


def cross_encoder_available() -> bool:
    """True when sentence-transformers is importable (graph extra installed)."""
    try:
        return importlib.util.find_spec("sentence_transformers") is not None
    except (ImportError, ValueError):
        return False


class CrossEncoderReranker:
    """Lazy-loading wrapper around ``sentence_transformers.CrossEncoder``.

    The model is constructed on the first :meth:`score` call and reused for
    the lifetime of the instance — model load (~2s warm, longer on first
    download) is paid once per process, not per scoring call.
    """

    def __init__(
        self,
        model_name: str = DEFAULT_CE_MODEL,
        batch_size: int = DEFAULT_BATCH_SIZE,
    ) -> None:
        self.model_name = model_name
        self.batch_size = batch_size
        self._model = None

    def _load(self):
        if self._model is None:
            cache = models_dir()
            cache.mkdir(parents=True, exist_ok=True)
            # transformers/huggingface_hub compute their cache constants at
            # IMPORT time, so the env vars must be in place before the
            # sentence_transformers import below. setdefault: an explicit
            # user-level HF_HOME wins over our default.
            os.environ.setdefault("HF_HOME", str(cache))
            os.environ.setdefault("SENTENCE_TRANSFORMERS_HOME", str(cache))
            os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")
            from sentence_transformers import CrossEncoder

            try:
                # Newer sentence-transformers accept an explicit cache dir.
                self._model = CrossEncoder(
                    self.model_name, cache_folder=str(cache)
                )
            except TypeError:
                # Older builds (<=2.x) don't take cache_folder — they fall
                # back to the HF_HOME / SENTENCE_TRANSFORMERS_HOME set above.
                self._model = CrossEncoder(self.model_name)
        return self._model

    def score(self, query: str, texts: Sequence[str]) -> list[float]:
        """Raw relevance logits for each (query, text) pair, in input order.

        ms-marco models emit unbounded logits (≈ -12 … +12); callers
        normalise (e.g. sigmoid) as needed.
        """
        if not texts:
            return []
        model = self._load()
        pairs = [(query, text[:_MAX_CANDIDATE_CHARS]) for text in texts]
        return [float(s) for s in model.predict(pairs, batch_size=self.batch_size)]


_RERANKER: CrossEncoderReranker | None = None


def get_reranker(
    model_name: str | None = None, batch_size: int | None = None
) -> CrossEncoderReranker:
    """Process-wide singleton so the model loads exactly once per process.

    A different ``model_name`` replaces the singleton (rare — only when the
    caller overrides the default model)."""
    global _RERANKER
    wanted = model_name or DEFAULT_CE_MODEL
    if _RERANKER is None or _RERANKER.model_name != wanted:
        _RERANKER = CrossEncoderReranker(
            wanted, batch_size or DEFAULT_BATCH_SIZE
        )
    elif batch_size:
        _RERANKER.batch_size = batch_size
    return _RERANKER
