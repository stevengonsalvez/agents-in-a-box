"""Markdown -> Telegram HTML and 4096-char message splitting."""

from __future__ import annotations

from ainb_phone_bridge import TG_MAX_LENGTH
from ainb_phone_bridge.format import md_to_tg_html, split_message


def test_bold_and_italic():
    assert md_to_tg_html("**bold** and *italic*") == "<b>bold</b> and <i>italic</i>"


def test_inline_code_is_escaped():
    out = md_to_tg_html("call `f(<x>)` now")
    assert out == "call <code>f(&lt;x&gt;)</code> now"


def test_fenced_code_block():
    out = md_to_tg_html("```python\nprint(1)\n```")
    assert out == "<pre><code>print(1)</code></pre>"


def test_html_special_chars_escaped_in_prose():
    assert md_to_tg_html("a < b & c > d") == "a &lt; b &amp; c &gt; d"


def test_link_conversion():
    out = md_to_tg_html("see [docs](https://example.com/x)")
    assert out == 'see <a href="https://example.com/x">docs</a>'


def test_code_span_not_reinterpreted_as_bold():
    # The asterisks inside a code span must survive verbatim.
    out = md_to_tg_html("`a**b**c`")
    assert out == "<code>a**b**c</code>"


def test_empty_string():
    assert md_to_tg_html("") == ""


def test_split_short_message_single_chunk():
    assert split_message("hello") == ["hello"]


def test_split_empty_yields_one_empty_chunk():
    assert split_message("") == [""]


def test_split_at_newline_boundaries():
    line = "x" * 3000
    text = "\n".join([line, line])  # 6001 chars total
    chunks = split_message(text)
    assert len(chunks) == 2
    assert all(len(c) <= TG_MAX_LENGTH for c in chunks)
    # Re-joining with newlines reconstructs the original.
    assert "\n".join(chunks) == text


def test_split_hard_cuts_oversized_line():
    oversized = "y" * (TG_MAX_LENGTH * 2 + 10)
    chunks = split_message(oversized)
    assert all(len(c) <= TG_MAX_LENGTH for c in chunks)
    assert "".join(chunks) == oversized
    assert len(chunks) == 3


def test_split_exactly_at_limit_single_chunk():
    text = "z" * TG_MAX_LENGTH
    assert split_message(text) == [text]


def test_split_just_over_limit():
    text = "z" * (TG_MAX_LENGTH + 1)
    chunks = split_message(text)
    assert len(chunks) == 2
    assert all(len(c) <= TG_MAX_LENGTH for c in chunks)


def test_split_raw_before_convert_never_cuts_entities_or_tags():
    """Splitting the RAW reply BEFORE md_to_tg_html keeps every chunk valid.

    Regression: converting to HTML first then splitting at 4096 could slice an
    HTML entity (e.g. ``&amp;``) or a tag (``<b>``) mid-token, which Telegram
    rejects with a 400. Splitting the raw text first guarantees boundaries fall
    on raw characters, so each converted chunk is self-contained.
    """
    # A >4096 reply full of chars that become entities, plus bold spans.
    unit = "**bold** a < b & c > d "
    raw = unit * 400  # well over TG_MAX_LENGTH
    assert len(raw) > TG_MAX_LENGTH

    raw_chunks = split_message(raw)
    assert len(raw_chunks) >= 2

    converted = [md_to_tg_html(c) for c in raw_chunks]

    for html_chunk in converted:
        # No half-written entity: every '&' must start a complete entity.
        for amp in _amp_positions(html_chunk):
            assert _has_complete_entity_at(html_chunk, amp), (
                f"entity sliced at {amp}: {html_chunk[amp : amp + 8]!r}"
            )
        # Balanced bold tags within the chunk (no dangling <b> or </b>).
        assert html_chunk.count("<b>") == html_chunk.count("</b>")
        # No stray '<' that isn't part of a recognised tag start.
        assert "< " not in html_chunk


def _amp_positions(s: str) -> list[int]:
    return [i for i, ch in enumerate(s) if ch == "&"]


def _has_complete_entity_at(s: str, idx: int) -> bool:
    semi = s.find(";", idx, idx + 10)
    return semi != -1
