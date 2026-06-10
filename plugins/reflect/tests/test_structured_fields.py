# ABOUTME: Regression tests for port S1 — structured field extraction at drain.
# ABOUTME: Pins the Hindsight fact_extraction shape: typed frontmatter fields
# ABOUTME: (problem/root_cause/fix/rule/category/entities/causal_relations) on
# ABOUTME: new learnings, recall --field projection, legacy-note degradation.
"""Port S1: structured field extraction at drain.

Acceptance bullets pinned here:
  1. new learnings have populated `rule` / `root_cause` / `fix` fields
     (output_generator writes them to frontmatter; the template and the
     reflect SKILL.md mandate them for LLM-authored notes)
  2. recall can filter by field (`recall.py --field rule` projects each hit
     to one structured field instead of the whole note)
  3. existing free-form notes degrade gracefully (missing field falls back
     to the matching body section, then key_insight/title — never dropped,
     never a crash)
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS_DIR = PLUGIN_ROOT / "scripts"
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))
sys.path.insert(0, str(RECALL_SCRIPTS))

import output_generator  # noqa: E402
import recall as recall_mod  # noqa: E402
from recall import (  # noqa: E402
    FIELD_BODY_SECTIONS,
    FIELD_VALUE_MAX_CHARS,
    Learning,
    RecallResult,
    _stringify_field,
    render_json,
    render_markdown,
)

LEARNING_TEMPLATE = PLUGIN_ROOT / "assets" / "learning_template.md"
REFLECT_SKILL = PLUGIN_ROOT / "skills" / "reflect" / "SKILL.md"

STRUCTURED_FIELDS = (
    "problem", "root_cause", "fix", "rule", "category",
    "entities", "causal_relations",
)


def _parse_note(path: Path) -> tuple[dict, str]:
    text = path.read_text()
    assert text.startswith("---")
    end = text.find("\n---", 3)
    fm = yaml.safe_load(text[3:end])
    return fm, text[end + 4:]


@pytest.fixture()
def project(tmp_path, monkeypatch):
    """Isolated non-git project dir so notes land under tmp_path."""
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    return tmp_path


# =========================================================================
# Acceptance 1 — new learnings have populated rule / root_cause / fix fields
# =========================================================================

def test_create_knowledge_note_writes_structured_frontmatter(project):
    path, slug = output_generator.create_knowledge_note(
        title="S1 structured note",
        category="debugging-sessions",
        tags=["s1"],
        symptoms=["pytest hangs"],
        root_cause="fixture leaks a thread that never joins",
        key_insight="join threads in fixture teardown",
        problem="pytest run hangs forever after the last test. The thread "
                "spawned in the fixture is non-daemon and never joined.",
        solution="Mark the worker thread daemon=True and join it in teardown.",
        fix="set daemon=True and join the worker in fixture teardown",
        rule="Always join fixture-spawned threads in teardown",
        entities=["pytest", "threading"],
        causal_relations=[
            {"source": "non-daemon thread", "target": "pytest hang",
             "type": "caused_by"},
        ],
    )
    fm, body = _parse_note(path)

    # The bead's headline fields are populated.
    assert fm["rule"] == "Always join fixture-spawned threads in teardown"
    assert fm["root_cause"] == "fixture leaks a thread that never joins"
    assert fm["fix"] == "set daemon=True and join the worker in fixture teardown"
    # The full structured set lands in frontmatter.
    assert fm["problem"].startswith("pytest run hangs forever")
    assert fm["category"] == "debugging-sessions"
    assert fm["entities"] == ["pytest", "threading"]
    assert fm["causal_relations"] == [
        {"source": "non-daemon thread", "target": "pytest hang",
         "type": "caused_by"},
    ]
    # Prose body stays as the human-readable rationale.
    assert "## Problem" in body
    assert "## Solution" in body
    assert "daemon=True" in body


def test_problem_frontmatter_is_one_liner_not_full_prose(project):
    long_problem = ("First sentence describes the failure. " +
                    "Filler detail. " * 50)
    path, _ = output_generator.create_knowledge_note(
        title="S1 one-liner", category="testing", tags=[], symptoms=[],
        root_cause="rc", key_insight="ki",
        problem=long_problem, solution="sol",
    )
    fm, body = _parse_note(path)
    assert fm["problem"] == "First sentence describes the failure."
    # Full prose retained in the body.
    assert "Filler detail." in body


def test_legacy_call_omits_unpopulated_structured_keys(project):
    """Pre-S1 call shape: no rule/fix/entities/causal_relations keys appear
    (absent, not empty strings) so old-style notes stay clean."""
    path, _ = output_generator.create_knowledge_note(
        title="S1 legacy shape", category="testing", tags=["t"],
        symptoms=["s"], root_cause="rc", key_insight="ki",
        problem="p", solution="s",
    )
    fm, _ = _parse_note(path)
    for key in ("rule", "fix", "entities", "causal_relations"):
        assert key not in fm
    # root_cause/category were always frontmatter; problem is now derived.
    assert fm["root_cause"] == "rc"
    assert fm["problem"] == "p"


def test_fabricated_ref_in_fix_or_rule_is_caught(tmp_path, monkeypatch):
    """M5 hallucination check covers the new LLM-authored fields too."""
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    (repo / "f.txt").write_text("x\n")
    subprocess.run(["git", "add", "f.txt"], cwd=repo, check=True)
    subprocess.run(
        ["git", "-c", "user.email=t@t", "-c", "user.name=t",
         "commit", "-qm", "init"],
        cwd=repo, check=True,
    )
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(repo))
    monkeypatch.chdir(repo)
    with pytest.raises(ValueError, match="all_refs_hallucinated"):
        output_generator.create_knowledge_note(
            title="S1 m5", category="testing", tags=[], symptoms=[],
            root_cause="rc", key_insight="ki",
            problem="no refs here", solution="none here either",
            fix="reverted in 1234567890abcdef1234",
        )


def test_learning_template_declares_structured_fields():
    text = LEARNING_TEMPLATE.read_text()
    for field in STRUCTURED_FIELDS:
        assert f"{field}:" in text, f"template missing `{field}:`"
    assert "caused_by" in text


def test_reflect_skill_mandates_structured_fields():
    text = REFLECT_SKILL.read_text()
    assert "Structured field extraction" in text
    for field in STRUCTURED_FIELDS:
        assert f"{field}:" in text, f"SKILL.md schema missing `{field}:`"
    # The selectivity tuning from Hindsight's prompt survives the port.
    assert "6 months" in text


# =========================================================================
# Acceptance 2 — recall can filter by field
# =========================================================================

def _structured_learning() -> Learning:
    return Learning(
        chunk_text=(
            "---\nid: lrn-s1-aaa111\n---\n\n## Problem\n\nLong prose problem "
            "paragraph that should NOT be injected when projecting.\n\n"
            "## Solution\n\nLong prose solution.\n"
        ),
        frontmatter={
            "id": "lrn-s1-aaa111",
            "title": "structured one",
            "key_insight": "insight text",
            "rule": "Never kill tmux by wildcard",
            "fix": "kill by exact session name",
            "root_cause": "wildcard matched other sessions",
        },
    )


def _legacy_learning() -> Learning:
    return Learning(
        chunk_text=(
            "---\nid: lrn-legacy-bbb222\n---\n\n## Problem\n\nLegacy prose "
            "problem statement.\n\n## Solution\n\nLegacy prose fix steps.\n"
        ),
        frontmatter={
            "id": "lrn-legacy-bbb222",
            "title": "legacy one",
            "key_insight": "legacy key insight",
        },
    )


def test_field_value_returns_frontmatter_field():
    lrn = _structured_learning()
    assert lrn.field_value("rule") == "Never kill tmux by wildcard"
    assert lrn.field_value("fix") == "kill by exact session name"
    assert lrn.field_value("root_cause") == "wildcard matched other sessions"


def test_render_markdown_field_projection_returns_just_the_field():
    out = render_markdown([_structured_learning()], "tmux", field="rule")
    assert "Never kill tmux by wildcard" in out
    assert "(field: rule)" in out
    # The prose body and the default key-insight rendering are NOT injected.
    assert "Long prose problem" not in out
    assert "insight text" not in out
    assert "How to apply" not in out


def test_render_json_carries_field_value():
    blob = json.loads(render_json(
        [_structured_learning()], "tmux", "naive", field="rule",
    ))
    assert blob["field"] == "rule"
    assert blob["results"][0]["field_value"] == "Never kill tmux by wildcard"


def test_render_json_without_field_unchanged():
    blob = json.loads(render_json([_structured_learning()], "tmux", "naive"))
    assert blob["field"] is None
    assert "field_value" not in blob["results"][0]


def test_cli_accepts_field_flag(monkeypatch, capsys):
    result = RecallResult([_structured_learning()], "tmux", "naive")
    monkeypatch.setattr(recall_mod, "recall", lambda *a, **k: result)
    monkeypatch.setattr(sys, "argv", ["recall.py", "tmux", "--field", "rule"])
    assert recall_mod.main() == 0
    out = capsys.readouterr().out
    assert "Never kill tmux by wildcard" in out
    assert "Long prose problem" not in out


def test_stringify_field_shapes():
    assert _stringify_field("  text  ") == "text"
    assert _stringify_field(["a", "b"]) == "a, b"
    rel = [{"source": "a", "target": "b", "type": "caused_by"}]
    assert json.loads(_stringify_field(rel)) == rel
    assert _stringify_field(None) == ""
    assert _stringify_field(True) == ""
    assert len(_stringify_field("x" * 2000)) == FIELD_VALUE_MAX_CHARS


# =========================================================================
# Acceptance 3 — existing free-form notes degrade gracefully
# =========================================================================

def test_legacy_note_falls_back_to_body_section():
    lrn = _legacy_learning()
    # problem → ## Problem, fix → ## Solution (FIELD_BODY_SECTIONS map).
    assert FIELD_BODY_SECTIONS["problem"] == "Problem"
    assert FIELD_BODY_SECTIONS["fix"] == "Solution"
    assert lrn.field_value("problem") == "Legacy prose problem statement."
    assert lrn.field_value("fix") == "Legacy prose fix steps."


def test_legacy_note_markdown_falls_back_to_key_insight():
    out = render_markdown([_legacy_learning()], "tmux", field="rule")
    # No `rule` frontmatter and no mapped section — key_insight stands in,
    # the hit is never dropped.
    assert "lrn-legacy-bbb222" in out
    assert "legacy key insight" in out


def test_legacy_note_json_field_value_is_null():
    blob = json.loads(render_json(
        [_legacy_learning()], "tmux", "naive", field="rule",
    ))
    # JSON stays honest: null, not a silent key-insight substitution.
    assert blob["results"][0]["field_value"] is None
    assert blob["count"] == 1


def test_mixed_corpus_renders_every_hit():
    out = render_markdown(
        [_structured_learning(), _legacy_learning()],
        "tmux", field="rule",
    )
    assert "Never kill tmux by wildcard" in out
    assert "legacy key insight" in out


def test_unknown_field_never_crashes():
    for lrn in (_structured_learning(), _legacy_learning()):
        assert lrn.field_value("no_such_field") == ""
    out = render_markdown([_legacy_learning()], "q", field="no_such_field")
    assert "legacy key insight" in out  # title/key_insight fallback


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
