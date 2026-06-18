# ABOUTME: Proves reflect's recall-layer arms are backend-agnostic — they call
# ABOUTME: `reflect search` as a subprocess and never import/reference the PG
# ABOUTME: backend, so 56 of the 57 ports can't be affected by the storage swap.

from __future__ import annotations

import pathlib

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]

# The recall layer (the home of the 4.1.0 ports). Skipped if absent (e.g. when
# ainb-reflect-memory is extracted to its own repo without the reflect plugin).
_RECALL_FILES = [
    _ROOT / "reflect-kb" / "src" / "reflect_kb" / "recall" / "recall.py",
    _ROOT / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py",
]

# References that would mean the recall arm is coupled to the storage backend.
_FORBIDDEN = [
    "REFLECT_PG",
    "ainb_reflect_memory",
    "PgGraphStorage",
    "PgVectorStorage",
    "PgKVStorage",
    "pgvector",
    "psycopg",
]


@pytest.mark.parametrize("path", _RECALL_FILES, ids=lambda p: p.name + ":" + p.parent.parent.name)
def test_recall_layer_has_no_backend_coupling(path: pathlib.Path) -> None:
    if not path.exists():
        pytest.skip(f"{path} not present in this checkout")
    src = path.read_text()
    for token in _FORBIDDEN:
        assert token not in src, (
            f"{path} references `{token}` — recall arms must stay backend-agnostic "
            "(they invoke `reflect search` as a subprocess; only graph_engine.py "
            "knows about the PG backend)."
        )
