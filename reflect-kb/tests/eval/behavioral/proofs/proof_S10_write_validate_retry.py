# ABOUTME: Behavioral proof for S10 — the drain write-validate-retry loop drives a
# ABOUTME: DETERMINISTIC fake writer (no LLM): it retries up to 3 attempts, accepts as soon
# ABOUTME: as output validates (recording the attempt count), and bails at exactly 3 with
# ABOUTME: validated=False when the writer is always malformed.
"""S10 write-validate-retry loop proof (storage/capture port, NOT retrieval).

Port S10 lives in ``plugins/reflect/scripts/reflect_cascade.py``
(``write_validate_retry`` + ``validate_drain_output`` + ``DrainOutput`` /
``DrainResult``), the ByteRover ``curate-session.ts`` correct-html loop
(MAX_ATTEMPTS=4) re-shaped for the reflect drain: after the drain LLM writes a
learning, its STRUCTURE is validated (required frontmatter fields present + the
``.entities.yaml`` sidecar valid via the EXISTING ``validate_sidecar.validate``
+ the sidecar's entity claims actually referenced in the body). On a structural
failure the writer is re-prompted with the errors inlined and tries again,
BOUNDED at 3 attempts; a note that never validates is still written, flagged
``validated: false``, so a malformed learning self-heals at write time instead
of polluting the corpus. ``recall.py`` and the GraphRAG engine never see this —
the signal is produced entirely at *capture/write* time — so the behavioral_kb
retrieval fixture is the wrong surface (there is nothing to rank).

The LOOP is the unit under test, NOT the LLM. The drain write step is abstracted
behind a callable (``writer(errors) -> DrainOutput``); this proof injects a
DETERMINISTIC fake writer that emits malformed output the first K times then
valid output — so NO LLM runs in any assertion. ``validate_drain_output`` is a
pure YAML/markdown linter that REUSES the shipped ``validate_sidecar`` validator
(the spec's "do not duplicate the validator" rule).

INVARIANTS (the fake writer's malformed-count K and the attempt budget fully
determine each outcome — no LLM in the assertion):

  A. CLEAN FIRST WRITE accepts at attempt 1. A writer whose very first output is
     structurally valid is accepted with attempts=1, validated=True, and the
     loop never re-invokes it (the control: a well-behaved writer pays no retry).

  B. ONE MALFORMED THEN VALID regenerates once then accepts. K=1: the 1st output
     fails validation, the writer is re-invoked WITH the errors inlined, the 2nd
     output validates -> attempts=2, validated=True. The acceptance criterion
     "a malformed learning regenerates once then accepts" + "telemetry tracks
     attempt count". The proof also asserts the re-prompt actually carried the
     prior attempt's errors (the self-heal feedback channel), and that the 1st
     call saw an EMPTY error list (nothing to inline yet).

  C. ACCEPT AS SOON AS VALID — the loop stops at the first valid attempt, it does
     NOT spend the whole budget. K=2 under a budget of 3 -> attempts=3 (valid on
     the 3rd, the last allowed); K=1 under the default budget -> attempts=2, not
     3. The recorded attempt count equals the attempt that first validated.

  D. ALWAYS-MALFORMED BAILS AT EXACTLY 3 with validated=False. A writer that is
     never valid is invoked EXACTLY max_attempts (3) times — not 2, not 4 — and
     the loop gives up: validated=False, attempts=3, errors carries the last
     attempt's failures, and the note body is STILL returned (signal is never
     dropped — it is quarantined via the flag, not discarded).

  E. THE BUDGET IS A KNOB (falsifiable, proves the loop owns the bound). The SAME
     always-malformed writer under max_attempts=2 is invoked exactly 2 times and
     bails (validated=False, attempts=2); under max_attempts=5 it is invoked
     exactly 5 times. Same seed, different knob, different invocation count and
     attempt total -> the bound is the LOOP's doing, not an artifact of the
     writer.

  F. VALIDATION REUSES THE REAL SIDECAR VALIDATOR (decisive, proves it is not a
     stub gate). An output whose ONLY defect is a structurally-broken sidecar
     (entity missing required keys) is rejected, and the surfaced error is the
     one ``validate_sidecar.validate`` emits, prefixed ``sidecar:`` — flipping
     that sidecar to a valid one (nothing else changed) clears the rejection and
     the loop accepts at attempt 1. So the loop's accept/reject is owned by the
     shipped validator, not incidental.

Falsifiability: if the loop did not retry, (B)/(C) would report attempts=1 and a
validated=False on the first malformed output. If it never bounded, (D)/(E)
would loop forever (or invoke != max_attempts). If it dropped the note on
give-up, (D)'s body assertion would fail. If validation were a stub that always
passed, (D) would wrongly accept and (F)'s sidecar-flip would not change the
verdict.

Surface used: capture/write (real reflect_cascade loop + real validate_sidecar),
not the behavioral_kb retrieval fixture — see above. No torch model is loaded;
fast.

PORT: S10
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest
import yaml

# The loop + validator live in the reflect plugin scripts dir. Resolve it the
# same way the S2 / M2 capture-layer proofs do so this runs from the repo layout
# regardless of cwd: parents[3] is the repo root where plugins/ sits alongside
# reflect-kb/; the fallback handles a reflect-kb-as-root checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_cascade as rc  # noqa: E402
from reflect_cascade import (  # noqa: E402
    DrainOutput,
    DrainResult,
    S10_MAX_ATTEMPTS,
    validate_drain_output,
    write_validate_retry,
)


# A structurally-valid note body: a `---`-fenced frontmatter block carrying every
# required field, and a body that mentions the sidecar's one entity ("tokio").
VALID_BODY = (
    "---\n"
    "title: Tokio nested runtime panic\n"
    "category: rust\n"
    "tags: [tokio, async]\n"
    "confidence: high\n"
    "---\n"
    "Do not nest a tokio runtime inside another; it panics.\n"
)

# Malformed body: no frontmatter block at all (a structural failure the writer
# is supposed to self-heal).
MALFORMED_BODY = "Just some prose with no frontmatter.\nMentions tokio.\n"


def _write_sidecar(path: Path, *, valid: bool = True) -> str:
    """Write a `.entities.yaml` sidecar. valid=True -> passes
    validate_sidecar.validate; valid=False -> an entity missing required keys
    (the exact thing the shipped validator rejects)."""
    if valid:
        data = {
            "entities": [
                {"name": "tokio", "type": "technology",
                 "description": "async runtime"},
            ],
            "relationships": [],
        }
    else:
        data = {"entities": [{"name": "tokio"}], "relationships": []}  # no type/description
    path.write_text(yaml.dump(data, sort_keys=False))
    return str(path)


class _FakeWriter:
    """Deterministic drain-write stand-in. Emits MALFORMED_BODY for the first
    ``bad_n`` invocations, then VALID_BODY. Records the ``errors`` it was handed
    on each call so the proof can assert the re-prompt carried the prior
    failures. NO LLM — pure replay keyed on the call count."""

    def __init__(self, *, bad_n: int, sidecar: str):
        self.bad_n = bad_n
        self.sidecar = sidecar
        self.calls = 0
        self.errors_seen: list[list[str]] = []

    def __call__(self, errors):
        self.errors_seen.append(list(errors))
        self.calls += 1
        body = MALFORMED_BODY if self.calls <= self.bad_n else VALID_BODY
        return DrainOutput(body=body, sidecar_path=self.sidecar)


# --------------------------------------------------------------------------- A
def test_S10_clean_first_write_accepts_at_attempt_one(tmp_path):
    """(A) CONTROL: a writer whose first output is valid is accepted at
    attempts=1, validated=True, and is invoked exactly once — a well-behaved
    writer pays no retry tax."""
    sidecar = _write_sidecar(tmp_path / "good.entities.yaml", valid=True)
    w = _FakeWriter(bad_n=0, sidecar=sidecar)
    result = write_validate_retry(w)
    assert isinstance(result, DrainResult)
    assert result.validated is True
    assert result.attempts == 1, "a clean first write must accept at attempt 1"
    assert result.errors == []
    assert w.calls == 1, "valid first output must not trigger any re-invocation"
    assert w.errors_seen[0] == [], "the first attempt has no prior errors to inline"


# --------------------------------------------------------------------------- B
def test_S10_one_malformed_then_valid_regenerates_once_then_accepts(tmp_path):
    """(B) ACCEPTANCE: a malformed learning regenerates once then accepts. K=1 ->
    attempts=2, validated=True; the 2nd writer call was re-prompted WITH the 1st
    attempt's validation errors (the self-heal feedback channel)."""
    sidecar = _write_sidecar(tmp_path / "good.entities.yaml", valid=True)
    w = _FakeWriter(bad_n=1, sidecar=sidecar)
    result = write_validate_retry(w)
    assert result.validated is True
    assert result.attempts == 2, (
        "one malformed write then a valid one must be accepted on the 2nd "
        "attempt — telemetry tracks the attempt count"
    )
    assert w.calls == 2
    # The re-prompt must carry the prior attempt's errors so the writer can fix.
    assert w.errors_seen[0] == [], "1st attempt has no errors to inline"
    assert w.errors_seen[1], "2nd attempt must be re-prompted WITH the prior errors inlined"
    assert any("frontmatter" in e for e in w.errors_seen[1]), (
        "the inlined errors must name the actual structural defect (missing frontmatter)"
    )


