# ABOUTME: Regression tests for port M8 — token-economics surfacing on every
# ABOUTME: recall block (claude-mem TokenCalculator/ContextBuilder shape).
# ABOUTME: Pins: write-time discovery_tokens, injection-time read_tokens, the
# ABOUTME: per-row D:/R:/pct display next to a MODE-driven glyph, header totals,
# ABOUTME: the SessionStart footer, and the recall_log.jsonl economics fields.
"""Port M8: token-economics surfacing on every recall block.

Acceptance bullets pinned here:
  1. every stored learning carries a discovery_tokens integer captured at
     write time (learning_template.md declares it; the frontmatter value
     wins over every fallback)
  2. read_tokens is computed on injection using the active tokenizer
     (the same ≈4-chars/token estimator the R4 budget uses)
  3. the injected block shows per-row 'D:<n> → R:<n> (-<pct>%)' next to a
     type glyph
  4. the header line summarises total saved tokens across the block
  5. the glyph mapping comes from the active mode (M4), not a hardcoded
     constant

Plus the design invariants: the discovery fallback chain (write-time field →
source transcript size → category average), the hook's one-line economics
footer ('memory: N learnings, ~X tok injected, est ~Y tok saved'), the
recall_log.jsonl roll-up for the A4 diagnostic, and the RECALL_ECONOMICS
kill-switch restoring pre-M8 output.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
RECALL_HOOKS = PLUGIN_ROOT / "skills" / "recall" / "hooks"
MODES_DIR = PLUGIN_ROOT / "references" / "modes"
LEARNING_TEMPLATE = PLUGIN_ROOT / "assets" / "learning_template.md"
HOOK = RECALL_HOOKS / "session_start_recall.py"

sys.path.insert(0, str(RECALL_SCRIPTS))
sys.path.insert(0, str(RECALL_HOOKS))

import recall as recall_mod  # noqa: E402
from recall import (  # noqa: E402
    DEFAULT_DISCOVERY_TOKENS,
    DISCOVERY_CATEGORY_AVERAGES,
    ECONOMICS_FALLBACK_GLYPH,
    Learning,
    _est_tokens,
    block_economics,
    learning_economics,
    log_recall,
    mode_glyphs,
    render_json,
    render_markdown,
)

import session_start_recall as hook_mod  # noqa: E402


ROW_RE = re.compile(r"(\S) D:(\d+) → R:(\d+) \((-|\+)(\d+)%\)")


def _learning(
    chunk_len: int = 400,
    learning_type: str = "bug-fix",
    lid: str = "lrn-econ-aaa111",
    **extra_fm,
) -> Learning:
    fm = {
        "id": lid,
        "title": "Econ test note",
        "key_insight": "the insight",
        "learning_type": learning_type,
        **extra_fm,
    }
    return Learning(chunk_text="x" * chunk_len, frontmatter=fm)


@pytest.fixture
def engineering_mode(monkeypatch):
    """Pin the active mode to the repo's real engineering mode regardless of
    the developer machine's REFLECT_MODE / project config."""
    monkeypatch.setenv("REFLECT_MODES_DIR", str(MODES_DIR))
    monkeypatch.setenv("REFLECT_MODE", "engineering")


# =========================================================================
# Acceptance 1 — discovery_tokens captured at write time
# =========================================================================

def test_learning_template_declares_discovery_tokens():
    """The write-time contract: every new learning carries the field."""
    template = LEARNING_TEMPLATE.read_text(encoding="utf-8")
    assert "discovery_tokens: {{DISCOVERY_TOKENS}}" in template


def test_write_time_discovery_tokens_wins_over_every_fallback(tmp_path):
    transcript = tmp_path / "session.jsonl"
    transcript.write_text("t" * 8000, encoding="utf-8")
    lrn = _learning(
        discovery_tokens=4321,
        provenance={"source_path": str(transcript)},
    )
    assert lrn.discovery_tokens == 4321


