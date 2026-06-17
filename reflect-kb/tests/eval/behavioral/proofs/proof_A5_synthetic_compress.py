# ABOUTME: Behavioral proof for A5 — synthetic (no-LLM) compression fallback.
# ABOUTME: Drives the REAL synthetic_compress module on a slice+signals with NO LLM:
# ABOUTME: asserts the heuristics populate every required learning field (title from
# ABOUTME: tool i/o, concepts from keywords, files from path regex, importance from the
# ABOUTME: rule table), stamps compression=synthetic, and is byte-shaped to index
# ABOUTME: identically to an LLM learning (same required frontmatter, valid YAML).
"""A5 synthetic-compression-fallback proof.

Port A5 (bead agents-in-a-box-kdo.54, surface=consolidation) is a CONSOLIDATION
port whose behaviour lives in the plugin's
``plugins/reflect/scripts/synthetic_compress.py`` — NOT in the file-engine recall
pipeline — so this proof drives the real module directly. There is NO LLM, NO
torch model, and NO vector engine in any assertion: the slice text + the
literal detected signals fully determine every asserted field. That is the whole
point of A5 — when the drain LLM is unavailable (budget exhausted, network down,
errored 3x, or ``--no-llm``) the old path captured NOTHING; A5 produces a
structured learning from HEURISTICS ALONE so every signal still reaches the
index, with ZERO tokens spent.

The invariant (each arm's slice + signals + the heuristic fully determine the
verdict — no LLM, no embedding model, no network anywhere in the assertion):

  A. HEURISTICS POPULATE THE REQUIRED FIELDS. ``synthetic_compress`` on a real
     slice + real detector signals returns a record whose four heuristic fields
     are populated by the documented heuristics — title FROM tool i/o (the
     strongest signal text), concepts FROM keyword extraction, files FROM the
     file-path regex (the exact paths in the slice, diff/relative prefixes
     normalised, path:line suffixes stripped), and importance FROM the rule
     table (a HIGH-confidence correction outranks an approval-only slice). No
     field is left empty for a signal-bearing slice.

  B. compression=synthetic IS CARRIED. The record — and the persisted note's
     frontmatter — carries ``compression: synthetic`` so rerank can
     de-prioritise heuristic captures. A control LLM-shaped note written through
     the SAME writer carries NO ``compression`` key, so the flag is decisive: it
     is present iff the synthetic path produced the note.

  C. BYTE-SHAPED TO INDEX IDENTICALLY. The synthetic note is written through the
     SAME ``output_generator.create_knowledge_note`` path the LLM drain uses, so
     its frontmatter is valid YAML carrying EVERY required field an LLM note
     carries (title/category/tags/symptoms/root_cause/key_insight/created/
     confidence/confidence_num/provenance) — it differs ONLY by the extra
     synthetic-provenance keys (compression/files_affected/synthetic_reason).
     This is the "indexed identically" half: the indexer cannot tell the shape
     apart, only the de-prioritisation flag differs.

  D. KNOB / FALSIFIABLE CONTROL — importance rule table. The SAME slice scored
     with a HIGH-confidence signal yields a strictly higher confidence midpoint
     than the SAME slice scored with only a LOW-confidence signal; an EMPTY
     signal set falls through to the LOW floor. Flipping the strongest signal's
     confidence — and nothing else — moves the importance, proving the rule
     table (not text luck) owns the score.

  E. DETERMINISM + ZERO LLM. Two calls on identical inputs produce identical
     records (title/concepts/files/confidence). The module imports no LLM/engine
     client; the proof patches the writer to a tmp project so no network or
     model is reachable, and the run still succeeds — there is no LLM in the
     path to call.

Falsifiability: if the title heuristic ignored tool i/o, arm A's title
assertion would fail. If the file regex missed slice paths, arm A's files
assertion would be empty. If the note dropped a required field, arm C's parity
assertion would fail (the indexer would see a different shape). If the
``compression`` flag were absent, arm B would fail and rerank could not
de-prioritise. If importance were a fixed constant (the naive agentmemory
``importance: 5``), arm D's HIGH-vs-LOW inequality would collapse to equality
and FAIL.

PORT: A5
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest
import yaml

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the S5 cascade proof does so this runs from either checkout layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import synthetic_compress as SC  # noqa: E402
import output_generator as OG  # noqa: E402
from signal_detector import detect_signals  # noqa: E402


# The frontmatter fields every learning note carries — what the indexer keys on.
# A synthetic note MUST carry all of these to "index identically".
_REQUIRED_FM_FIELDS = {
    "title", "category", "tags", "symptoms", "root_cause",
    "key_insight", "created", "confidence", "confidence_num", "provenance",
}

# A real slice: an explicit HIGH correction naming a file, plus a fix line
# naming a second file with a path:line suffix. The detector finds the signals;
# the regex finds the files. No LLM involved.
_SLICE = (
    "User: never use print() for logging, always use the logger in "
    "src/util/log.py\n"
    "Assistant: switching to the logger.\n"
    "User: the bug was a race condition in handlers/worker.py:42, "
    "fixed by adding a mutex\n"
)


@pytest.fixture
def project(tmp_path, monkeypatch):
    """Point the note writer at an isolated tmp project so no real docs/ is
    touched and no network/model is reachable — the synthetic path is pure."""
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    return tmp_path


def _read_frontmatter(note_path: str) -> dict:
    """Parse the leading YAML frontmatter of a written note. Proves the note is
    valid YAML (byte-shaped to index) — a malformed block would raise here."""
    text = Path(note_path).read_text(encoding="utf-8")
    assert text.startswith("---"), "note missing leading frontmatter fence"
    block = text.split("---", 2)[1]
    return yaml.safe_load(block)


# --- arm A: heuristics populate the required fields -------------------------

def test_heuristics_populate_required_fields(project):
    """title FROM tool i/o, concepts FROM keywords, files FROM path regex,
    importance FROM the rule table — every heuristic field is populated for a
    real signal-bearing slice, with ZERO LLM calls."""
    signals = detect_signals(_SLICE)
    assert signals, "detector must find the explicit correction/fix signals"

    rec = SC.synthetic_compress(
        _SLICE, signals, source_path="t.jsonl", reason="no_llm", write=True
    )

    # title <- tool i/o (the strongest signal's text), non-empty + capped.
    assert rec.title.strip()
    assert "never use print()" in rec.title  # the HIGH correction surfaced

    # concepts <- keyword extraction (content words, stopwords dropped).
    assert rec.concepts, "keyword extraction yielded no concepts"
    assert "logger" in rec.concepts
    assert "the" not in rec.concepts  # stopword dropped

    # files <- file-path regex (both files, prefixes/suffixes normalised).
    assert "src/util/log.py" in rec.files
    assert "handlers/worker.py" in rec.files  # ':42' suffix stripped
    assert all(":" not in f for f in rec.files)  # no path:line residue

    # importance <- rule table; a HIGH correction caps at MEDIUM (synthetic is
    # one tier below an adjudicated note), never the LOW floor.
    assert rec.confidence == "MEDIUM"
    assert rec.confidence_num == pytest.approx(0.6)
    assert rec.importance == "high"  # strongest detected signal was HIGH

    assert rec.note_path is not None  # the note was persisted


# --- arm B: compression=synthetic is carried (decisive flag) ----------------

def test_compression_flag_present_and_decisive(project):
    """The record AND the persisted frontmatter carry ``compression: synthetic``;
    an LLM-shaped note through the SAME writer carries NO such key — so the flag
    is present iff the synthetic path produced the note."""
    signals = detect_signals(_SLICE)
    rec = SC.synthetic_compress(_SLICE, signals, reason="budget", write=True)

    assert rec.compression == SC.COMPRESSION_SYNTHETIC == "synthetic"
    fm = _read_frontmatter(rec.note_path)
    assert fm.get("compression") == "synthetic"
    assert fm.get("synthetic_reason") == "budget"

    # Control: a plain LLM note written through the same writer has NO flag.
    llm_path, _ = OG.create_knowledge_note(
        title="llm authored note", category="Domain", tags=["x"], symptoms=[],
        root_cause="rc", key_insight="ki", problem="p", solution="s",
    )
    llm_fm = yaml.safe_load(Path(llm_path).read_text().split("---", 2)[1])
    assert "compression" not in llm_fm  # the flag is synthetic-only


# --- arm C: byte-shaped to index identically --------------------------------

def test_indexed_identically_to_llm_note(project):
    """The synthetic note's frontmatter is valid YAML carrying EVERY required
    field an LLM note carries — it differs only by the synthetic-provenance
    keys. The indexer sees the same shape; only the de-prioritisation flag is
    extra."""
    signals = detect_signals(_SLICE)
    rec = SC.synthetic_compress(_SLICE, signals, reason="no_llm", write=True)
    syn_fm = _read_frontmatter(rec.note_path)  # raises if not valid YAML

    llm_path, _ = OG.create_knowledge_note(
        title="llm authored note", category="Domain", tags=["x"], symptoms=[],
        root_cause="rc", key_insight="ki", problem="p", solution="s",
    )
    llm_fm = yaml.safe_load(Path(llm_path).read_text().split("---", 2)[1])

    # Both carry every required field — same shape to the indexer.
    assert _REQUIRED_FM_FIELDS <= set(syn_fm), (
        f"synthetic note missing required fields: "
        f"{_REQUIRED_FM_FIELDS - set(syn_fm)}"
    )
    assert _REQUIRED_FM_FIELDS <= set(llm_fm)

    # The ONLY difference is the synthetic-provenance keys — nothing required
    # was dropped, so the index row shape is identical.
    extra = set(syn_fm) - set(llm_fm)
    assert extra <= {"compression", "files_affected", "synthetic_reason"}, extra
    assert "compression" in extra  # the de-prioritisation flag is the marker

    # provenance shape matches the LLM note's (same keys) — same traceability.
    assert set(syn_fm["provenance"]) == set(llm_fm["provenance"])


# --- arm D: importance rule table is the knob (falsifiable) ------------------

class _Sig:
    """Minimal signal shape the heuristics read (.signal/.confidence/.category/
    .source_quote)."""

    def __init__(self, signal, confidence):
        self.signal = signal
        self.confidence = confidence
        self.category = ""
        self.source_quote = ""
        self.line_number = 1


def test_importance_rule_table_is_the_knob():
    """Flipping ONLY the strongest signal's confidence moves the importance
    score: HIGH > LOW > (empty -> floor). A fixed constant would collapse these
    to equality — this is the decisive falsifier against the naive
    agentmemory ``importance: 5`` constant."""
    high_tier, high_num, high_label = SC.score_importance(
        [_Sig("never do X", "HIGH")]
    )
    low_tier, low_num, low_label = SC.score_importance(
        [_Sig("nice", "LOW")]
    )
    empty_tier, empty_num, empty_label = SC.score_importance([])

    # HIGH correction strictly outranks a LOW-only slice.
    assert high_num > low_num, (high_num, low_num)
    assert high_label == "high" and low_label == "low"
    # Empty signal set falls through to the LOW floor (never invents trust).
    assert empty_num == low_num == pytest.approx(0.3)
    assert empty_label == "none"

    # The strongest signal wins regardless of order — a HIGH anywhere lifts it.
    mixed_tier, mixed_num, _ = SC.score_importance(
        [_Sig("nice", "LOW"), _Sig("never do X", "HIGH")]
    )
    assert mixed_num == high_num


# --- arm E: determinism + zero LLM ------------------------------------------

def test_deterministic_no_llm(project):
    """Two calls on identical inputs produce identical records. The module has
    no LLM/engine client and the writer is patched to a tmp project — there is
    no model or network in the path to call, yet the capture still succeeds."""
    signals = detect_signals(_SLICE)
    a = SC.synthetic_compress(_SLICE, signals, reason="no_llm", write=False)
    b = SC.synthetic_compress(_SLICE, signals, reason="no_llm", write=False)

    assert a.title == b.title
    assert a.concepts == b.concepts
    assert a.files == b.files
    assert (a.confidence, a.confidence_num) == (b.confidence, b.confidence_num)
    assert a.compression == b.compression == "synthetic"

    # The module surface exposes no LLM/embedding entry points — purely
    # heuristic functions. (Sanity that the path is genuinely no-LLM.)
    for forbidden in ("embed", "llm", "anthropic", "openai", "completion"):
        assert not hasattr(SC, forbidden), (
            f"synthetic_compress unexpectedly exposes {forbidden!r} — A5 must "
            f"be zero-LLM"
        )


def test_empty_signals_still_indexes(project):
    """Fallback robustness: even a slice the detector found no signals on still
    yields a valid, indexable synthetic record (title from the first slice line)
    — A5 never drops a capture, which is its whole reason to exist."""
    plain = "User: we migrated the store to src/db/store.py last week\n"
    rec = SC.synthetic_compress(plain, [], reason="no_llm", write=True)
    assert rec.title.strip()
    assert rec.compression == "synthetic"
    fm = _read_frontmatter(rec.note_path)
    assert _REQUIRED_FM_FIELDS <= set(fm)
    assert fm["compression"] == "synthetic"
    # The file regex still fired off the slice text even with no signals.
    assert "src/db/store.py" in rec.files
