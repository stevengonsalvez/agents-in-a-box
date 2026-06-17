# ABOUTME: Behavioral proof for S4 — provenance proof_count is first-class on learnings.
# ABOUTME: It surfaces verbatim via `--field proof_count` AND nudges rank via a bounded log boost.
"""S4 provenance / proof_count proof.

True invariant (corrected against the real diff at 464f2fe1 —
`feat(reflect): provenance source ids and proof_count on learnings (S4)`):

`proof_count` is a first-class provenance field on a learning, read by recall.py
from frontmatter (top-level OR nested under a `provenance:` block — see
``Learning.proof_count``). It is NOT emitted as an automatic field on every
recall row; instead S4 manifests in TWO observable ways, both driven entirely by
the seed + a documented flag, with no LLM in the assertion:

  A. SURFACING — `recall --field proof_count` projects the declared value into
     the result row's ``field_value`` (the S1 ``--field`` projection path,
     ``Learning.field_value``). A seed that DECLARES proof_count surfaces it
     verbatim; a control seed that does NOT declare it surfaces null. This is
     the "first-class field, present through the real engine" half.

  B. RANKING (knob toggle, decisive) — the reranker multiplies in the real
     ``proof_count_boost`` (recall.py):
     ``proof_norm = clamp(0.5 + ln(proof_count)/10, 0, 1)`` fed through the R8
     bounded form ``1 + α·(norm − 0.5)`` with ``α = RECALL_PROOF_ALPHA`` (default
     0.1 → max +5%). proof_count 1 (or missing/garbage) sits at norm 0.5 → boost
     EXACTLY 1.0 (legacy notes never penalised); proof_count ≥ 2 lifts the
     multiplier above 1.0, monotonically, up to the +α/2 clamp. We drive the
     REAL function directly under two imports — default α and α=0 — and show the
     same inputs go from a real lift to a strict no-op. That is the decisive
     knob proof: the flag from the S4 diff, not text luck, owns the effect.

  C. RANKING (engine causation) — the boost CHANGES the live recall order. Two
     near-identical-text twins are seeded (the cross-encoder scores them a
     near-tie); the engine's residual base order ranks one first when proof
     evidence is neutral. We put a strong proof_count on the OTHER twin: with
     the boost ON it overtakes to rank 0, and with ``RECALL_PROOF_ALPHA=0`` the
     order flips back — proving proof_count, not incidental text/index order,
     drove the live ranking through the real engine.

Why no LLM: the proof_count values are literal frontmatter, the queries are
fixed, the boost is a pure function, and the toggling flag is recall.py's own
``RECALL_PROOF_ALPHA`` env. The seeds plus the flag fully determine the surfaced
``field_value``, the boost values, and the live order.

Seeding note: the behavioral_kb fixture's ``_doc_md`` does not emit a
``proof_count`` frontmatter key (S4 post-dates the fixture), so this proof seeds
through the fixture, then injects the ``proof_count:`` line into the written
document frontmatter and re-runs ``reflect reindex --force`` with the fixture's
own hermetic env — touching only KB files inside the fixture's tmp dir. No
production code and no fixture code is modified.

Isolation note (why this file is deterministic across arm order): every arm is
hermetic and shares NO mutable state with any other arm.

  * Each ``behavioral_kb`` is per-test (the fixture is built from pytest's
    ``tmp_path``), so Arm A's and Arm C's KBs, GraphRAG caches, and state dirs
    live in different tmp trees — neither reindex nor search can see the other.

  * Arm B drives the pure ``proof_count_boost`` function in a CHILD PROCESS
    (one fresh ``python`` import of recall.py per α). It does NOT import
    recall.py into the shared pytest process and does NOT mutate this process's
    ``os.environ`` / ``sys.modules``. Earlier this arm imported recall.py
    in-process and patched the live ``os.environ`` between the two live-engine
    arms; running it out-of-process removes that as a possible cross-arm leak
    entirely (and the subprocess is the more honest "import-time α capture"
    proof anyway — it is exactly how the real CLI reads the env).

  * Each live-engine arm (A and C) reseeds its OWN fresh KB and, before
    asserting any ordering, PROBES that the seeds are actually retrievable. A
    transient empty ``reflect search`` (the engine's documented "primary arm
    failed, every booster empty -> []" path) is therefore surfaced as a loud
    infra assertion, never silently mistaken for a ranking outcome.

PORT: S4
"""
from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
import textwrap
from pathlib import Path


