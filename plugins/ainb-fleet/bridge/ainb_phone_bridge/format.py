# ABOUTME: Markdown -> Telegram-HTML conversion and 4096-char message splitting.
#
# Both functions are pure (stdlib `html` only) and ported from agent-deck's
# `md_to_tg_html` / `split_message`. Telegram's HTML parse mode supports a small
# tag subset; we map the common Markdown constructs an agent emits and escape
# everything else so a stray '<' never breaks the message.

from __future__ import annotations

import html
import re

from . import TG_MAX_LENGTH

# Order matters: fenced code blocks first (greedy, multiline), then inline spans.
_FENCE_RE = re.compile(r"```[a-zA-Z0-9_+-]*\n?(.*?)```", re.DOTALL)
_INLINE_CODE_RE = re.compile(r"`([^`\n]+?)`")
_BOLD_RE = re.compile(r"\*\*([^*]+?)\*\*")
_ITALIC_RE = re.compile(r"(?<![*\w])\*([^*\n]+?)\*(?![*\w])")
_LINK_RE = re.compile(r"\[([^\]]+)\]\((https?://[^)\s]+)\)")


def md_to_tg_html(text: str) -> str:
    """Convert a Markdown-ish string to the Telegram HTML subset.

    Strategy: pull code spans out behind placeholders so their contents are
    never re-interpreted as markup, HTML-escape the remainder, apply the
    inline conversions, then restore the (escaped) code spans wrapped in
    ``<pre>`` / ``<code>``.
    """
    if not text:
        return ""

    placeholders: list[str] = []

    def _stash(rendered: str) -> str:
        token = f"\x00PH{len(placeholders)}\x00"
        placeholders.append(rendered)
        return token

    # Fenced blocks -> <pre><code>…</code></pre>
    def _fence_sub(m: re.Match[str]) -> str:
        inner = html.escape(m.group(1).rstrip("\n"))
        return _stash(f"<pre><code>{inner}</code></pre>")

    text = _FENCE_RE.sub(_fence_sub, text)

    # Inline `code` -> <code>…</code>
    def _inline_sub(m: re.Match[str]) -> str:
        return _stash(f"<code>{html.escape(m.group(1))}</code>")

    text = _INLINE_CODE_RE.sub(_inline_sub, text)

    # Escape the prose body (placeholders survive — they contain no '<').
    text = html.escape(text)

    # Links: [label](url) -> <a href="url">label</a>
    text = _LINK_RE.sub(lambda m: f'<a href="{html.escape(m.group(2))}">{m.group(1)}</a>', text)
    # Bold / italic.
    text = _BOLD_RE.sub(r"<b>\1</b>", text)
    text = _ITALIC_RE.sub(r"<i>\1</i>", text)

    # Restore code placeholders.
    for i, rendered in enumerate(placeholders):
        text = text.replace(f"\x00PH{i}\x00", rendered)

    return text


def split_message(text: str, limit: int = TG_MAX_LENGTH) -> list[str]:
    """Split ``text`` into chunks no longer than ``limit`` characters.

    Prefers newline boundaries; falls back to a hard character cut for any
    single line that is itself longer than the limit. Never returns an empty
    list — an empty input yields ``[""]`` so callers can always ``send`` once.
    """
    if len(text) <= limit:
        return [text]

    chunks: list[str] = []
    current = ""
    for line in text.split("\n"):
        # A single oversized line: flush, then hard-cut it.
        if len(line) > limit:
            if current:
                chunks.append(current)
                current = ""
            for i in range(0, len(line), limit):
                chunks.append(line[i : i + limit])
            continue

        candidate = line if not current else f"{current}\n{line}"
        if len(candidate) > limit:
            chunks.append(current)
            current = line
        else:
            current = candidate

    if current:
        chunks.append(current)
    return chunks or [""]
