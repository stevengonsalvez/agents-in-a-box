# ABOUTME: Behavioral proof for S1 — structured field extraction at drain.
# ABOUTME: Drives the REAL output_generator.create_knowledge_note (capture surface)
# ABOUTME: and asserts typed frontmatter fields land iff the input carries them.
"""Port S1: structured field extraction at drain.

True invariant (corrected against the real diff at d4e8cb42 —
`feat(reflect): extract structured fields at drain (S1)`):

At DRAIN (write) time, ``output_generator.create_knowledge_note`` emits typed,
single-purpose frontmatter fields — ``problem`` / ``fix`` / ``rule`` /
``entities`` / ``causal_relations`` (the Hindsight fact_extraction shape) —
*alongside* the prose body, but ONLY for the data the caller actually passes:

  * POPULATED: when the structured inputs are supplied, they land verbatim in
    the emitted YAML frontmatter. ``problem`` is auto-derived from the prose by
    the deterministic ``_one_liner`` parser (first non-empty line, truncated at
    the first ". " sentence boundary), and ``causal_relations`` (a list of
    dicts) is JSON-encoded so it parses back as valid structured YAML — not a
    stringified python dict. The full prose stays in the ``## Problem`` /
    ``## Solution`` body.

  * ABSENT (no hallucinated fill): when those inputs are omitted, the keys are
    OMITTED from frontmatter entirely — not written as empty strings, not
    invented. The extractor never fabricates a field the input did not contain.
    A legacy-shaped call gains only the derived ``problem`` line.

The decisive, LLM-free contrast: ``fix`` / ``rule`` / ``entities`` /
``causal_relations`` appear in the frontmatter IFF the corresponding argument is
present. The presence of each typed field is *caused by* the structured input,
not by any model deciding what to write. This is the capture-surface version of
the knob toggle: the "knob" is whether the structured argument is supplied; the
emitted frontmatter changes accordingly.

Why no LLM is involved: ``create_knowledge_note`` is a pure write-time function.
The inputs are deterministic literals, ``_one_liner`` is a string parser (no
model), and YAML serialization is deterministic. The assertions read back the
emitted frontmatter and compare it to the literals — nothing an LLM decided.

Surface: capture (drain write path). This proof does NOT use the retrieval
engine — it drives ``output_generator`` directly via the eval venv python in a
fresh child process per arm, each under its OWN isolated ``CLAUDE_PROJECT_DIR``
tmp tree, so no arm shares note-output state, config, or environment with any
other arm (the same per-arm isolation discipline the engine proofs use).

PORT: S1
"""
from __future__ import annotations

import json
import subprocess
import sys
import textwrap

# Resolve the plugin scripts dir from the same anchor conftest uses for
# recall.py: RECALL_PY is .../plugins/reflect/skills/recall/scripts/recall.py,
# so parents[0..3] are scripts/recall/skills/reflect — parents[3] is the
# plugin root, whose scripts/ holds output_generator.
from eval.behavioral.conftest import RECALL_PY  # noqa: E402

PLUGIN_ROOT = RECALL_PY.parents[3]          # plugins/reflect
SCRIPTS_DIR = PLUGIN_ROOT / "scripts"       # plugins/reflect/scripts
assert (SCRIPTS_DIR / "output_generator.py").exists(), (
    f"output_generator.py not found under {SCRIPTS_DIR}"
)


# The structured input the populated arm feeds. Multi-sentence ``problem`` prose
# so we can also prove the one-liner derivation is a parse (first sentence only).
_STRUCTURED_INPUT = dict(
    title="S1 structured drain note",
    category="debugging-sessions",
    tags=["s1", "threading"],
    symptoms=["pytest hangs after the last test"],
    root_cause="a fixture leaks a non-daemon thread that is never joined",
    key_insight="join fixture-spawned threads in teardown",
    problem=(
        "pytest run hangs forever after the last test. "
        "The thread spawned in the fixture is non-daemon and never joined, so "
        "the interpreter waits on it at shutdown."
    ),
    solution="Mark the worker thread daemon=True and join it in teardown.",
    fix="set daemon=True and join the worker in fixture teardown",
    rule="Always join fixture-spawned threads in teardown",
    entities=["pytest", "threading"],
    causal_relations=[
        {"source": "non-daemon thread", "target": "pytest hang",
         "type": "caused_by"},
    ],
)