def _inject_proof_count(kb, name: str, value: int) -> None:
    """Add a top-level ``proof_count: <value>`` line to a seeded doc's
    frontmatter (just before the closing ``---``). Mutates only a file inside
    the fixture's hermetic KB dir."""
    doc = kb.kb_dir / "documents" / f"{name}.md"
    text = doc.read_text()
    # Insert right after the `name:` line so it lands inside the frontmatter
    # block regardless of the other keys present.
    new_text, n = re.subn(
        r"(?m)^(name: .*)$",
        rf"\1\nproof_count: {value}",
        text,
        count=1,
    )
    assert n == 1, f"could not locate frontmatter `name:` line in {doc}"
    doc.write_text(new_text)


def _reindex(kb) -> None:
    """Rebuild the engine index FROM SCRATCH over the patched KB.

    Root-cause of the historical Arm C flake (proven with a standalone probe):
    the fixture's ``seed()`` already runs one ``reflect reindex --force``, which
    populates nano-graphrag's ``working_dir`` (``<kb>/nano_graphrag_cache``).
    Re-injecting ``proof_count`` and reindexing AGAINST THAT SAME, ALREADY-
    POPULATED working_dir is a second *incremental* insert — exactly the
    nano-graphrag re-insert hazard the engine warns about ("community_reports
    dropped, early return skipping KV persistence"). It intermittently leaves
    the NAIVE vector store unqueryable, so ``reflect search --mode naive``
    returns empty context (exit 0, ``{"results": []}``) → recall.py gets ``[]``.

    A single from-scratch build does NOT hit this: the probe shows one reindex
    over a freshly-written doc returns the twins every time. So we wipe the
    GraphRAG cache and reindex once, making this arm's index a deterministic
    cold build of the proof_count-bearing docs — no second incremental insert.
    """
    cache = kb.kb_dir / "nano_graphrag_cache"
    if cache.exists():
        shutil.rmtree(cache)
    r = subprocess.run(
        ["reflect", "reindex", "--force"],
        capture_output=True,
        text=True,
        env=kb.env(),
        timeout=1800,
    )
    assert r.returncode == 0, f"reflect reindex failed: {r.stderr[-800:]}"


def _rank_of(ids: list[str], name: str) -> int:
    assert name in ids, f"expected {name!r} in results, got {ids}"
    return ids.index(name)


# --------------------------------------------------------------------------
# Arm A: surfacing. One seed DECLARES proof_count; a control seed does not.
# --------------------------------------------------------------------------
_EVIDENCED = dict(
    name="s4-evidenced",
    title="Rotate JWT signing keys without invalidating live sessions",
    category="security",
    tags=["jwt", "auth", "keys"],
    confidence="medium",
    created="2026-06-01",
    archived="2026-06-05T00:00:00",
    key_insight="Publish the new JWT signing key alongside the old one before cutover so live tokens stay valid.",
    body=(
        "To rotate JWT signing keys without logging everyone out, run a "
        "key-overlap window: publish the new public key in the JWKS endpoint "
        "before you start signing with it, keep verifying against both old and "
        "new keys, then retire the old key only after every old token has "
        "expired."
    ),
)

