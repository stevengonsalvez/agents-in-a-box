#!/usr/bin/env python3
# ABOUTME: Privacy tag stripping at the LLM-prompt boundary (port M6, from claude-mem).
# ABOUTME: Removes <private>…</private> spans + canonical strip-list tags before any text reaches an LLM payload.
"""Privacy filter for LLM-bound payloads.

Port M6 (source: claude-mem `src/utils/tag-stripping.ts` + PrivacyCheckValidator).

Two layers:

1. **<private> spans** — anything a user (or tool output) wraps in
   ``<private>…</private>`` is removed wholesale, replaced with a
   ``[private content removed]`` marker so downstream readers know an
   elision happened (an invisible elision can change the meaning of a
   correction).

2. **Canonical strip-list tags** — harness/system wrapper tags whose
   *content* is machine context that should never be quoted back into an
   LLM prompt: ``claude-mem-context``, ``system-reminder``,
   ``system_instruction``, ``persisted-output``, ``reflect-private``.

Both layers tolerate malformed input: unclosed tags strip to end-of-text
(fail closed — better to over-strip than leak), nested same-name tags are
consumed greedily, and tag names are case-insensitive.

Usage::

    from privacy_filter import strip_private
    safe = strip_private(raw_text)

The function is pure and dependency-free so every hook can import it
without dragging in the rest of the plugin.
"""
from __future__ import annotations

import re

__all__ = ["strip_private", "PRIVATE_MARKER", "STRIP_TAGS"]

PRIVATE_MARKER = "[private content removed]"

# Tags whose content must never reach an LLM payload. <private> is the
# user-facing one; the rest are machine-context wrappers (claude-mem's
# canonical list, plus reflect's own).
STRIP_TAGS = (
    "private",
    "claude-mem-context",
    "system-reminder",
    "system_instruction",
    "persisted-output",
    "reflect-private",
)


def _tag_pattern(tag: str) -> re.Pattern[str]:
    # Closed pair (non-greedy) OR unclosed-to-EOF (fail closed).
    return re.compile(
        rf"<{re.escape(tag)}(?:\s[^>]*)?>.*?</{re.escape(tag)}\s*>"
        rf"|<{re.escape(tag)}(?:\s[^>]*)?>.*\Z",
        re.IGNORECASE | re.DOTALL,
    )


_PATTERNS = [(_tag_pattern(t), t) for t in STRIP_TAGS]


def strip_private(text: str) -> str:
    """Remove all strip-list tag spans from ``text``.

    ``<private>`` spans are replaced with :data:`PRIVATE_MARKER`; machine
    wrapper tags are removed silently (their absence isn't meaningful).
    """
    if not text or "<" not in text:
        return text
    out = text
    for pattern, tag in _PATTERNS:
        replacement = PRIVATE_MARKER if tag == "private" else ""
        # Loop: removing one span can join text that forms no new tags, but a
        # document may contain many disjoint spans of the same tag.
        out = pattern.sub(replacement, out)
    return out
