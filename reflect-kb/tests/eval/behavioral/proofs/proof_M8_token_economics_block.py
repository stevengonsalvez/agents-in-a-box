# ABOUTME: Behavioral proof for M8 — every recall block surfaces a token-economics roll-up.
# ABOUTME: Knob RECALL_ECONOMICS toggles it: ON => envelope+rows carry economics; OFF => absent/null.
"""M8 token-economics surfacing proof.

Invariant (the heart of M8): every recall block is self-justifying about its
token ROI. The JSON envelope recall.py emits carries a block-total
``economics`` roll-up (claude-mem ``calculateTokenEconomics`` shape:
``read_tokens`` / ``discovery_tokens`` / ``saved_tokens`` / ``savings_pct`` /
``count``) and EACH result row carries its own ``economics`` object
(``discovery_tokens`` / ``read_tokens`` / ``savings_pct`` / ``glyph``). The
numbers are computed at injection time from the learning itself — discovery
cost from frontmatter/transcript/category-average fallback, read cost from the
active ≈4-chars/token estimator — so NO LLM participates in producing or
asserting them.

This is a PRESENCE-WITH-CONTROL proof. The decisive knob is the documented kill
switch ``RECALL_ECONOMICS`` (``recall.py`` line 296:
``ECONOMICS_ENABLED = os.environ.get("RECALL_ECONOMICS", "1") != "0"``):

  Arm ON  (default, RECALL_ECONOMICS unset): the envelope's ``economics`` is a
    non-null dict carrying the rolled-up keys, with ``read_tokens > 0`` and
    ``discovery_tokens > 0`` (every seed falls back to at least the category
    average / DEFAULT_DISCOVERY_TOKENS, so the roll-up is always positive), and
    every result row carries an ``economics`` object.

  Arm OFF (RECALL_ECONOMICS=0): the SAME seed + SAME query produces an envelope
    whose ``economics`` is exactly ``null`` and whose result rows carry NO
    ``economics`` key at all — byte-for-byte the pre-M8 shape.

Only the env flag differs between the two arms — same KB, same query, same
ranking — so the presence of the economics block is caused by the M8 port, not
by anything the text or the model decided. If the toggle were ignored (block
always present, or always absent) one of the two arms fails.

PORT: M8
"""
from __future__ import annotations

# A small handful of seeds on a coherent topic so the recall returns >=1 row
# with a stable, positive economics roll-up. The bodies are deliberately
# ordinary engineering notes — nothing here encodes economics; the numbers come
# from recall.py's M8 estimators, not from the seed text.
_SEEDS = [
    dict(
        name="m8-retry-503",
        title="Retry transient HTTP 503 with exponential backoff and jitter",
        category="reliability",
        tags=["http", "retry", "backoff"],
        confidence="medium",
        created="2026-05-01",
        archived="2026-05-10T00:00:00",
        key_insight="Back off exponentially with jitter on transient 503s instead of retrying immediately.",
        body=(
            "Transient HTTP 503 responses should be retried with exponential "
            "backoff and jitter rather than a tight immediate retry loop, so a "
            "struggling upstream is not stampeded by synchronized client retries."
        ),
    ),
    dict(
        name="m8-retry-timeout",
        title="Set sensible client timeouts before adding retries",
        category="reliability",
        tags=["http", "retry", "timeout"],
        confidence="medium",
        created="2026-05-02",
        archived="2026-05-11T00:00:00",
        key_insight="A bounded client timeout must precede any retry policy so retries are not stacked on hung calls.",
        body=(
            "Before adding a retry policy to an HTTP client, set a bounded "
            "connect and read timeout. Without it, retries stack on top of "
            "hung calls and amplify load against a failing upstream."
        ),
    ),
]

_QUERY = "how to retry transient HTTP 503 errors with exponential backoff"

# The block-total keys M8 promises in the JSON envelope (block_economics shape).
_BLOCK_KEYS = {"count", "read_tokens", "discovery_tokens", "saved_tokens", "savings_pct"}
# The per-result keys M8 promises (learning_economics shape).
_ROW_KEYS = {"discovery_tokens", "read_tokens", "savings_pct", "glyph"}


