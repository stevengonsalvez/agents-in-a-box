# ABOUTME: Behavioral proof for port M2 — output_classifier.classify buckets writer stdout and
# ABOUTME: track() is a respawn circuit breaker that bounds retries (3 consecutive invalids or 1 poison).
"""M2 writer-output classifier + respawn circuit-breaker proof (capture port, NOT retrieval).

Port M2 lives in ``plugins/reflect/scripts/output_classifier.py`` (commit
ec91c7fb), a stdlib-only module the drain hook (``reflect-drain-bg.sh``) calls
per writer subprocess. ``recall.py`` and the GraphRAG engine have NO reference to
it — the signal is produced entirely at *capture* time, when the drainer
classifies each ``claude -p --output-format json`` writer's stdout and decides
whether to keep feeding a drifting writer or kill + archive the entry. So the
behavioral_kb retrieval fixture is the WRONG surface: there is nothing to rank.
This proof drives the REAL module directly (no mock, no stub, no torch — fast),
and NO LLM runs in any assertion — ``classify`` and ``track`` are pure
deterministic functions / a JSONL-replay state machine.

The supplied hypothesis said "malformed/empty output is classified as bad and
trips a respawn breaker that bounds retries". The real diff is more precise; the
invariant is corrected against the shipped code:

  * classify(raw) -> EXACTLY one of CATEGORIES = (valid, prose, idle, poisoned,
    malformed). A parseable result envelope is "valid"; empty/whitespace is
    "idle"; a known wedge marker ("prompt is too long", …) is "poisoned"; broken
    or wrong-shape JSON is "malformed"; anything else is "prose".
  * track(state, transcript, category, threshold) is the breaker. It replays the
    per-transcript consecutive-invalid streak from a JSONL sidecar and returns a
    WriterHealth verdict. Knob: threshold = REFLECT_DRAIN_INVALID_THRESHOLD
    (default DEFAULT_THRESHOLD = 3).

INVARIANTS (seeds + the threshold knob fully determine each outcome):

  A. CLASSIFY closed set — good output is "valid", and each bad shape lands in
     its own bucket. A healthy envelope that merely *mentions* a wedge marker in
     its summary text is still "valid" (only an is_error envelope carrying the
     marker is "poisoned") — this pins the structural gate, not text luck.

  B. GOOD OUTPUT does not respawn: a valid classification leaves consecutive=0,
     respawn=False. This is the control the breaker contrasts against.

  C. BREAKER BOUNDS RETRIES (port ON): feeding the SAME transcript repeated bad
     output trips respawn=True at exactly the threshold-th consecutive invalid —
     the first (threshold-1) return respawn=False, only the threshold-th fires.
     This bounds the respawn loop at its limit instead of retrying forever.

  D. THRESHOLD IS A KNOB (falsifiable, proves M2 caused it): with
     REFLECT_DRAIN_INVALID_THRESHOLD=2 the breaker trips after 2 (not 3); with
     =5 the same two bad outputs do NOT trip. Same seed, different knob, opposite
     verdict — so the respawn is the port's doing, not an artifact of the input.

  E. POISON TRIPS IMMEDIATELY: a single "poisoned" classification respawns on
     sighting 1 (deterministic wedge; retries are pure waste).

  F. VALID RESETS THE STREAK: a partial invalid streak followed by a valid
     output resets consecutive to 0, so transient drift that recovers does not
     accumulate toward a respawn.

  G. RESPAWN RESETS PAST THE ARCHIVE: after a trip the streak does NOT survive —
     the next bad output on the same transcript starts a fresh streak at 1, not
     at threshold+1. (Without the reset record the breaker would re-fire on
     every subsequent output forever.)

Falsifiability: if M2 were absent/broken, (C)/(E) would never report respawn=True
(the writer would fail silently, the exact bug M2 fixes); (D) would not flip with
the env knob; (F)/(G) would let the streak run away. If classify lacked the
envelope gate, (A)'s healthy-envelope-mentioning-a-marker case would be
mis-bucketed "poisoned" and false-trip the breaker.

Surface used: capture/signal (real output_classifier module), not the
behavioral_kb retrieval fixture — see above. No torch model is loaded; fast.

PORT: M2
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# output_classifier lives in the reflect plugin scripts dir. Resolve it the same
# way the SG5 / M6 capture-layer proofs resolve their modules so this runs from
# the repo layout regardless of cwd.
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

import output_classifier as oc  # noqa: E402
from output_classifier import (  # noqa: E402
    CATEGORIES,
    DEFAULT_THRESHOLD,
    classify,
    default_threshold,
    track,
)

# A healthy result envelope is the shape `claude -p --output-format json` prints.
GOOD_ENVELOPE = json.dumps(
    {"type": "result", "is_error": False, "result": "Captured one learning about retries."}
)


@pytest.fixture(autouse=True)
def _clean_knob(monkeypatch):
    """Ensure the threshold knob starts unset so the default (3) is in force unless
    a test explicitly sets it. Keeps each test hermetic from the developer's env."""
    monkeypatch.delenv("REFLECT_DRAIN_INVALID_THRESHOLD", raising=False)
    yield


