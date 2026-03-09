"""Heavy tests for GraphRAG: insert + search with real sentence-transformers.

Requires sentence-transformers (~420MB model). Skipped in CI by default.
Run explicitly with: pytest -m heavy

Each test creates a fresh GraphRAG cache in a temporary directory.
"""

import pytest

# ---------------------------------------------------------------------------
# Module-level guard: skip entire file when sentence-transformers is absent
# ---------------------------------------------------------------------------

def _has_sentence_transformers() -> bool:
    try:
        import sentence_transformers  # noqa: F401
        return True
    except ImportError:
        return False


pytestmark = [
    pytest.mark.heavy,
    pytest.mark.skipif(
        not _has_sentence_transformers(),
        reason="sentence-transformers not installed",
    ),
]

# Install the graspologic shim once per session (idempotent).
from graspologic_shim import install_shim
install_shim()


# ---------------------------------------------------------------------------
# Sample data
# ---------------------------------------------------------------------------

SAMPLE_LEARNING = """\
---
title: Python asyncio event loop internals
tags: [python, asyncio, concurrency]
confidence: high
created: 2025-06-15
---

# Python asyncio event loop internals

The default asyncio event loop in CPython uses selectors (epoll on Linux,
kqueue on macOS) under the hood.  `asyncio.run()` creates a new event loop,
runs the coroutine, and closes the loop.  Using `loop.run_in_executor()` is
the standard way to offload blocking I/O to a thread pool without stalling
the event loop.

Key takeaway: never call blocking functions directly inside an async
coroutine -- always delegate to an executor or use an async-native library.
"""

SAMPLE_LEARNING_2 = """\
---
title: Redis caching strategies for microservices
tags: [redis, caching, microservices, performance]
confidence: high
created: 2025-07-20
---

# Redis caching strategies for microservices

Write-through caching with Redis ensures that every database write also
updates the cache, keeping data consistent at the cost of slightly higher
write latency.  Cache-aside (lazy loading) only populates the cache on
read misses, trading consistency for lower write overhead.

TTL-based expiration prevents stale data from lingering.  A common pattern
is to set a 5-minute TTL for user session data and a 1-hour TTL for
reference data that changes infrequently.
"""

SIDECAR_ENTITIES_YAML = """\
document_id: redis-caching-strategies
extracted_at: "2025-07-20T10:00:00"
entities:
  - name: Redis
    type: technology
    description: In-memory data store used for caching in microservices
  - name: Write-through caching
    type: pattern
    description: Caching strategy that updates cache on every database write
  - name: Cache-aside
    type: pattern
    description: Lazy loading caching strategy that populates cache on read misses
  - name: TTL-based expiration
    type: pattern
    description: Time-to-live mechanism to prevent stale cached data
relationships:
  - source: Redis
    target: Write-through caching
    type: requires
    description: Redis implements write-through caching for consistency
    strength: 8
  - source: Redis
    target: Cache-aside
    type: requires
    description: Redis implements cache-aside for low write overhead
    strength: 8
  - source: TTL-based expiration
    target: Redis
    type: relates_to
    description: TTL is commonly configured in Redis to expire stale entries
    strength: 7
"""


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def graph_engine(tmp_path):
    """Create a LearningsGraphEngine backed by a temporary cache directory."""
    from graph_engine import LearningsGraphEngine

    cache_dir = tmp_path / "nano_graphrag_cache"
    cache_dir.mkdir()
    return LearningsGraphEngine(cache_dir=cache_dir)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestInsertDocument:
    """Insert a plain document without errors."""

    def test_insert_completes_without_error(self, graph_engine):
        """A single insert_document call must succeed and leave engine usable."""
        graph_engine.insert_document(SAMPLE_LEARNING)

        stats = graph_engine.get_stats()
        assert stats["cache_exists"] is True


class TestInsertWithEntitySidecar:
    """Insert with pre-extracted entities from sidecar."""

    def test_insert_with_sidecar_entities(self, graph_engine):
        """Sidecar entities fed through passthrough LLM and stored in graph."""
        from entity_store import DocumentEntities

        doc_entities = DocumentEntities.from_yaml(SIDECAR_ENTITIES_YAML)
        entities_formatted = doc_entities.to_graphrag_format()

        graph_engine.insert_document(
            SAMPLE_LEARNING_2,
            entities_formatted=entities_formatted,
        )

        stats = graph_engine.get_stats()
        assert stats["cache_exists"] is True
        assert stats["entity_count"] >= 1


class TestNaiveSearch:
    """Naive (vector-only) search returns results after insert."""

    def test_naive_search_returns_results(self, graph_engine):
        """After insert, naive search for a topic returns non-empty context."""
        graph_engine.insert_document(SAMPLE_LEARNING)

        result = graph_engine.search("asyncio event loop", mode="naive")

        assert isinstance(result, str)
        assert len(result) > 0, "Naive search returned empty string"


class TestGetStats:
    """get_stats reflects cache state after insert."""

    def test_stats_after_insert(self, graph_engine):
        """Stats show cache_exists: True and entity_count >= 1 after insert."""
        graph_engine.insert_document(SAMPLE_LEARNING)

        stats = graph_engine.get_stats()

        assert stats["cache_exists"] is True
        assert "cache_dir" in stats
        assert "entity_count" in stats
        assert "relationship_count" in stats
        assert stats["entity_count"] >= 1


class TestMultiDocInsert:
    """Insert 2 docs, search finds both."""

    def test_insert_two_docs_search_finds_both(self, graph_engine):
        """Multi-doc batch indexing works, both docs are retrievable."""
        graph_engine.insert_documents_batch([
            (SAMPLE_LEARNING, None),
            (SAMPLE_LEARNING_2, None),
        ])

        # Search for topic from doc 1
        result1 = graph_engine.search("asyncio event loop", mode="naive")
        assert len(result1) > 0, "Failed to find doc 1 content"

        # Search for topic from doc 2
        result2 = graph_engine.search("Redis caching strategies", mode="naive")
        assert len(result2) > 0, "Failed to find doc 2 content"


class TestSearchNoResults:
    """Search with no matching results returns graceful empty."""

    def test_search_empty_graceful(self, graph_engine):
        """Search on empty graph returns empty string, no crash."""
        # Don't insert anything — search on empty graph
        result = graph_engine.search("quantum computing algorithms", mode="naive")
        assert isinstance(result, str)
        # Should be empty or very short (no crash)
