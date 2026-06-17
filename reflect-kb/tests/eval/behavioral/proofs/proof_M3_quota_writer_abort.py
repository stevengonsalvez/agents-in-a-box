# ABOUTME: Behavioral proof for port M3 — quota_store.should_abort gates the drain writer on subscription
# ABOUTME: quota telemetry (rejection / surpassedThreshold-without-overage / utilization ceiling), ingest+TTL persists it.
"""M3 subscription-quota-aware writer abort proof (capture port, NOT retrieval).

Port M3 lives in ``plugins/reflect/scripts/quota_store.py`` (commit 1249399d), a
stdlib-only clean-room port of claude-mem's ``RateLimitStore``. The drain hook
(``reflect-drain-bg.sh``) consults it before EACH queue entry: a CLOSED gate
defers the whole queue (``reason=quota_near_limit``) instead of burning the
daily cap into a hard quota wall; entries are never consumed, so they replay
once the gate reopens. ``recall.py`` and the GraphRAG engine never touch quota —
the signal is produced and consumed entirely at *capture* time, by the writer
drainer. So the behavioral_kb retrieval fixture is the WRONG surface: there is
nothing to rank. This proof drives the REAL module directly (no mock, no stub,
no torch — fast), and NO LLM runs in any assertion. ``should_abort`` is a pure
deterministic function over a dict snapshot; ``ingest_infos`` / ``load_state`` /
``parse_output`` / the defer marker are deterministic disk I/O. The gate reads
ONLY the in-memory / on-disk snapshot — never the network — which is itself the
port's headline guarantee (zero extra API calls to check quota).

The supplied hypothesis said "the writer aborts when a subscription quota / rate
limit is exceeded, tracked in a RateLimitStore; with quota available the writer
proceeds, with quota exhausted it aborts cleanly, the store transitions
correctly." The real diff is more precise; the invariant is corrected against the
shipped code:

  * should_abort(state, api_key_auth, limits) -> GateDecision(abort, reason,
    window). Per window, in priority order: provider rejection
    (status=='rejected', or overageStatus=='rejected' on the overage window) >
    surpassedThreshold-truthy-AND-NOT-isUsingOverage > utilization >= the
    per-window ceiling (DEFAULT_THRESHOLDS, e.g. five_hour=0.95). Unknown /
    empty state = gate OPEN.
  * API-key auth is EXEMPT (per-call billing = the user authorized the spend):
    api_key_auth=True forces abort=False even on a provider-rejected snapshot.
  * ingest_infos persists the SDK rate_limit snapshot to disk (last-write-wins
    per rateLimitType bucket). load_state expires buckets older than the TTL ON
    READ, so a stale snapshot can never wedge the gate shut forever — the gate
    fails OPEN past the TTL.
  * The deferred-write marker is informational only: queue entries are never
    consumed on defer, so a deferral does not destroy pending work.

INVARIANTS (the seeded snapshot + the auth/TTL knobs fully determine each
outcome; no LLM decides anything):

  A. QUOTA AVAILABLE -> WRITER PROCEEDS (control). A fresh "allowed" low-
     utilization snapshot leaves the gate OPEN (abort=False). This is the
     baseline the abort cases contrast against.

  B. PROVIDER REJECTION -> ABORT. A snapshot the provider already declared
     exhausted (status=='rejected') closes the gate cleanly, naming the window.

  C. surpassedThreshold WITHOUT overage -> ABORT; WITH overage -> PROCEED. The
     acceptance rule: crossing the warning threshold aborts ONLY when overage
     isn't absorbing the spill. Same surpassedThreshold seed, the isUsingOverage
     bit flips the verdict — so the abort is the rule's doing, not the input's.

  D. UTILIZATION CEILING IS A DECISIVE BOUNDARY. util just over the per-window
     ceiling aborts; util just under does NOT. Same window, the value crossing
     the ceiling is what flips the gate.

  E. API-KEY AUTH IS AN ABSENCE CONTROL. The SAME provider-rejected snapshot
     that aborts under subscription auth (B) does NOT abort under api_key_auth
     — proving the gate is the port's subscription-quota logic, not a blanket
     "rejected => stop".

  F. INGEST -> LOAD ROUND-TRIPS THROUGH THE REAL STORE. parse_output extracts
     the rate_limit_info from a realistic SDK system-event envelope;
     ingest_infos persists it; load_state reads it back and the SAME exhausted
     snapshot drives an abort — proving the store actually carries the
     telemetry end-to-end (not just an in-memory dict the test built).

  G. TTL FAILS OPEN (knob, falsifiable). The IDENTICAL persisted "rejected"
     snapshot closes the gate when read within the TTL but reopens it once the
     bucket has aged past the TTL. Same bytes on disk, the freshness knob flips
     the verdict — so a stale reading can never wedge background memory work
     shut forever.

  H. DEFER DOES NOT CONSUME WORK. Writing the defer marker records WHY the
     queue stalled but leaves the marker readable and clearable; the proof pins
     that the marker is informational (round-trips, then clears) so deferral is
     replayable, never destructive.

Falsifiability: if M3 were absent/broken, (B)/(C)/(D) would never report
abort=True (the writer would burn the quota into a wall — the exact bug M3
fixes); (E) would abort even for api-key users who authorized the spend; (G)
would let a stale snapshot wedge the gate permanently; (F) would mean the
telemetry never actually persisted. Each abort case has its own absence/boundary
control, so a passing verdict is the port's doing, not an artifact of the seed.

Surface used: capture/signal (real quota_store module), not the behavioral_kb
retrieval fixture — see above. No torch model is loaded; fast.

PORT: M3
"""
from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path