_CONTROL = dict(
    name="s4-control",
    title="Rotate JWT signing keys without invalidating live sessions",
    category="security",
    tags=["jwt", "auth", "keys"],
    confidence="medium",
    created="2026-06-01",
    archived="2026-06-05T00:00:00",
    key_insight="Publish the new JWT signing key alongside the old one before cutover so live tokens stay valid.",
    body=(
        "To rotate JWT signing keys without logging everyone out, run a "
        "key-overlap window: publish the new public key in the JWKS endpoint "
        "before you start signing with it, keep verifying against both old and "
        "new keys, then retire the old key only after every old token has "
        "expired, which avoids a thundering herd of re-authentications."
    ),
)

_JWT_QUERY = "how do I rotate JWT signing keys without logging users out"


def test_S4_proof_count_surfaces_via_field_projection(behavioral_kb):
    """Arm A: a seed declaring proof_count surfaces it verbatim through
    `--field proof_count`; a control seed without it surfaces null."""
    kb = behavioral_kb
    kb.seed([_EVIDENCED, _CONTROL])
    _inject_proof_count(kb, _EVIDENCED["name"], 5)  # control left undeclared
    _reindex(kb)

    payload = kb.recall(
        _JWT_QUERY, limit=5, no_mmr=True, extra_args=["--field", "proof_count"]
    )
    rows = {r.get("id"): r for r in payload.get("results", [])}

    assert _EVIDENCED["name"] in rows, (
        f"evidenced seed missing from results: {list(rows)}"
    )
    surfaced = rows[_EVIDENCED["name"]].get("field_value")
    assert str(surfaced) == "5", (
        "the declared proof_count must surface verbatim in the recall row's "
        f"field_value via `--field proof_count`; got {surfaced!r}. If this is "
        "None, Learning.proof_count / field_value is not reading the frontmatter."
    )

    # The control seed declared no proof_count → field_value must be null,
    # proving the surfaced value came from the FIELD, not a constant.
    if _CONTROL["name"] in rows:
        ctrl = rows[_CONTROL["name"]].get("field_value")
        assert ctrl in (None, "", "None"), (
            "a seed that does NOT declare proof_count must surface a null "
            f"field_value, not a value; got {ctrl!r}"
        )


# --------------------------------------------------------------------------
# Arm B: ranking. Near-identical text twins differ ONLY in proof_count.
# The bounded boost breaks the CE near-tie; RECALL_PROOF_ALPHA=0 removes it.
# --------------------------------------------------------------------------
_WELL_PROVEN = dict(
    name="s4-well-proven",
    title="Debounce a search-as-you-type input to cut redundant API calls",
    category="frontend",
    tags=["debounce", "search", "perf"],
    confidence="medium",
    created="2026-06-01",
    archived="2026-06-08T00:00:00",
    key_insight="Debounce the input ~300ms so only the settled query hits the API.",
    body=(
        "For a search-as-you-type box, debounce the input by roughly 300 "
        "milliseconds so a request fires only after the user stops typing, "
        "instead of one request per keystroke, which cuts redundant API load."
    ),
)

# Same topic, near-identical body (one trailing clause differs so the two files
# are not byte-identical and don't collapse to a single chunk). proof_count 1.
_SINGLE_SHOT = dict(
    name="s4-single-shot",
    title="Debounce a search-as-you-type input to cut redundant API calls",
    category="frontend",
    tags=["debounce", "search", "perf"],
    confidence="medium",
    created="2026-06-01",
    archived="2026-06-08T00:00:00",
    key_insight="Debounce the input ~300ms so only the settled query hits the API.",
    body=(
        "For a search-as-you-type box, debounce the input by roughly 300 "
        "milliseconds so a request fires only after the user stops typing, "
        "instead of one request per keystroke, which cuts redundant API load "
        "and keeps the backend from being hammered by every caller at once."
    ),
)

_DEBOUNCE_QUERY = "how to debounce a search-as-you-type input to reduce API calls"


# conftest resolves recall.py; reuse that exact path for the module-level proof.
from eval.behavioral.conftest import RECALL_PY  # noqa: E402


