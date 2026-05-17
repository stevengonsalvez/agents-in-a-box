"""GraphRAG engine wrapping nano-graphrag for Global Learnings.

Uses all-mpnet-base-v2 for local embeddings (no API key needed) and a
passthrough LLM that returns pre-extracted entities from sidecar files
instead of calling an external API.

For search queries, uses only_need_context=True so the calling Claude
session can synthesize results itself.
"""

import logging
import shutil
from collections import deque
from pathlib import Path
from typing import Optional, List, Tuple

# Install graspologic shim BEFORE any nano-graphrag import.
# This avoids the broken transitive dependency chain:
# graspologic -> hyppo -> numba -> llvmlite (Python <3.10 only)
from reflect_kb.cli.graspologic_shim import install_shim as _install_graspologic_shim
_install_graspologic_shim()

from reflect_kb.cli.entity_store import COMPLETION_DELIMITER

logger = logging.getLogger(__name__)

# Minimal placeholder entity for docs without sidecars.
# Ensures nano-graphrag's insert() doesn't abort before persisting
# full_docs and text_chunks (which happens when extraction returns None).
_PLACEHOLDER_ENTITY = (
    '("entity"<|>"knowledge_entry"<|>"learning"'
    '<|>"A knowledge base document entry")\n'
    f"{COMPLETION_DELIMITER}"
)


class GraphEngineError(Exception):
    pass