import pytest

# quota_store lives in the reflect plugin scripts dir. Resolve it the same way
# the M2 / SG5 / M6 capture-layer proofs resolve their modules so this runs from
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

import quota_store as q  # noqa: E402
from quota_store import (  # noqa: E402
    DEFAULT_THRESHOLDS,
    GateDecision,
    ingest_infos,
    load_state,
    parse_output,
    read_defer_marker,
    should_abort,
    write_defer_marker,
)

# Pin a fixed observation time so TTL math is deterministic (no wall-clock).
T0 = 1_000_000.0

# A realistic SDK system-event envelope: `claude -p` carries the live
# subscription quota state as a system/rate_limit event. The drain ingests
# exactly this off the result stream — no extra API call.
EXHAUSTED_ENVELOPE = json.dumps({
    "type": "system",
    "subtype": "rate_limit",
    "rate_limit_info": {
        "status": "rejected",
        "rateLimitType": "five_hour",
        "utilization": 0.99,
        "isUsingOverage": False,
        "surpassedThreshold": 1,
        "resetsAt": 4_500_000_000_000,
    },
})


# --------------------------------------------------------------------------- A
def test_M3_quota_available_writer_proceeds():
    """(A) CONTROL: a fresh allowed, low-utilization snapshot leaves the gate
    OPEN — the writer proceeds. This is the baseline the abort cases contrast
    against; without it an 'abort' verdict could not be attributed to exhaustion."""
    state = {"five_hour": {"status": "allowed", "utilization": 0.20,
                           "isUsingOverage": False}}
    d = should_abort(state, api_key_auth=False)
    assert isinstance(d, GateDecision)
    assert d.abort is False, "available quota must leave the writer gate OPEN"


# --------------------------------------------------------------------------- B
def test_M3_provider_rejection_aborts_cleanly():
    """(B) PORT ON: a snapshot the provider already declared exhausted
    (status=='rejected') closes the gate, naming the offending window — the
    writer aborts instead of issuing the call that would hit the hard wall."""
    state = {"five_hour": {"status": "rejected"}}
    d = should_abort(state, api_key_auth=False)
    assert d.abort is True, "provider-rejected window must abort the writer"
    assert d.window == "five_hour", "the gate must name the offending window"
    assert "rejected" in d.reason


# --------------------------------------------------------------------------- C
def test_M3_surpassed_threshold_overage_bit_flips_the_verdict():
    """(C) PORT ON + falsifiable: surpassedThreshold aborts ONLY when overage
    isn't absorbing the spill. The SAME surpassedThreshold seed yields abort=True
    without overage and abort=False with it — the isUsingOverage bit, not the
    input shape, decides. This is the acceptance rule the port is built on."""
    base = {"surpassedThreshold": 1, "utilization": 0.50, "status": "allowed"}

    no_overage = should_abort({"five_hour": {**base, "isUsingOverage": False}},
                              api_key_auth=False)
    assert no_overage.abort is True, (
        "surpassedThreshold without an overage cushion must abort before the wall")
    assert "surpassedThreshold" in no_overage.reason

    with_overage = should_abort({"five_hour": {**base, "isUsingOverage": True}},
                                api_key_auth=False)
    assert with_overage.abort is False, (
        "with overage absorbing the spill the SAME snapshot must NOT abort — the "
        "overage bit flips the verdict, so the rule (not the seed) decides")


# --------------------------------------------------------------------------- D
def test_M3_utilization_ceiling_is_a_decisive_boundary():
    """(D) PORT ON + boundary: utilization just OVER the per-window ceiling aborts;
    just UNDER does not. Same window, the value crossing the shipped ceiling is
    exactly what flips the gate — the self-imposed headroom that keeps the
    background writer from burning the last percent the interactive session needs."""
    ceiling = DEFAULT_THRESHOLDS["five_hour"]
    assert ceiling == 0.95, "proof pins the shipped five_hour utilization ceiling"

    over = should_abort({"five_hour": {"status": "allowed", "utilization": ceiling + 0.01}},
                        api_key_auth=False)
    assert over.abort is True, "utilization at/over the ceiling must abort"
    assert "utilization" in over.reason

    under = should_abort({"five_hour": {"status": "allowed", "utilization": ceiling - 0.05}},
                         api_key_auth=False)
    assert under.abort is False, (
        "utilization under the ceiling must NOT abort — the ceiling is the decisive "
        "boundary, so the same window flips the gate purely on the value")