def _boost_values_under_alpha(alpha: str | None) -> dict:
    """Import the REAL recall.py in a FRESH child process under a given
    ``RECALL_PROOF_ALPHA`` and return its ``PROOF_COUNT_ALPHA`` plus
    ``proof_count_boost`` over a fixed grid of inputs.

    Out-of-process on purpose: recall.py reads the α at IMPORT time, so a fresh
    interpreter is the only faithful way to capture the knob — and, decisively
    for this file's determinism, the child cannot touch the pytest process's
    ``os.environ`` or ``sys.modules``, so Arm B can never leak state into the
    live-engine arms (A, C) no matter the run order. The boost is a pure
    function of proof_count and α with no engine noise, so the values are
    fully determined by the seed grid and the flag — no LLM, no model load.
    """
    probe = textwrap.dedent(
        """
        import json, importlib.util, sys
        spec = importlib.util.spec_from_file_location("recall_probe", sys.argv[1])
        mod = importlib.util.module_from_spec(spec)
        # recall.py defines @dataclass classes (Learning, RecallResult). When the
        # dataclass machinery processes a class it does
        # ``sys.modules.get(cls.__module__).__dict__`` to resolve forward refs;
        # if the module is NOT registered under its own name in sys.modules that
        # lookup returns None and crashes at IMPORT time. Registering the module
        # before exec_module is the standard idiom for spec-loading a dataclass-
        # using module and makes this probe import deterministically. (This was
        # the real breakage: the boost probe never even ran the pure function.)
        sys.modules[spec.name] = mod
        spec.loader.exec_module(mod)
        grid = [None, 1, 2, 8, 50]
        print(json.dumps({
            "alpha": mod.PROOF_COUNT_ALPHA,
            "boosts": {str(n): mod.proof_count_boost(n) for n in grid},
        }))
        """
    )
    import os

    env = dict(os.environ)
    if alpha is None:
        env.pop("RECALL_PROOF_ALPHA", None)  # force the documented default
    else:
        env["RECALL_PROOF_ALPHA"] = alpha
    proc = subprocess.run(
        [sys.executable, "-c", probe, str(RECALL_PY)],
        capture_output=True, text=True, env=env, timeout=120,
    )
    assert proc.returncode == 0, (
        f"recall.py boost probe failed (α={alpha!r}):\n{proc.stderr[-800:]}"
    )
    return json.loads(proc.stdout)


def test_S4_proof_count_boost_is_real_and_toggles_with_the_alpha_knob():
    """Arm B (knob ON vs OFF, decisive): the REAL ``proof_count_boost`` lifts a
    well-evidenced learning above a single-shot one when ``RECALL_PROOF_ALPHA``
    is at its default, and collapses to a strict no-op when the knob is 0 —
    proving the PORT's flag, not incidental ranking, drives the effect.

    Driven entirely in child processes so this arm shares NO mutable in-process
    state with the live-engine arms (A, C) — making the file order-independent.
    """
    on = _boost_values_under_alpha(None)  # default α (0.1)
    on_alpha = on["alpha"]
    on_b = on["boosts"]
    assert on_alpha > 0.0, f"expected a positive default proof α, got {on_alpha}"
    # Boost ON: missing and single-proof are EXACTLY neutral (legacy notes never
    # penalised); >=2 proofs strictly exceed 1.0 and grow monotonically.
    assert on_b["None"] == 1.0
    assert on_b["1"] == 1.0
    b2, b8, b50 = on_b["2"], on_b["8"], on_b["50"]
    assert 1.0 < b2 < b8 < b50, (
        f"proof boost must be monotone above 1.0; got {b2}, {b8}, {b50}"
    )
    # Bounded to +α/2 = +5%: even a huge proof_count can't exceed the clamp.
    assert b50 <= 1.0 + on_alpha / 2 + 1e-9, (
        f"proof boost must stay within +α/2; got {b50}"
    )

    # Boost OFF (RECALL_PROOF_ALPHA=0): the SAME function on the SAME inputs is
    # now a strict no-op — α=0 makes bounded_boost(norm, 0) == 1.0 for every
    # proof_count. This is the decisive toggle: the knob from the S4 diff, not
    # text luck, is what produced the lift above.
    off = _boost_values_under_alpha("0")
    assert off["alpha"] == 0.0
    for n, val in off["boosts"].items():
        assert val == 1.0, (
            f"with RECALL_PROOF_ALPHA=0 the proof boost must be a strict no-op "
            f"for proof_count={n}; got {val}"
        )


