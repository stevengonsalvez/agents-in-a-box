# ABOUTME: Behavioral proof for port SG5 — loop_detector.record_call deterministically
# ABOUTME: flags a stuck agent's tool loop (3x identical repeat / A-B-A-B oscillation) so the
# ABOUTME: mini-learning watcher can arm on a SUCCESSFUL loop, not only on tool failure.
"""SG5 tool-loop detection proof (capture/signal port, NOT a retrieval port).

Port SG5 lives in ``plugins/reflect/scripts/loop_detector.py`` (commit b8fb45e5),
a stdlib-only module that the PostToolUse hook calls before its failure check.
``recall.py`` has NO reference to it — the signal is produced entirely at capture
time. So the behavioral_kb retrieval fixture is the WRONG surface here: there is
nothing to rank, the invariant is "did the detector fire on the loop pattern".
This proof drives the REAL module directly (no mock, no stub, no torch — fast).

The supplied hypothesis said ``record_call`` returns a "flag boolean". The real
diff returns ``Optional[LoopHit]`` — a dict ``{"kind","tool","count"}`` on
detection, else ``None``. The invariant is corrected against the real code:

  detector constants (the port's knobs): WINDOW=10, REPEAT_N=3, OSC_CYCLES=2.

INVARIANT (seeds + the threshold knobs fully determine each outcome — no LLM
runs in the assertion; ``record_call`` is a pure deterministic state machine):

  1. BENIGN (no fire): a varied sequence of distinct (tool, input) pairs never
     trips the window — every call returns ``None``. This is the control: a
     normally-progressing agent produces no loop signal.

  2. REPEAT fires (port ON): the SAME (session, tool, arg-hash) appearing
     REPEAT_N=3 times consecutively returns a LoopHit with kind="repeat". The
     first two identical calls return None; only the 3rd — crossing the
     threshold — fires. This pins the threshold, not mere "saw it twice".

  3. ARG-HASH DISCRIMINATION (falsifiable knob): three calls to the SAME tool
     with DIFFERENT inputs do NOT fire. The loop is keyed on (tool, arg_hash),
     so changing the argument breaks the repeat. This proves the detector keys
     on input identity (the port's design), not on tool name alone — if it
     keyed on tool only, this control would false-positive.

  4. OSCILLATION fires (port ON): an A,B,A,B tail (OSC_CYCLES=2 full cycles,
     A != B) returns a LoopHit with kind="oscillation" naming both tools. A
     sub-threshold A,B,A (only 3 calls) does NOT fire.

  5. WINDOW RESET (port ON): after a detection the window is cleared, so one
     stuck stretch arms ONCE — the call immediately after a hit returns None,
     and it takes a fresh REPEAT_N run to re-arm. (Without this the hook would
     arm on every subsequent identical call.)

Falsifiability: if SG5 were absent or the thresholds wrong, (2)/(4) would return
None (no signal), and (1)/(3) would have nothing to contrast against. If the
detector keyed on tool-name only, (3) would false-fire. If reset were dropped,
(5)'s "immediately after a hit returns None" assertion would fail.

Surface used: signal (real loop_detector module), not the behavioral_kb
retrieval fixture — see above. No torch model is loaded; this proof is fast.

PORT: SG5
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# loop_detector lives in the reflect plugin scripts dir. Resolve it the same way
# the M6 / SG1 capture-layer proofs resolve their modules so this runs from the
# repo layout regardless of cwd.
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

import loop_detector  # noqa: E402
from loop_detector import record_call, REPEAT_N, OSC_CYCLES  # noqa: E402


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    """Point the detector's on-disk window state at a throwaway dir so this proof
    never touches the developer's ~/.reflect/loops/ state, and each test starts
    from an empty window."""
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    yield tmp_path


def test_SG5_benign_varied_sequence_does_not_fire():
    """(1) CONTROL: a normally-progressing agent (all distinct tool/input pairs)
    produces NO loop signal — every record_call returns None."""
    sid = "benign-session"
    seq = [
        ("Read", {"file": "a.py"}),
        ("Grep", {"q": "config"}),
        ("Edit", {"file": "a.py", "to": "x"}),
        ("Bash", {"command": "pytest"}),
        ("Read", {"file": "b.py"}),
        ("Write", {"file": "c.py", "body": "..."}),
    ]
    for tool, inp in seq:
        hit = record_call(sid, tool, inp)
        assert hit is None, (
            f"a varied, progressing sequence must not trip the loop detector; "
            f"{tool}{inp} unexpectedly fired {hit!r}"
        )


def test_SG5_three_identical_calls_fire_repeat_at_threshold():
    """(2) PORT ON: REPEAT_N=3 identical (tool, arg-hash) calls fire kind=repeat.
    The first two return None — only crossing the threshold fires."""
    assert REPEAT_N == 3, "proof pins the shipped repeat threshold"
    sid = "repeat-session"
    inp = {"command": "ls -la"}

    assert record_call(sid, "Bash", inp) is None, "1st identical call must not fire"
    assert record_call(sid, "Bash", inp) is None, "2nd identical call must not fire"
    hit = record_call(sid, "Bash", inp)
    assert hit is not None, (
        "3rd identical (session,tool,input) call must fire — SG5 exists to catch "
        "exactly this stuck-repeat pattern"
    )
    assert hit["kind"] == "repeat", f"expected kind=repeat, got {hit!r}"
    assert hit["tool"] == "Bash", f"hit must name the looping tool, got {hit!r}"
    assert hit["count"] == REPEAT_N


def test_SG5_same_tool_different_args_does_not_fire():
    """(3) FALSIFIABLE KNOB: the detector keys on (tool, arg_hash), not tool name.
    Three SAME-tool calls with DIFFERENT inputs must NOT fire. If it keyed on the
    tool alone (the wrong design), this would false-positive — so this control
    isolates arg-hash identity as the cause of the repeat in test (2)."""
    sid = "varied-args-session"
    assert record_call(sid, "Bash", {"command": "ls a"}) is None
    assert record_call(sid, "Bash", {"command": "ls b"}) is None
    hit = record_call(sid, "Bash", {"command": "ls c"})
    assert hit is None, (
        "same tool with different inputs is NOT a loop; the detector must key on "
        f"(tool, arg-hash). Got false-positive {hit!r}"
    )


def test_SG5_oscillation_fires_and_subthreshold_does_not():
    """(4) PORT ON: an A,B,A,B tail (OSC_CYCLES=2 cycles, A != B) fires
    kind=oscillation naming both tools; a sub-threshold A,B,A does not."""
    assert OSC_CYCLES == 2, "proof pins the shipped oscillation cycle count"
    a = ("Read", {"file": "x.py"})
    b = ("Edit", {"file": "x.py", "patch": "y"})

    # Sub-threshold: A,B,A (3 calls) — not yet 2 full cycles.
    sid_lo = "osc-sub"
    assert record_call(sid_lo, *a) is None
    assert record_call(sid_lo, *b) is None
    assert record_call(sid_lo, *a) is None, "A,B,A is below the oscillation threshold"

    # Threshold: A,B,A,B — the 4th call closes the 2nd cycle and fires.
    sid = "osc-session"
    assert record_call(sid, *a) is None
    assert record_call(sid, *b) is None
    assert record_call(sid, *a) is None
    hit = record_call(sid, *b)
    assert hit is not None, "A,B,A,B must fire the oscillation signal"
    assert hit["kind"] == "oscillation", f"expected kind=oscillation, got {hit!r}"
    assert "Read" in hit["tool"] and "Edit" in hit["tool"], (
        f"oscillation hit must name both ping-ponged tools, got {hit!r}"
    )


def test_SG5_window_resets_after_detection_arms_once():
    """(5) PORT ON: after a hit the window is cleared so one stuck stretch arms
    ONCE — the very next identical call returns None, and re-arming takes a fresh
    REPEAT_N run. Without reset the hook would fire on every later identical call."""
    sid = "reset-session"
    inp = {"command": "retry"}

    assert record_call(sid, "Bash", inp) is None
    assert record_call(sid, "Bash", inp) is None
    assert record_call(sid, "Bash", inp) is not None, "3rd call fires"

    # Window reset: the immediately-following identical call must NOT re-fire.
    assert record_call(sid, "Bash", inp) is None, (
        "after detection the window must reset so the loop arms once, not on "
        "every subsequent identical call"
    )
    assert record_call(sid, "Bash", inp) is None
    assert record_call(sid, "Bash", inp) is not None, (
        "a fresh REPEAT_N run must be required to re-arm"
    )


def test_SG5_signal_is_keyed_per_session():
    """Cross-check: the window is per session_id, so identical calls split across
    two sessions never accumulate into a loop. This pins the (session, tool, input)
    keying the hypothesis named, and confirms one agent's calls can't trip another's
    signal."""
    inp = {"command": "build"}
    # Interleave two sessions; each only sees 2 of its own calls -> no fire.
    for _ in range(2):
        assert record_call("sess-A", "Bash", inp) is None
        assert record_call("sess-B", "Bash", inp) is None
    # A third call to A alone crosses ITS threshold (A has now seen 3).
    assert record_call("sess-A", "Bash", inp) is not None, (
        "session A's own 3rd identical call fires; B's interleaved calls neither "
        "helped nor blocked it"
    )
    # State is persisted on disk per session (each PostToolUse is a fresh process).
    state_dir = Path(loop_detector._state_dir())
    assert (state_dir / "sess-B.json").exists(), "per-session state file must exist"
    calls_b = json.loads((state_dir / "sess-B.json").read_text())["calls"]
    assert len(calls_b) == 2, "session B accumulated only its own 2 calls"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