# --------------------------------------------------------------------------- E
def test_M3_api_key_auth_is_an_absence_control():
    """(E) ABSENCE CONTROL: the SAME provider-rejected snapshot that aborts under
    subscription auth (B) does NOT abort under api-key auth — per-call billing
    means the user already authorized the spend. This proves the gate is M3's
    subscription-quota logic, not a blanket 'rejected => stop' on any input."""
    rejected = {"five_hour": {"status": "rejected"}}

    sub = should_abort(rejected, api_key_auth=False)
    assert sub.abort is True, "subscription auth aborts on the rejected snapshot"

    api = should_abort(rejected, api_key_auth=True)
    assert api.abort is False, (
        "api-key auth must be EXEMPT — the identical rejected snapshot that aborts "
        "a subscription user must let an api-key user proceed")
    assert "api_key" in api.reason


# --------------------------------------------------------------------------- F
def test_M3_ingest_load_roundtrips_real_store(tmp_path):
    """(F) PORT ON, end-to-end: parse_output extracts the rate_limit_info from a
    realistic SDK system-event envelope; ingest_infos persists it to disk;
    load_state reads it back; and the round-tripped snapshot drives the abort.
    This proves the telemetry actually flows through the real store on disk —
    not just an in-memory dict the test hand-built."""
    sd = tmp_path / "state"
    infos = parse_output(EXHAUSTED_ENVELOPE)
    assert infos and infos[0].get("rateLimitType") == "five_hour", (
        "parse_output must pull the rate_limit_info out of the SDK envelope")

    n = ingest_infos(sd, infos, now=T0)
    assert n == 1, "ingest must persist exactly one snapshot bucket"

    # Read back within the TTL and confirm the persisted snapshot drives abort.
    state = load_state(sd, ttl=3600, now=T0 + 10)
    assert "five_hour" in state, "the snapshot must survive the disk round-trip"
    d = should_abort(state, api_key_auth=False)
    assert d.abort is True and d.window == "five_hour", (
        "the ingested-from-disk exhausted snapshot must abort the writer — the "
        "store carries the telemetry end-to-end")


# --------------------------------------------------------------------------- G
def test_M3_ttl_fails_open(tmp_path):
    """(G) FALSIFIABLE KNOB: the IDENTICAL persisted 'rejected' snapshot closes the
    gate when read within the TTL but REOPENS it once the bucket has aged past the
    TTL. Same bytes on disk, the freshness knob flips the verdict — a stale
    reading can never wedge background memory work shut forever (expiry is applied
    on READ, the gate fails OPEN)."""
    sd = tmp_path / "state"
    ingest_infos(sd, parse_output(EXHAUSTED_ENVELOPE), now=T0)

    fresh = load_state(sd, ttl=3600, now=T0 + 10)  # 10s old, within TTL
    assert should_abort(fresh, api_key_auth=False).abort is True, (
        "within the TTL the persisted rejected snapshot must keep the gate CLOSED")

    expired = load_state(sd, ttl=3600, now=T0 + 5000)  # 5000s old, past TTL
    assert expired == {}, "buckets older than the TTL must be dropped on read"
    assert should_abort(expired, api_key_auth=False).abort is False, (
        "past the TTL the gate must fail OPEN — same on-disk snapshot, the "
        "freshness knob flips the verdict, so a stale reading can't wedge the gate")


# --------------------------------------------------------------------------- H
def test_M3_defer_marker_is_informational_not_destructive(tmp_path):
    """(H) PORT ON: deferral records WHY the queue stalled (round-trips through the
    marker) but is purely informational — queue entries are never consumed on
    defer in the drain hook, so the deferral is replayable, never destructive.
    The proof pins the marker contract the drain relies on."""
    sd = tmp_path / "state"
    assert read_defer_marker(sd) is None, "no marker before any deferral"

    marker = write_defer_marker(sd, reason="quota_near_limit",
                                detail="quota:five_hour rejected by provider",
                                window="five_hour", now=T0)
    back = read_defer_marker(sd)
    assert back == marker, "the defer marker must round-trip through disk"
    assert back["reason"] == "quota_near_limit"
    assert back["window"] == "five_hour"
    assert back["detail"], "the marker must record WHY the queue deferred"

    q.clear_defer_marker(sd)
    assert read_defer_marker(sd) is None, (
        "clearing the marker must leave no standing deferral — its presence "
        "always means 'currently deferred', so it is cleared when the gate reopens")


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