class LearningsGraphEngine:
    """Wrapper around nano-graphrag for the Global Learnings knowledge base."""

    def __init__(self, cache_dir: str | Path):
        self._cache_dir = Path(cache_dir)
        self._cache_dir.mkdir(parents=True, exist_ok=True)
        self._graph = None
        self._model = None
        self._pending_entities: Optional[str] = None
        self._entity_queue: deque = deque()

    def _load_embedding_model(self):
        """Lazy-load the sentence transformer model."""
        if self._model is None:
            try:
                from sentence_transformers import SentenceTransformer

                self._model = SentenceTransformer("all-mpnet-base-v2")
            except ImportError:
                raise GraphEngineError(
                    "sentence-transformers not installed. "
                    "Run: uv pip install sentence-transformers"
                )
        return self._model

    def _get_embedding_func(self):
        """Create nano-graphrag compatible async embedding function."""
        import numpy as np
        from nano_graphrag._utils import wrap_embedding_func_with_attrs

        engine = self

        @wrap_embedding_func_with_attrs(embedding_dim=768, max_token_size=8192)
        async def embedding_func(texts: list[str]) -> np.ndarray:
            model = engine._load_embedding_model()
            embeddings = model.encode(texts, normalize_embeddings=True)
            return np.array(embeddings)

        return embedding_func

    def _is_entity_extraction_prompt(self, prompt: str) -> bool:
        """Detect nano-graphrag's entity extraction prompt."""
        lower = prompt[:200].lower()
        return "-goal-" in lower and "text document" in lower

    async def _llm_complete(
        self,
        prompt: str,
        system_prompt: Optional[str] = None,
        history_messages: list = [],
        **kwargs,
    ) -> str:
        """Passthrough LLM function for nano-graphrag.

        Routes different LLM call types:
        1. Entity extraction: returns pre-extracted entities (from sidecar
           queue or single pending, with placeholder fallback)
        2. Community reports: returns a minimal valid JSON report
        3. Other calls: returns empty/minimal response

        For queries with only_need_context=True, the LLM is not called
        for answer synthesis, so this fallback is rarely hit.
        """
        # Pop cache KV if present (nano-graphrag convention)
        kwargs.pop("hashing_kv", None)

        # Entity extraction calls
        if self._is_entity_extraction_prompt(prompt):
            # Batch mode: pop from queue (reindex uses this)
            if self._entity_queue:
                entities = self._entity_queue.popleft()
                return entities if entities else _PLACEHOLDER_ENTITY

            # Single mode: consume pending (add command uses this)
            if self._pending_entities is not None:
                entities = self._pending_entities
                self._pending_entities = None
                return entities

            # No entities available - return placeholder so insert completes
            return _PLACEHOLDER_ENTITY

        # Community report calls - return minimal valid JSON
        prompt_lower = prompt.lower() if prompt else ""
        if "community" in prompt_lower or "report" in prompt_lower:
            import json
            return json.dumps({
                "title": "Community Summary",
                "summary": "A group of related technical concepts and patterns.",
                "findings": [
                    {
                        "summary": "Related technical entities",
                        "explanation": "These entities are connected through technical relationships in the knowledge base."
                    }
                ],
                "rating": 5.0,
                "rating_explanation": "Moderate impact technical knowledge."
            })

        # Fallback for any other LLM calls
        return "No additional information available."

    def _init_graph(self):
        """Initialize the GraphRAG instance (lazy)."""
        if self._graph is not None:
            return

        try:
            from nano_graphrag import GraphRAG
        except ImportError:
            raise GraphEngineError(
                "nano-graphrag not installed. "
                "Run: uv pip install nano-graphrag"
            )

        self._graph = GraphRAG(
            working_dir=str(self._cache_dir),
            embedding_func=self._get_embedding_func(),
            best_model_func=self._llm_complete,
            cheap_model_func=self._llm_complete,
            enable_naive_rag=True,
        )

    def insert_document(self, text: str, entities_formatted: Optional[str] = None):
        """Insert a single document into the graph.

        Args:
            text: The document text content.
            entities_formatted: Pre-extracted entities in nano-graphrag format.
                If provided, the passthrough LLM returns these instead of
                calling an external API.
        """
        self._init_graph()
        self._pending_entities = entities_formatted
        try:
            self._graph.insert(text)
        finally:
            self._pending_entities = None
            self._entity_queue.clear()

    def insert_documents_batch(
        self,
        docs_with_entities: List[Tuple[str, Optional[str]]],
    ):
        """Insert multiple documents in a single batch.

        Batching avoids nano-graphrag state issues that occur with
        sequential insert() calls (community_reports dropped, early
        return skipping KV persistence).

        Args:
            docs_with_entities: List of (text, entities_formatted) tuples.
                entities_formatted can be None for docs without sidecars.
        """
        if not docs_with_entities:
            return

        self._init_graph()

        # Build entity queue - one entry per document, in order.
        # nano-graphrag processes chunks in document order, so the
        # passthrough LLM pops from this queue on each extraction call.
        self._entity_queue = deque(
            entities for _, entities in docs_with_entities
        )

        texts = [text for text, _ in docs_with_entities]

        try:
            self._graph.insert(texts)
        finally:
            self._entity_queue.clear()
            self._pending_entities = None

    def search(
        self,
        query: str,
        mode: str = "local",
        only_context: bool = True,
    ) -> str:
        """Search the graph for relevant context.

        Args:
            query: The search query.
            mode: Search mode - "naive" (vector only), "local" (entity
                  neighborhood), or "global" (community reports).
            only_context: If True, returns raw context without LLM synthesis.
                         Default True since Claude synthesizes results.

        Returns:
            Search results as a string.
        """
        self._init_graph()

        from nano_graphrag import QueryParam

        param = QueryParam(mode=mode, only_need_context=only_context)
        result = self._graph.query(query, param=param)
        return result if result else ""

    def clear_cache(self):
        """Clear the graph cache for full rebuild."""
        if self._cache_dir.exists():
            shutil.rmtree(self._cache_dir)
            self._cache_dir.mkdir(parents=True, exist_ok=True)
        self._graph = None

    def get_stats(self) -> dict:
        """Get graph statistics."""
        stats = {
            "cache_dir": str(self._cache_dir),
            "cache_exists": self._cache_dir.exists(),
            "entity_count": 0,
            "relationship_count": 0,
        }

        if not self._cache_dir.exists():
            return stats

        # Try to get graph-level stats from the stored graph
        graph_file = self._cache_dir / "graph_chunk_entity_relation.graphml"
        if graph_file.exists():
            try:
                import networkx as nx

                G = nx.read_graphml(str(graph_file))
                stats["entity_count"] = G.number_of_nodes()
                stats["relationship_count"] = G.number_of_edges()
            except Exception as e:
                logger.debug(f"Could not read graph stats: {e}")

        return stats