# The control: the SAME prose, but the structured args are omitted (the
# legacy/pre-S1 call shape). Only the derived ``problem`` line should appear.
_CONTROL_INPUT = dict(
    title="S1 control drain note",
    category="debugging-sessions",
    tags=["s1", "threading"],
    symptoms=["pytest hangs after the last test"],
    root_cause="a fixture leaks a non-daemon thread that is never joined",
    key_insight="join fixture-spawned threads in teardown",
    problem=(
        "pytest run hangs forever after the last test. "
        "The thread spawned in the fixture is non-daemon and never joined, so "
        "the interpreter waits on it at shutdown."
    ),
    solution="Mark the worker thread daemon=True and join it in teardown.",
)

# The deterministically-derived one-liner: first non-empty line up to the first
# ". " sentence boundary (inclusive). NOT an LLM summary — a string slice.
_EXPECTED_PROBLEM_ONE_LINER = (
    "pytest run hangs forever after the last test."
)


def _drain_in_isolated_project(tmp_path, kwargs: dict) -> dict:
    """Run the REAL ``create_knowledge_note`` in a FRESH child process under an
    isolated ``CLAUDE_PROJECT_DIR`` (this arm's own tmp tree) and return the
    parsed frontmatter + body of the emitted note.

    Out-of-process on purpose: the note is written under ``CLAUDE_PROJECT_DIR``
    and the M5 commit-verifier resolves its repo from the same env, so a per-arm
    child with its own tmp project dir shares NO output state, config, or env
    with any other arm — whatever the run order. The inputs are literals and the
    serialization is deterministic, so the emitted frontmatter is fully
    determined by ``kwargs``.
    """
    project_dir = tmp_path / "project"
    project_dir.mkdir()

    driver = textwrap.dedent(
        """
        import json, sys
        sys.path.insert(0, sys.argv[1])  # plugins/reflect/scripts
        import yaml
        import output_generator
        kwargs = json.loads(sys.argv[2])
        path, slug = output_generator.create_knowledge_note(**kwargs)
        text = path.read_text()
        assert text.startswith("---"), "note must begin with YAML frontmatter"
        end = text.find("\\n---", 3)
        assert end != -1, "frontmatter terminator not found"
        fm = yaml.safe_load(text[3:end])
        body = text[end + 4:]
        print(json.dumps({"frontmatter": fm, "body": body}))
        """
    )

    import os

    env = dict(os.environ)
    # Isolate the write target AND the M5 commit-verifier's repo lookup to this
    # arm's own tmp dir (no .git there, and our inputs carry no commit refs, so
    # verify_refs finds nothing to check and the write proceeds).
    env["CLAUDE_PROJECT_DIR"] = str(project_dir)

    proc = subprocess.run(
        [sys.executable, "-c", driver, str(SCRIPTS_DIR), json.dumps(kwargs)],
        capture_output=True, text=True, env=env, timeout=120,
        cwd=str(project_dir),
    )
    assert proc.returncode == 0, (
        "create_knowledge_note drain failed:\n"
        f"STDOUT:\n{proc.stdout[-800:]}\nSTDERR:\n{proc.stderr[-1500:]}"
    )
    return json.loads(proc.stdout)