def test_malformed_discovery_tokens_degrades_to_fallbacks():
    for bad in ("not-a-number", -5, True, None, [1]):
        lrn = _learning(discovery_tokens=bad, learning_type="bug-fix")
        assert lrn.discovery_tokens == DISCOVERY_CATEGORY_AVERAGES["bug-fix"]


def test_discovery_fallback_uses_source_transcript_size(tmp_path):
    transcript = tmp_path / "session.jsonl"
    transcript.write_text("t" * 8000, encoding="utf-8")
    lrn = _learning(provenance={"source_path": str(transcript)})
    assert lrn.discovery_tokens == 8000 // 4


def test_discovery_fallback_category_average_then_default(tmp_path):
    # Vanished transcript -> category average by learning_type.
    lrn = _learning(
        learning_type="decision",
        provenance={"source_path": str(tmp_path / "gone.jsonl")},
    )
    assert lrn.discovery_tokens == DISCOVERY_CATEGORY_AVERAGES["decision"]
    # Unknown/absent type -> generic default.
    assert _learning(learning_type="").discovery_tokens == DEFAULT_DISCOVERY_TOKENS
    assert (
        _learning(learning_type="no-such-type").discovery_tokens
        == DEFAULT_DISCOVERY_TOKENS
    )


# =========================================================================
# Acceptance 2 — read_tokens via the active tokenizer
# =========================================================================

def test_read_tokens_uses_the_active_tokenizer():
    lrn = _learning(chunk_len=403)
    assert lrn.read_tokens == _est_tokens(lrn.chunk_text) == 403 // 4


def test_learning_economics_savings_pct():
    lrn = _learning(chunk_len=400, discovery_tokens=1000)  # read = 100
    econ = learning_economics(lrn, glyphs={})
    assert econ["discovery_tokens"] == 1000
    assert econ["read_tokens"] == 100
    assert econ["savings_pct"] == 90


def test_learning_economics_negative_savings_not_clamped():
    lrn = _learning(chunk_len=8000, discovery_tokens=1000)  # read = 2000
    econ = learning_economics(lrn, glyphs={})
    assert econ["savings_pct"] == -100
    row = recall_mod._economics_row(econ)
    assert "(+100%)" in row  # bloated notes surface honestly


# =========================================================================
# Acceptance 3 — per-row 'D:<n> → R:<n> (-<pct>%)' next to a type glyph
# =========================================================================

def test_markdown_rows_carry_glyph_and_economics(engineering_mode):
    out = render_markdown(
        [
            _learning(chunk_len=400, learning_type="bug-fix",
                      discovery_tokens=3000, lid="lrn-a"),
            _learning(chunk_len=200, learning_type="decision",
                      discovery_tokens=1200, lid="lrn-b"),
        ],
        "tmux kill server",
        max_chars=4000,
    )
    rows = ROW_RE.findall(out)
    assert len(rows) == 2, f"expected 2 economics rows, got: {out!r}"
    # bug-fix row: ⚒ D:3000 → R:100 (-97%)
    assert "⚒ D:3000 → R:100 (-97%)" in out
    # decision row: ⚖ D:1200 → R:50 (-96%)
    assert "⚖ D:1200 → R:50 (-96%)" in out


def test_field_projection_rows_also_carry_economics(engineering_mode):
    out = render_markdown(
        [_learning(discovery_tokens=2000, rule="never wildcard-kill tmux")],
        "tmux",
        field="rule",
        max_chars=4000,
    )
    assert "(field: rule)" in out
    assert "never wildcard-kill tmux" in out
    assert ROW_RE.search(out), f"no economics row in field projection: {out!r}"


# =========================================================================
# Acceptance 4 — header line summarises totals across the block
# =========================================================================