# --------------------------------------------------------------------------- A
def test_M2_classify_closed_set_buckets_each_shape():
    """(A) classify returns exactly one CATEGORY per input, and each writer-output
    shape lands in its designed bucket. Good output -> valid; bad shapes -> their
    own buckets — this is the structural gate the breaker keys on."""
    cases = {
        GOOD_ENVELOPE: "valid",
        json.dumps({"is_error": True, "result": "tool failed"}): "valid",  # envelope, success is drainer's call
        "   \n  ": "idle",
        "": "idle",
        "Sure! Here is a summary of what I learned today.": "prose",
        '{"type":"result","is_er': "malformed",   # truncated envelope (JSON-intent, broken)
        "[1, 2, 3]": "malformed",                  # parseable JSON, wrong shape
        '"just a string"': "malformed",            # parseable JSON, not an envelope
        "Prompt is too long for this session.": "poisoned",
        "The context window has been exhausted.": "poisoned",
    }
    for raw, expected in cases.items():
        got = classify(raw)
        assert got in CATEGORIES, f"classify must return a closed-set category, got {got!r}"
        assert got == expected, f"classify({raw!r:.40}) -> {got!r}, expected {expected!r}"


def test_M2_healthy_envelope_mentioning_marker_is_not_poisoned():
    """(A, structural gate): a SUCCESSFUL envelope whose summary text merely
    mentions a wedge marker is "valid", not "poisoned" — only an is_error
    envelope carrying the marker is poisoned. Without this gate a good learning
    about context windows would false-trip the breaker."""
    healthy = json.dumps(
        {"type": "result", "is_error": False,
         "result": "Learned: keep the prompt is too long error in mind when chunking."}
    )
    assert classify(healthy) == "valid", "healthy envelope must not be mis-flagged poisoned"

    error_wedge = json.dumps({"is_error": True, "result": "Prompt is too long"})
    assert classify(error_wedge) == "poisoned", "error envelope carrying the marker IS poisoned"


# --------------------------------------------------------------------------- B
def test_M2_good_output_does_not_respawn(tmp_path):
    """(B) CONTROL: classifying a good envelope as valid leaves the breaker idle —
    consecutive=0, respawn=False. A live, well-behaved writer never trips."""
    state = tmp_path / "writer-health.jsonl"
    cat = classify(GOOD_ENVELOPE)
    assert cat == "valid"
    h = track(state, "transcript-good.jsonl", cat)
    assert h.respawn is False, "valid writer output must never trip the respawn breaker"
    assert h.consecutive == 0, "valid output keeps the invalid streak at zero"
    assert h.category == "valid"


# --------------------------------------------------------------------------- C
def test_M2_breaker_trips_at_default_threshold_bounding_retries(tmp_path):
    """(C) PORT ON: repeated bad output on ONE transcript trips respawn at exactly
    the DEFAULT_THRESHOLD-th consecutive invalid — the loop is bounded, not
    infinite. The first threshold-1 outputs return respawn=False; only the
    threshold-th fires. This is the liveness fix: a drifting writer used to retry
    silently forever."""
    assert DEFAULT_THRESHOLD == 3, "proof pins the shipped default threshold"
    state = tmp_path / "writer-health.jsonl"
    transcript = "transcript-drift.jsonl"
    # Realistic mix of bad shapes a drifting writer emits, each classified live.
    bad_outputs = [
        "I think I should summarize the learnings now.",  # prose
        "",                                               # idle (timeout/killed)
        '{"type":"result", "is_er',                       # malformed (truncated)
    ]
    verdicts = []
    for raw in bad_outputs:
        cat = classify(raw)
        assert cat != "valid", f"{raw!r:.30} must classify as bad, got {cat}"
        verdicts.append(track(state, transcript, cat))

    assert verdicts[0].respawn is False and verdicts[0].consecutive == 1
    assert verdicts[1].respawn is False and verdicts[1].consecutive == 2
    assert verdicts[2].respawn is True, (
        "the 3rd consecutive invalid must trip the respawn breaker — M2 exists to "
        "stop the drifting writer at the bound instead of retrying forever"
    )
    assert verdicts[2].consecutive == DEFAULT_THRESHOLD
    # The breaker reports WHICH outputs offended, so the drain envelope can show health.
    assert verdicts[2].categories == ["prose", "idle", "malformed"], (
        f"breaker must carry the offending categories, got {verdicts[2].categories!r}"
    )