def test_S4_proof_count_flips_ranking_through_the_real_engine(behavioral_kb):
    """Arm C (engine causation): a strong proof_count flips the live recall
    order. Two near-identical twins are seeded; the engine's residual base
    order ranks ``s4-well-proven`` first when proof_count is neutral. We give
    the OTHER twin (``s4-single-shot``) a strong proof_count, so with the boost
    ON it overtakes to rank 0; with ``RECALL_PROOF_ALPHA=0`` the boost vanishes
    and the residual base order returns — a genuine, proof_count-driven flip."""
    kb = behavioral_kb  # fresh per-test hermetic KB (own tmp tree; no shared state)
    kb.seed([_WELL_PROVEN, _SINGLE_SHOT])
    # Heavy evidence on the twin that the engine otherwise ranks SECOND, none on
    # the other (None == neutral). boost(40) ≈ +3.7%, comfortably inside the
    # +5% clamp but enough to flip a near-tie the base order put the other way.
    _inject_proof_count(kb, _SINGLE_SHOT["name"], 40)
    _reindex(kb)

    ids_on = kb.recall_ids(_DEBOUNCE_QUERY, no_mmr=True)
    ids_off = kb.recall_ids(
        _DEBOUNCE_QUERY, no_mmr=True, env={"RECALL_PROOF_ALPHA": "0"}
    )

    # Retrievability guard (the deterministic fix): both twins MUST come back in
    # both runs. recall.py returns an empty set when its primary `reflect search`
    # arm fails and every booster is also empty — a transient engine/index infra
    # failure, NOT a ranking outcome. We surface that loudly here so an empty
    # result can never masquerade as (or silently invalidate) the order flip the
    # arm asserts below. This guard is what makes Arm C order-independent: it
    # depends only on THIS arm's own freshly-seeded, freshly-reindexed KB.
    for label, ids in (("boost-ON", ids_on), ("boost-OFF", ids_off)):
        assert _WELL_PROVEN["name"] in ids and _SINGLE_SHOT["name"] in ids, (
            f"both twins must be retrieved from this arm's own fresh KB on the "
            f"{label} run, but the engine returned {ids}. An empty/partial set "
            "here is a transient `reflect search` infra failure for THIS KB (the "
            "documented 'primary arm failed, boosters empty -> []' path), not a "
            "proof_count ranking result — re-run; it is independent of any other "
            "arm's KB."
        )

    on_proven_first = _rank_of(ids_on, _SINGLE_SHOT["name"]) < _rank_of(
        ids_on, _WELL_PROVEN["name"]
    )
    off_proven_first = _rank_of(ids_off, _SINGLE_SHOT["name"]) < _rank_of(
        ids_off, _WELL_PROVEN["name"]
    )

    # The decisive, causal claim: the proof-count boost CHANGES the order. With
    # the boost ON the heavily-evidenced twin leads; turning the knob OFF must
    # change that ordering. If the order were identical ON and OFF, proof_count
    # would not be what determined it — so we require the two orders to differ.
    assert on_proven_first and not off_proven_first, (
        "proof_count must change the live ranking: with the boost ON the "
        f"heavily-evidenced twin (proof_count=40) must lead (got {ids_on}); with "
        f"RECALL_PROOF_ALPHA=0 the boost vanishes and the order must change so it "
        f"no longer leads (got {ids_off}). Identical orders would mean proof_count "
        "did not drive the ranking."
    )