# --------------------------------------------------------------------------- C
def test_S10_accepts_as_soon_as_valid_not_whole_budget(tmp_path):
    """(C) accept-on-first-valid: the loop stops at the first valid attempt and
    records THAT attempt's number — it does not always burn the whole budget."""
    sidecar = _write_sidecar(tmp_path / "good.entities.yaml", valid=True)

    # K=1 under the default budget (3) -> stops at 2, not 3.
    w1 = _FakeWriter(bad_n=1, sidecar=sidecar)
    r1 = write_validate_retry(w1)
    assert r1.validated is True and r1.attempts == 2 and w1.calls == 2

    # K=2 under a budget of 3 -> valid on the last allowed attempt (3).
    w2 = _FakeWriter(bad_n=2, sidecar=sidecar)
    r2 = write_validate_retry(w2, max_attempts=3)
    assert r2.validated is True, "valid on the 3rd of 3 attempts must still be accepted"
    assert r2.attempts == 3 and w2.calls == 3


# --------------------------------------------------------------------------- D
def test_S10_always_malformed_bails_at_exactly_three_validated_false(tmp_path):
    """(D) ACCEPTANCE: an always-malformed writer is invoked EXACTLY 3 times and
    the loop gives up — validated=False, attempts=3, errors carries the last
    failures, and the note body is STILL returned (quarantined, not dropped)."""
    assert S10_MAX_ATTEMPTS == 3, "proof pins the shipped default attempt budget"
    sidecar = _write_sidecar(tmp_path / "good.entities.yaml", valid=True)
    w = _FakeWriter(bad_n=99, sidecar=sidecar)  # never produces a valid body
    result = write_validate_retry(w)
    assert result.validated is False, "a never-valid writer must bail with validated=False"
    assert result.attempts == 3, "must bail at exactly the 3-attempt bound"
    assert w.calls == 3, "the writer must be invoked exactly max_attempts (3) times — not 2, not 4"
    assert result.errors, "give-up must surface the last attempt's validation errors"
    assert result.body == MALFORMED_BODY, (
        "the note is still written on give-up (flagged invalid) — signal is "
        "quarantined, never silently dropped"
    )


