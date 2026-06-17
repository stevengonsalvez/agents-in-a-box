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
removed as one balanced span (depth-aware so the OUTER close is honoured,
never an inner one), disjoint spans of the same tag are each removed, and
tag names are case-insensitive.

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


def _tag_res(tag: str) -> tuple[re.Pattern[str], re.Pattern[str]]:
    """Compiled (open, close) matchers for one strip tag."""
    open_re = re.compile(rf"<{re.escape(tag)}(?:\s[^>]*)?>", re.IGNORECASE | re.DOTALL)
    close_re = re.compile(rf"</{re.escape(tag)}\s*>", re.IGNORECASE)
    return open_re, close_re


_TAG_RES = [(t, *_tag_res(t)) for t in STRIP_TAGS]


def _strip_tag(text: str, open_re: re.Pattern[str], close_re: re.Pattern[str],
               replacement: str) -> str:
    """Remove every balanced ``<tag>…</tag>`` span, depth-aware.

    A naive non-greedy ``.*?</tag>`` closes at the FIRST close tag, so on a
    nested span it strips only up to the inner close and LEAKS the outer tail
    (the exact failure this replaces). Instead, for each opening tag we walk
    forward counting nested opens vs closes until depth returns to zero — that
    is the matching OUTER close — and remove the whole span. An opening tag
    with no matching close strips to end-of-text (fail closed). Disjoint spans
    are each handled by the outer loop.
    """
    while True:
        m_open = open_re.search(text)
        if not m_open:
            return text
        depth = 1
        pos = m_open.end()
        end = None
        while depth > 0:
            nxt_open = open_re.search(text, pos)
            nxt_close = close_re.search(text, pos)
            if nxt_close is None:
                # Unclosed → strip from the open to EOF (fail closed).
                end = len(text)
                break
            if nxt_open is not None and nxt_open.start() < nxt_close.start():
                depth += 1
                pos = nxt_open.end()
            else:
                depth -= 1
                pos = nxt_close.end()
                if depth == 0:
                    end = pos
        text = text[:m_open.start()] + replacement + text[end:]


def strip_private(text: str) -> str:
    """Remove all strip-list tag spans from ``text``.

    ``<private>`` spans are replaced with :data:`PRIVATE_MARKER`; machine
    wrapper tags are removed silently (their absence isn't meaningful).
    """
    if not text or "<" not in text:
        return text
    out = text
    for tag, open_re, close_re in _TAG_RES:
        replacement = PRIVATE_MARKER if tag == "private" else ""
        out = _strip_tag(out, open_re, close_re, replacement)
    return out
