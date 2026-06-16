# ABOUTME: Behavioral proof for port SG4 — test_outcome_parser parses pass/fail from a Bash
# ABOUTME: tool_response (pytest/jest/cargo/go) and a per-session state machine turns the
# ABOUTME: failure-count transition (N->0 fix, 0->N regression) into a deterministic memory signal.
"""SG4 test-outcome parsing proof (capture/signal port, NOT a retrieval port).

Port SG4 lives in ``plugins/reflect/scripts/test_outcome_parser.py`` (commit
ac44601d) and is wired into ``plugins/reflect/hooks/posttooluse_minilearning.py``
(it imports ``observe_bash`` and stuffs the result into the hook payload as
``test_outcome``). ``recall.py`` never touches it — the signal is produced
entirely at capture time from the Bash tool's output. So the behavioral_kb
retrieval fixture is the WRONG surface here: there is nothing to rank. The
invariant is "did the parser detect the right test outcome, and did the state
machine emit the right transition signal". This proof drives the REAL module
directly (no mock, no stub, no torch — fast).

The supplied hypothesis said the path "parses pass/fail into a capture signal".
Corrected against the real diff, the port has TWO deterministic layers and a
session-keyed state machine sits between them:

  * ``parse_test_output(text)`` -> ``TestOutcome(runner, passed, failed) | None``.
    Summary-anchored regexes per runner; prose that merely says "passed" does
    NOT match (it requires the runner's summary shape, e.g. pytest's
    ``N failed, M passed in T s`` line).
  * ``record_outcome(session, outcome)`` -> transition dict. A run is only a
    "fix" (kind=fix) when failures went N->0 vs the PREVIOUS run in the same
    session; a "regression" (kind=regression) when 0->N. The very first run in a
    session has no prior, so it never transitions.
  * ``observe_bash`` ties them together and, on a fix, writes a HIGH-confidence
    ``source: test-outcome`` learning to disk.

INVARIANT (the raw Bash output text + the prior session state FULLY determine
every outcome — no LLM runs anywhere in capture or in these assertions; the
parser is pure regex and the state machine is a pure per-session counter):

  1. FAIL DETECTED: a failing pytest summary parses to failed>0 (the exact
     counts), and through ``observe_bash`` a passing->failing sequence yields a
     ``regression`` signal naming new_failed=N.
  2. PASS DETECTED + FIX SIGNAL: a passing summary parses to failed==0, and a
     failing->passing sequence yields a ``fix`` signal AND writes the
     high-confidence ``source: test-outcome`` learning to disk.
  3. NON-TEST OUTPUT -> NO SIGNAL (the decisive control): Bash output that is
     not a test-runner summary (git status, a build log that merely contains the
     word "passed") parses to ``None`` and ``observe_bash`` returns ``None`` —
     no test signal at all. This is the falsifiable contrast: if the parser
     keyed on the bare word "passed"/"failed" instead of the runner summary
     shape, this control would false-fire.
  4. THRESHOLD / FIRST-RUN: the first run in a session never transitions (no
     prior to compare against); a flat fail->fail or pass->pass sequence emits
     no fix/regression. Only an actual N<->0 crossing fires. This pins the
     transition on the state change, not on the mere presence of a parse.

Falsifiability: if SG4 were absent or the parser keyed on loose words, (3) would
false-fire and (1)/(2) would have no clean control to contrast against. If the
state machine fired on every parse rather than on the crossing, (4) would fail.
If fix detection were inverted, (2)'s on-disk learning would not appear.

Surface used: signal (real test_outcome_parser module), not the behavioral_kb
retrieval fixture — see above. No torch model is loaded; this proof is fast.

PORT: SG4
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

# test_outcome_parser lives in the reflect plugin scripts dir. Resolve it the
# same way the SG5 / M6 capture-layer proofs resolve their modules so this runs
# from the repo layout regardless of cwd.
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

import test_outcome_parser as T  # noqa: E402
from test_outcome_parser import parse_test_output, observe_bash  # noqa: E402


# --- Real test-runner outputs (verbatim summary shapes) ----------------------

PYTEST_FAIL = (
    "============ test session starts ============\n"
    "FAILED tests/test_x.py::test_a - AssertionError\n"
    "FAILED tests/test_x.py::test_b - AssertionError\n"
    "============ 2 failed, 5 passed in 1.23s ============"
)
PYTEST_PASS = "==================== 7 passed in 0.45s ===================="
JEST_FAIL = "Tests:       1 failed, 2 passed, 3 total"

# Decisive controls: NOT test-runner summaries even though they contain the
# words "passed"/"failed".
GIT_STATUS = "On branch main\nnothing to commit, working tree clean"
BUILD_LOG = "Build passed successfully after 3 steps; 0 warnings"


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    """Point the parser's on-disk session state AND learnings dir at throwaway
    dirs so this proof never touches the developer's ~/.reflect/ or ~/.learnings/
    state, and each test starts from an empty session history."""
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setenv("REFLECT_LEARNINGS_DIR", str(tmp_path / "learnings"))
    yield tmp_path


# --- (1) FAIL detected + regression signal -----------------------------------

def test_SG4_failing_pytest_summary_parses_failed_count():
    """(1a) PORT ON: a failing pytest summary parses to the exact (passed, failed)
    counts with runner=pytest. This is pure regex — no LLM, no state."""
    out = parse_test_output(PYTEST_FAIL)
    assert out is not None, "a real pytest failure summary must parse"
    assert out.runner == "pytest", f"expected runner=pytest, got {out!r}"
    assert out.failed == 2, f"must extract the failed count, got {out!r}"
    assert out.passed == 5, f"must extract the passed count, got {out!r}"


def test_SG4_pass_then_fail_emits_regression_signal():
    """(1b) PORT ON: within one session a passing run followed by a failing run
    is a regression (0->N). observe_bash returns kind=regression naming new_failed.
    The seeds (two Bash outputs) fully determine this — no inference."""
    sid = "regress-session"
    ti = {"command": "pytest -q"}

    first = observe_bash(sid, ti, {"stdout": PYTEST_PASS})
    assert first is None, "first run has no prior to compare — no transition"

    hit = observe_bash(sid, ti, {"stdout": PYTEST_FAIL})
    assert hit is not None, "0->N failures within a session is a regression signal"
    assert hit["kind"] == "regression", f"expected kind=regression, got {hit!r}"
    assert hit["new_failed"] == 2, f"regression must name the new failure count, got {hit!r}"
    assert hit["prev_failed"] == 0, f"regression's prior was a clean run, got {hit!r}"


# --- (2) PASS detected + fix signal + on-disk learning -----------------------

def test_SG4_passing_summary_parses_zero_failed():
    """(2a) PORT ON: a passing pytest summary parses to failed==0."""
    out = parse_test_output(PYTEST_PASS)
    assert out is not None, "a real pytest pass summary must parse"
    assert out.failed == 0, f"a green run has zero failures, got {out!r}"
    assert out.passed == 7, f"must extract the passed count, got {out!r}"


def test_SG4_fail_then_pass_emits_fix_and_writes_high_confidence_learning(tmp_path):
    """(2b) PORT ON: within one session a failing run followed by a passing run is
    a FIX (N->0). observe_bash returns kind=fix AND writes a high-confidence
    source=test-outcome learning to disk. The test runner *proved* the fix, so the
    confidence is fixed by the transition, not by any LLM judgement."""
    sid = "fix-session"
    ti = {"command": "pytest -q tests/"}

    first = observe_bash(sid, ti, {"stdout": PYTEST_FAIL})
    assert first is None, "first run has no prior — no transition yet"

    hit = observe_bash(sid, ti, {"stdout": PYTEST_PASS})
    assert hit is not None, "N->0 failures within a session is a fix signal"
    assert hit["kind"] == "fix", f"expected kind=fix, got {hit!r}"
    assert hit["runner"] == "pytest"
    assert hit["failed"] == 0 and hit["passed"] == 7

    # The fix learning was written to the (isolated) learnings dir with the
    # port's HIGH-confidence, test-outcome-sourced frontmatter.
    slug = hit.get("learning")
    assert slug, f"a fix must produce a learning slug, got {hit!r}"
    ld = Path(os.environ["REFLECT_LEARNINGS_DIR"])
    note = ld / f"{slug}.md"
    assert note.exists(), f"the fix learning file must exist at {note}"
    body = note.read_text(encoding="utf-8")
    assert "confidence: high" in body, "fix learning must be high-confidence"
    assert "source: test-outcome" in body, "fix learning must be sourced from the test outcome"


# --- (3) DECISIVE CONTROL: non-test output yields NO signal -------------------

@pytest.mark.parametrize("text", [GIT_STATUS, BUILD_LOG], ids=["git-status", "build-log-passed"])
def test_SG4_non_test_output_parses_to_none(text):
    """(3a) CONTROL: Bash output that is not a test-runner summary parses to None,
    even when it literally contains the word 'passed'. This isolates the
    summary-shape anchoring as the cause of the parse in (1)/(2): a looser
    'passed'/'failed' word match would false-fire here."""
    assert parse_test_output(text) is None, (
        f"non-test output must NOT be parsed as a test outcome: {text!r}"
    )


@pytest.mark.parametrize("text", [GIT_STATUS, BUILD_LOG], ids=["git-status", "build-log-passed"])
def test_SG4_non_test_output_emits_no_signal_through_hook(text):
    """(3b) CONTROL: through the real observe_bash hook entry point, non-test Bash
    output produces NO test signal (returns None) — so the PostToolUse hook arms
    nothing and writes no learning for non-test commands."""
    hit = observe_bash("control-session", {"command": "git status"}, {"stdout": text})
    assert hit is None, f"non-test Bash output must not produce a test signal, got {hit!r}"


def test_SG4_jest_runner_also_detected():
    """(3c) Cross-runner sanity: the jest summary shape is detected too (failed=1),
    proving the port is multi-runner, not pytest-only."""
    out = parse_test_output(JEST_FAIL)
    assert out is not None and out.runner == "jest", f"jest summary must parse, got {out!r}"
    assert out.failed == 1 and out.passed == 2, f"jest counts must extract, got {out!r}"


# --- (4) THRESHOLD / first-run + flat sequences do not transition ------------

def test_SG4_first_run_and_flat_sequences_do_not_transition():
    """(4) PORT ON: the state machine fires ONLY on a failure-count crossing, not
    on the mere presence of a parse. The first run never transitions (no prior),
    and a flat fail->fail or pass->pass sequence emits no fix/regression. This
    pins the signal on the state change, not on "we parsed a test outcome"."""
    # First run in a fresh session: parses fine, but no prior -> no transition.
    sid = "flat-session"
    ti = {"command": "pytest"}
    assert observe_bash(sid, ti, {"stdout": PYTEST_FAIL}) is None, (
        "the first parsed run has no prior to compare against — no signal"
    )
    # fail -> fail: still failing, no crossing -> no signal.
    assert observe_bash(sid, ti, {"stdout": PYTEST_FAIL}) is None, (
        "fail -> fail is not a transition (no N->0 crossing)"
    )

    # pass -> pass in a clean session: never failed, so no fix/regression.
    sid2 = "green-session"
    assert observe_bash(sid2, ti, {"stdout": PYTEST_PASS}) is None
    assert observe_bash(sid2, ti, {"stdout": PYTEST_PASS}) is None, (
        "pass -> pass is not a transition (no 0->N crossing)"
    )


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