def test_header_line_summarises_block_totals(engineering_mode):
    learnings = [
        _learning(chunk_len=400, discovery_tokens=3000, lid="lrn-a"),
        _learning(chunk_len=200, discovery_tokens=1200, lid="lrn-b"),
    ]
    totals = block_economics(learnings)
    assert totals == {
        "count": 2,
        "read_tokens": 150,
        "discovery_tokens": 4200,
        "saved_tokens": 4050,
        "savings_pct": 96,
    }
    out = render_markdown(learnings, "tmux", max_chars=4000)
    header = out.splitlines()[0]
    assert header.startswith("## Prior learnings relevant to")
    assert "2 learnings" in header
    assert "~150 tok injected" in header
    assert "est ~4050 tok saved" in header


# =========================================================================
# Acceptance 5 — glyph mapping comes from the active mode (M4)
# =========================================================================

def test_glyphs_come_from_the_active_mode_not_a_constant(tmp_path, monkeypatch):
    """A custom mode file changes the glyph with ZERO code changes."""
    modes = tmp_path / "modes"
    modes.mkdir()
    (modes / "engineering.json").write_text(
        json.dumps(
            {
                "name": "Custom",
                "learning_types": [
                    {"id": "bug-fix", "label": "Bug Fix", "work_emoji": "★"},
                ],
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setenv("REFLECT_MODES_DIR", str(modes))
    monkeypatch.setenv("REFLECT_MODE", "engineering")
    assert mode_glyphs() == {"bug-fix": "★"}
    out = render_markdown(
        [_learning(discovery_tokens=1000)], "q", max_chars=4000,
    )
    assert "★ D:1000" in out


def test_default_engineering_mode_declares_work_glyphs(engineering_mode):
    assert mode_glyphs() == {
        "pattern": "⌕",
        "correction": "⌕",
        "bug-fix": "⚒",
        "decision": "⚖",
        "anti-pattern": "⚠",
    }


def test_emoji_falls_back_when_work_emoji_absent(tmp_path, monkeypatch):
    modes = tmp_path / "modes"
    modes.mkdir()
    (modes / "engineering.json").write_text(
        json.dumps(
            {"learning_types": [{"id": "decision", "emoji": "⚖"}]}
        ),
        encoding="utf-8",
    )
    monkeypatch.setenv("REFLECT_MODES_DIR", str(modes))
    monkeypatch.setenv("REFLECT_MODE", "engineering")
    assert mode_glyphs() == {"decision": "⚖"}


def test_broken_modes_dir_degrades_to_neutral_glyph(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_MODES_DIR", str(tmp_path / "nope"))
    monkeypatch.setenv("REFLECT_MODE", "engineering")
    assert mode_glyphs() == {}
    econ = learning_economics(_learning(discovery_tokens=1000), glyphs={})
    assert econ["glyph"] == ECONOMICS_FALLBACK_GLYPH


def test_unknown_type_takes_the_neutral_fallback_glyph(engineering_mode):
    econ = learning_economics(
        _learning(learning_type="no-such-type", discovery_tokens=1000)
    )
    assert econ["glyph"] == ECONOMICS_FALLBACK_GLYPH


# =========================================================================
# JSON surface + recall_log.jsonl roll-up (A4 correlation)
# =========================================================================

def test_render_json_carries_per_result_and_block_economics(engineering_mode):
    blob = json.loads(render_json(
        [_learning(chunk_len=400, discovery_tokens=3000)], "q", "naive",
    ))
    assert blob["economics"] == {
        "count": 1,
        "read_tokens": 100,
        "discovery_tokens": 3000,
        "saved_tokens": 2900,
        "savings_pct": 97,
    }
    row = blob["results"][0]["economics"]
    assert row["discovery_tokens"] == 3000
    assert row["read_tokens"] == 100
    assert row["savings_pct"] == 97
    assert row["glyph"] == "⚒"


def test_log_recall_records_economics(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    learnings = [_learning(chunk_len=400, discovery_tokens=3000)]
    log_recall(
        "q", "naive", 1, cached=False, economics=block_economics(learnings),
    )
    line = json.loads(
        (tmp_path / "recall_log.jsonl").read_text().splitlines()[-1]
    )
    assert line["injected_tokens"] == 100
    assert line["discovery_tokens"] == 3000
    assert line["saved_tokens"] == 2900
    assert line["savings_pct"] == 97


def test_log_recall_without_economics_keeps_legacy_shape(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    log_recall("q", "naive", 0, cached=True, cache_tier="exact")
    line = json.loads(
        (tmp_path / "recall_log.jsonl").read_text().splitlines()[-1]
    )
    assert "injected_tokens" not in line
    assert line["cache_tier"] == "exact"


# =========================================================================
# Kill-switch — RECALL_ECONOMICS=0 restores pre-M8 output
# =========================================================================

def test_kill_switch_restores_pre_m8_markdown(engineering_mode, monkeypatch):
    monkeypatch.setattr(recall_mod, "ECONOMICS_ENABLED", False)
    out = render_markdown(
        [_learning(discovery_tokens=3000)], "tmux", max_chars=4000,
    )
    assert "D:" not in out
    assert "tok injected" not in out
    assert out.splitlines()[0] == "## Prior learnings relevant to `tmux`"


def test_kill_switch_nulls_json_economics(engineering_mode, monkeypatch):
    monkeypatch.setattr(recall_mod, "ECONOMICS_ENABLED", False)
    blob = json.loads(render_json([_learning()], "q", "naive"))
    assert blob["economics"] is None
    assert "economics" not in blob["results"][0]


# =========================================================================
# SessionStart hook — the one-line economics footer
# =========================================================================

def test_economics_footer_sums_the_rows():
    block = (
        "## Prior learnings relevant to `q` — 2 learnings, ~150 tok "
        "injected, est ~4050 tok saved\n"
        "- **[lrn-a]** x — ⚒ D:3000 → R:100 (-97%)\n"
        "- **[lrn-b]** y — ⚖ D:1200 → R:50 (-96%)\n"
    )
    assert hook_mod.economics_footer(block) == (
        "memory: 2 learnings, ~150 tok injected, est ~4050 tok saved"
    )


def test_economics_footer_empty_without_rows():
    assert hook_mod.economics_footer("") == ""
    assert hook_mod.economics_footer("- plain learning, no economics") == ""


def test_hook_appends_footer_to_injected_block(tmp_path):
    """End-to-end: the hook subprocess emits block + footer, exit 0."""
    home = tmp_path / "home"
    home.mkdir()
    project = tmp_path / "playwright"
    project.mkdir()
    uv_dir = tmp_path / "uvbin"
    uv_dir.mkdir()
    block = "- **[lrn-a]** x — ⚒ D:3000 → R:100 (-97%)"
    uv = uv_dir / "uv"
    uv.write_text(f"#!/bin/sh\necho '{block}'\n", encoding="utf-8")
    uv.chmod(0o755)
    env = {
        "PATH": str(uv_dir),
        "HOME": str(home),
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "CLAUDE_PROJECT_DIR": str(project),
    }
    result = subprocess.run(
        [sys.executable, str(HOOK)],
        input="{}", capture_output=True, text=True, env=env, timeout=30,
    )
    assert result.returncode == 0, result.stderr
    ctx = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
    assert block in ctx
    assert ctx.rstrip().endswith(
        "memory: 1 learnings, ~100 tok injected, est ~2900 tok saved"
    )


def test_hook_emits_no_footer_for_economics_free_block(tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    project = tmp_path / "playwright"
    project.mkdir()
    uv_dir = tmp_path / "uvbin"
    uv_dir.mkdir()
    uv = uv_dir / "uv"
    uv.write_text("#!/bin/sh\necho '- plain learning [lrn-z]'\n", encoding="utf-8")
    uv.chmod(0o755)
    env = {
        "PATH": str(uv_dir),
        "HOME": str(home),
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "CLAUDE_PROJECT_DIR": str(project),
    }
    result = subprocess.run(
        [sys.executable, str(HOOK)],
        input="{}", capture_output=True, text=True, env=env, timeout=30,
    )
    assert result.returncode == 0, result.stderr
    ctx = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
    assert "plain learning" in ctx
    assert "memory:" not in ctx


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