# --------------------------------------------------------------------------- D
def test_M2_threshold_is_an_env_knob_flips_the_verdict(tmp_path, monkeypatch):
    """(D) FALSIFIABLE KNOB: the SAME two bad outputs trip the breaker under
    REFLECT_DRAIN_INVALID_THRESHOLD=2 but NOT under =5. Same seed, opposite
    verdict driven solely by the env flag from the M2 diff — this proves the
    respawn is the PORT's doing, not an artifact of the inputs."""
    # Knob = 2 -> trips on the 2nd consecutive invalid.
    monkeypatch.setenv("REFLECT_DRAIN_INVALID_THRESHOLD", "2")
    assert default_threshold() == 2, "env knob must lower the threshold"
    state_lo = tmp_path / "knob-2.jsonl"
    t = "transcript-knob.jsonl"
    v1 = track(state_lo, t, classify("prose drift one"))
    v2 = track(state_lo, t, classify("prose drift two"))
    assert v1.respawn is False, "1st invalid under threshold=2 must not trip"
    assert v2.respawn is True, "2nd invalid under threshold=2 MUST trip (knob lowered the bound)"

    # Knob = 5 -> the exact same two bad outputs do NOT trip.
    monkeypatch.setenv("REFLECT_DRAIN_INVALID_THRESHOLD", "5")
    assert default_threshold() == 5
    state_hi = tmp_path / "knob-5.jsonl"
    w1 = track(state_hi, t, classify("prose drift one"))
    w2 = track(state_hi, t, classify("prose drift two"))
    assert w1.respawn is False and w2.respawn is False, (
        "under threshold=5 the same two bad outputs must NOT trip — flipping the "
        "knob flips the verdict, so the breaker, not the input, decides respawn"
    )
    assert w2.consecutive == 2, "streak still accrues; it just hasn't reached the higher bound"


# --------------------------------------------------------------------------- E
def test_M2_single_poison_trips_immediately(tmp_path):
    """(E) PORT ON: a single "poisoned" output (a wedged session — deterministic,
    retrying is waste) respawns on sighting 1, well below the consecutive
    threshold. Bounds the loop at the first deterministic failure."""
    state = tmp_path / "writer-health.jsonl"
    cat = classify("API Error: Prompt is too long")
    assert cat == "poisoned"
    h = track(state, "transcript-wedged.jsonl", cat)
    assert h.respawn is True, "one poisoned output must trip immediately, not after 3"
    assert h.consecutive == 1, "poison trips on the first sighting"


# --------------------------------------------------------------------------- F
def test_M2_valid_output_resets_partial_streak(tmp_path):
    """(F) PORT ON: a valid output in the middle of a partial invalid streak resets
    consecutive to 0, so transient drift that recovers never accumulates toward a
    respawn. Without the reset, intermittent badness would eventually false-trip."""
    state = tmp_path / "writer-health.jsonl"
    t = "transcript-recover.jsonl"
    assert track(state, t, classify("prose blah")).consecutive == 1
    assert track(state, t, classify("[1,2]")).consecutive == 2  # malformed
    recovered = track(state, t, classify(GOOD_ENVELOPE))
    assert recovered.category == "valid"
    assert recovered.consecutive == 0, "valid output must reset the invalid streak"
    assert recovered.categories == []
    # And a fresh bad output after recovery starts at 1, not 3.
    after = track(state, t, classify("prose again"))
    assert after.consecutive == 1 and after.respawn is False, (
        "after a valid reset the next bad output starts a fresh streak, not where "
        "the pre-reset streak left off"
    )


# --------------------------------------------------------------------------- G
def test_M2_respawn_resets_streak_past_the_archive(tmp_path):
    """(G) PORT ON: after the breaker trips, the streak does NOT survive — the next
    bad output on the same transcript starts fresh at 1, not threshold+1. Without
    this reset record the breaker would re-fire on every subsequent output."""
    state = tmp_path / "writer-health.jsonl"
    t = "transcript-archive.jsonl"
    for _ in range(DEFAULT_THRESHOLD - 1):
        assert track(state, t, "malformed").respawn is False
    tripped = track(state, t, "malformed")
    assert tripped.respawn is True and tripped.consecutive == DEFAULT_THRESHOLD

    # Post-archive: the re-enqueued transcript starts from a clean slate.
    after = track(state, t, "malformed")
    assert after.consecutive == 1, (
        "after a respawn the streak must reset so the breaker doesn't re-fire on "
        f"every later output; got consecutive={after.consecutive}"
    )
    assert after.respawn is False, "first post-archive bad output must not immediately re-trip"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
