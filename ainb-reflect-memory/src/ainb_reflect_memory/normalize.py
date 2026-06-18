"""Content normalization + dedupe hashing.

The dedupe contract (mirrors the ``unique (workspace_id, content_hash)``
constraint in the migration): two memory items with the *same normalized
content* in the *same tenant* are the same item. Normalization is deliberately
conservative — it folds away only insignificant differences (surrounding
whitespace, internal whitespace runs, case, Unicode form) so that
"  Fixed   the BUG\n" and "fixed the bug" collapse to one row, while genuinely
different text stays distinct.

Hashing happens client-side (here), never in the database, keeping the server
dumb. The hash is plain SHA-256 hex — stable across processes and languages, so
a future non-Python client can reproduce it.
"""

from __future__ import annotations

import hashlib
import unicodedata

__all__ = ["normalize_content", "content_hash"]


def normalize_content(content: str) -> str:
    """Return a canonical form of ``content`` for dedupe comparison.

    Steps: NFC Unicode normalization → strip → lowercase → collapse internal
    whitespace runs (including newlines/tabs) to single spaces.
    """
    if not isinstance(content, str):  # defensive: callers may pass non-str
        raise TypeError(f"content must be str, got {type(content).__name__}")
    text = unicodedata.normalize("NFC", content)
    text = text.strip().lower()
    # Collapse any run of whitespace (spaces, tabs, newlines) to one space.
    return " ".join(text.split())


def content_hash(content: str) -> str:
    """Stable SHA-256 hex digest of the *normalized* content.

    Same normalized content → same hash, across runs and machines. This is the
    value stored in ``memory_items.content_hash`` and the dedupe key for
    idempotent inserts.
    """
    normalized = normalize_content(content)
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()