def test_M8_economics_block_present_when_enabled(behavioral_kb):
    """Arm ON: with economics enabled (default), the recall JSON envelope
    carries a non-null block-total economics roll-up AND every result row
    carries its own economics object — both with positive token counts."""
    kb = behavioral_kb
    kb.seed(_SEEDS)

    payload = kb.recall(_QUERY, no_mmr=True)
    results = payload.get("results", [])
    assert results, f"expected at least one recall result, got envelope: {payload}"

    block = payload.get("economics")
    assert isinstance(block, dict), (
        "with RECALL_ECONOMICS on (default), the recall envelope must carry a "
        f"non-null block-total economics roll-up; got {block!r}"
    )
    assert _BLOCK_KEYS <= set(block), (
        "the block economics roll-up must carry the claude-mem "
        f"calculateTokenEconomics keys {_BLOCK_KEYS}; got keys {set(block)}"
    )
    # Every seed falls back to at least DEFAULT_DISCOVERY_TOKENS / a category
    # average, and the stored chunk is non-empty, so the roll-up is strictly
    # positive — the block is real, not a zeroed stub.
    assert block["read_tokens"] > 0 and block["discovery_tokens"] > 0, (
        "the surfaced economics must carry positive injected (read) and "
        f"discovery token totals; got {block}"
    )
    # saved = discovery - read; it is reported honestly (may be negative if a
    # note is bloated), but the field must be present and an int.
    assert isinstance(block["saved_tokens"], int)

    # Every row carries its own economics object next to the mode glyph.
    for row in results:
        econ = row.get("economics")
        assert isinstance(econ, dict), (
            f"each result row must carry a per-row economics object; row {row.get('id')!r} "
            f"had economics={econ!r}"
        )
        assert _ROW_KEYS <= set(econ), (
            f"per-row economics must carry {_ROW_KEYS}; got {set(econ)} for {row.get('id')!r}"
        )
        assert econ["discovery_tokens"] > 0 and econ["read_tokens"] > 0


def test_M8_economics_absent_when_kill_switch_set(behavioral_kb):
    """Arm OFF (control): the SAME seed + SAME query with RECALL_ECONOMICS=0
    produces an envelope whose economics is exactly null and result rows that
    carry NO economics key — the pre-M8 shape. Proves the knob, not the text,
    drives the block's presence."""
    kb = behavioral_kb
    kb.seed(_SEEDS)

    payload = kb.recall(_QUERY, no_mmr=True, env={"RECALL_ECONOMICS": "0"})
    results = payload.get("results", [])
    assert results, f"expected at least one recall result, got envelope: {payload}"

    assert payload.get("economics") is None, (
        "with RECALL_ECONOMICS=0 the block-total economics must be null "
        f"(pre-M8 byte-identical shape); got {payload.get('economics')!r}"
    )
    for row in results:
        assert "economics" not in row, (
            "with the kill switch set, result rows must carry NO economics key "
            f"at all; row {row.get('id')!r} had {row.get('economics')!r}"
        )


def test_M8_block_presence_is_toggled_by_the_knob(behavioral_kb):
    """Decisive control: identical KB + query, the ONLY difference is the
    RECALL_ECONOMICS env flag. ON => economics dict present, OFF => null. The
    XOR of the two outcomes proves the M8 port (not ranking/text luck) is what
    surfaces the economics block."""
    kb = behavioral_kb
    kb.seed(_SEEDS)

    on = kb.recall(_QUERY, no_mmr=True).get("economics")
    off = kb.recall(_QUERY, no_mmr=True, env={"RECALL_ECONOMICS": "0"}).get("economics")

    assert isinstance(on, dict) and off is None, (
        "the economics block must appear ONLY when RECALL_ECONOMICS is on — "
        f"ON economics={on!r}, OFF economics={off!r}. If both are present or "
        "both absent, the kill switch is not gating M8."
    )