# =========================================================================
# Arm 1 — POPULATED: typed fields land verbatim in the emitted frontmatter.
# =========================================================================
def test_S1_structured_input_lands_in_frontmatter(tmp_path):
    """Feed a structured learning; assert the typed fields appear in the emitted
    YAML frontmatter, correctly parsed: ``fix`` / ``rule`` verbatim, ``entities``
    as a list, ``causal_relations`` as a list-of-dicts (valid structured YAML,
    not a stringified python dict), and ``problem`` as the deterministically
    derived one-liner — while the full prose stays in the body."""
    out = _drain_in_isolated_project(tmp_path, _STRUCTURED_INPUT)
    fm = out["frontmatter"]
    body = out["body"]

    # The bead's headline LLM-authored fields land verbatim.
    assert fm.get("rule") == _STRUCTURED_INPUT["rule"], (
        f"`rule` must be written verbatim to frontmatter; got {fm.get('rule')!r}"
    )
    assert fm.get("fix") == _STRUCTURED_INPUT["fix"], (
        f"`fix` must be written verbatim to frontmatter; got {fm.get('fix')!r}"
    )

    # ``entities`` survives as a real list (not a stringified blob).
    assert fm.get("entities") == _STRUCTURED_INPUT["entities"], (
        f"`entities` must round-trip as a YAML list; got {fm.get('entities')!r}"
    )

    # ``causal_relations`` is JSON-encoded so it parses back as a list of dicts —
    # the S1 fix that made dict-valued lists valid YAML. A stringified python
    # dict would NOT yaml.safe_load back to this structure.
    assert fm.get("causal_relations") == _STRUCTURED_INPUT["causal_relations"], (
        "`causal_relations` must round-trip as a list of dicts (valid structured "
        f"YAML via JSON encoding); got {fm.get('causal_relations')!r}"
    )

    # ``problem`` is the deterministically derived one-liner (first sentence),
    # NOT the whole multi-sentence prose — proving a parser, not an LLM summary.
    assert fm.get("problem") == _EXPECTED_PROBLEM_ONE_LINER, (
        "`problem` frontmatter must be the first-sentence one-liner derived by "
        f"_one_liner; got {fm.get('problem')!r}"
    )

    # The full prose rationale stays in the human-readable body, untouched.
    assert "## Problem" in body and "## Solution" in body, (
        "the prose body sections must remain; fields complement, not replace"
    )
    assert "the interpreter waits on it at shutdown" in body, (
        "the full multi-sentence problem prose must stay in the body even though "
        "the frontmatter `problem` is just the one-liner"
    )


# =========================================================================
# Arm 2 — ABSENT: omitted structured inputs leave the keys absent (no fill).
# This is the decisive contrast: the typed fields exist IFF the input had them.
# =========================================================================
def test_S1_absent_structured_input_leaves_fields_absent(tmp_path):
    """Feed the SAME prose with the structured args omitted (legacy call shape).
    Assert the typed keys are ABSENT from frontmatter — not empty-filled, not
    hallucinated. Only the derived ``problem`` line appears. This proves the port
    extracts what the input carries and never fabricates a missing field."""
    out = _drain_in_isolated_project(tmp_path, _CONTROL_INPUT)
    fm = out["frontmatter"]

    for key in ("fix", "rule", "entities", "causal_relations"):
        assert key not in fm, (
            f"`{key}` must be ABSENT from frontmatter when the input omits it "
            f"(no empty-string fill, no hallucination); got {fm.get(key)!r}. "
            f"frontmatter keys: {sorted(fm)}"
        )

    # The derived problem one-liner is still present (always auto-derived from
    # the prose), proving the absence above is selective, not a total no-op.
    assert fm.get("problem") == _EXPECTED_PROBLEM_ONE_LINER, (
        "the auto-derived `problem` one-liner must still be written even in the "
        f"legacy shape; got {fm.get('problem')!r}"
    )


# =========================================================================
# Arm 3 — CONTRAST IS PER-FIELD: each typed field tracks ITS OWN input.
# Supplying only `rule` (not `fix`/`entities`/`causal_relations`) emits exactly
# `rule` and leaves the rest absent — the presence is field-by-field, not a
# blanket "structured mode" switch.
# =========================================================================
def test_S1_partial_input_emits_only_supplied_fields(tmp_path):
    """Supply ONLY ``rule`` of the new structured fields. Assert ``rule`` lands
    while ``fix`` / ``entities`` / ``causal_relations`` stay absent — proving the
    extraction is per-field (each typed field is caused by its own argument),
    not an all-or-nothing toggle."""
    partial = dict(_CONTROL_INPUT)
    partial["title"] = "S1 partial drain note"
    partial["rule"] = "Never share KB state across proof arms"

    out = _drain_in_isolated_project(tmp_path, partial)
    fm = out["frontmatter"]

    assert fm.get("rule") == "Never share KB state across proof arms", (
        f"the supplied `rule` must land; got {fm.get('rule')!r}"
    )
    for key in ("fix", "entities", "causal_relations"):
        assert key not in fm, (
            f"`{key}` was NOT supplied and must stay absent even though `rule` "
            f"was supplied (per-field extraction, not a blanket switch); "
            f"got {fm.get(key)!r}. frontmatter keys: {sorted(fm)}"
        )


if __name__ == "__main__":
    import pytest

    raise SystemExit(pytest.main([__file__, "-v"]))