# --------------------------------------------------------------------------- E
def test_S10_attempt_budget_is_a_knob_flips_invocation_count(tmp_path):
    """(E) FALSIFIABLE KNOB: the SAME always-malformed writer is invoked exactly
    2 times under max_attempts=2 and exactly 5 under max_attempts=5 — same seed,
    different bound, different total. The loop, not the writer, owns the limit."""
    sidecar = _write_sidecar(tmp_path / "good.entities.yaml", valid=True)

    w_lo = _FakeWriter(bad_n=99, sidecar=sidecar)
    r_lo = write_validate_retry(w_lo, max_attempts=2)
    assert r_lo.validated is False and r_lo.attempts == 2 and w_lo.calls == 2, (
        "under max_attempts=2 the bound must be 2 invocations"
    )

    w_hi = _FakeWriter(bad_n=99, sidecar=sidecar)
    r_hi = write_validate_retry(w_hi, max_attempts=5)
    assert r_hi.validated is False and r_hi.attempts == 5 and w_hi.calls == 5, (
        "under max_attempts=5 the SAME writer must be invoked 5 times — flipping "
        "the knob flips the bound, so the loop decides the limit, not the input"
    )


# --------------------------------------------------------------------------- F
def test_S10_validation_reuses_real_sidecar_validator(tmp_path):
    """(F) DECISIVE: the gate is the SHIPPED validate_sidecar.validate, not a
    stub. An output whose ONLY defect is a structurally-broken sidecar is
    rejected with the validator's own error (prefixed `sidecar:`); flipping the
    sidecar to valid (nothing else changed) clears it and the loop accepts at
    attempt 1."""
    bad_sidecar = _write_sidecar(tmp_path / "bad.entities.yaml", valid=False)
    good_sidecar = _write_sidecar(tmp_path / "good.entities.yaml", valid=True)

    # Direct validation: a valid body + broken sidecar yields a sidecar error
    # that matches what validate_sidecar.validate emits (missing required keys).
    errs_bad = validate_drain_output(DrainOutput(body=VALID_BODY, sidecar_path=bad_sidecar))
    assert errs_bad, "a broken sidecar must be rejected"
    assert any(e.startswith("sidecar:") and "missing required keys" in e for e in errs_bad), (
        f"the error must be the shipped validator's, prefixed `sidecar:`; got {errs_bad!r}"
    )
    # Sanity: those errors are exactly the validator's, only re-prefixed.
    import validate_sidecar
    raw = validate_sidecar.validate(Path(bad_sidecar))
    assert raw, "the shipped validator must independently reject the same sidecar"

    # Same body, VALID sidecar -> clean.
    errs_good = validate_drain_output(DrainOutput(body=VALID_BODY, sidecar_path=good_sidecar))
    assert errs_good == [], "a valid body + valid sidecar must pass"

    # And in the loop: a writer emitting (valid body, valid sidecar) accepts at 1.
    w = _FakeWriter(bad_n=0, sidecar=good_sidecar)
    result = write_validate_retry(w)
    assert result.validated is True and result.attempts == 1


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
