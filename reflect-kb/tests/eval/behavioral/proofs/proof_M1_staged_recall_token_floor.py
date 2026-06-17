# ABOUTME: Behavioral proof for M1 — the enforced 3-layer staged-recall pipeline (claude-mem).
# ABOUTME: Step-1 index rows are token-capped through the real engine; Step-3 hydrate of the
# ABOUTME: SAME id costs strictly more, and the Step-0 bootstrap carries the __IMPORTANT marker.
"""M1 staged-recall (3-layer workflow) proof.

True invariant (corrected against the real diff at 36e40b2d —
`feat(reflect): enforced 3-layer staged recall workflow (M1)`, module
`plugins/reflect/skills/recall/scripts/recall_stages.py`):

M1 ports claude-mem's staged retrieval discipline as a four-step contract on
top of the real recall engine: `workflow` (Step 0 bootstrap), `index` (Step 1,
ID-only rows), `timeline` (Step 2), `hydrate` (Step 3, full bodies). The
load-bearing, runtime-OBSERVABLE invariants — none of which an LLM decides —
are:

  A. STAGING COMPRESSES (decisive, with a control). Step 1 (`index`) emits, for
     each result the REAL engine returns, a compact row hard-capped at
     ``recall_stages.ROW_TOKEN_CAP`` = 100 estimated tokens (measured with
     recall.py's own ``_est_tokens`` ≈ 4-chars/token estimator — the same one
     the port's row-shaving loop uses). Step 3 (`hydrate`) of the SAME id, over
     the SAME seeded KB and SAME engine, returns the full body + frontmatter +
     sidecar and is therefore STRICTLY MORE expensive than its index row. The
     index row is the COMPRESSED form; the hydrate row is the UNCOMPRESSED
     CONTROL — same data, same engine, the only difference is the stage. That
     gap is the port's whole reason for existing ("~10x token savings vs
     hydrating everything up front"), and it is a pure byte measurement, not a
     model judgement.

       Why this is the M1 cause and not text luck: the cap is enforced by the
       module's ``_index_row`` (truncation + a shave-until-it-fits loop), and
       the hydrate>>index gap is enforced by ``reflect_hydrate`` shipping the
       whole document body. We seed a learning with a body large enough that its
       UNcapped representation would blow past 100 tokens, so an index row that
       still fits 100 can ONLY be the staging compression — if the port were a
       no-op pass-through, the index row would carry the full body and exceed the
       cap, and the gap to hydrate would collapse.

  B. BOOTSTRAP MARKER PRESENT (presence, with an implicit control). Step 0
     (`workflow`) emits the literal ``3-LAYER WORKFLOW (ALWAYS FOLLOW)``
     contract — claude-mem's ``__IMPORTANT`` bootstrap surface — both as the
     printed contract body and as the CLI tool description any client renders.
     This is the "instruct the agent to actually run staged recall before
     answering" half of the hypothesis. It is a static, deterministic emission
     of the real module (no engine, no model), so it is asserted directly: the
     marker text is PRESENT in the bootstrap output. The control is the `index`
     stage's payload, which carries NO such workflow-contract marker — proving
     the marker is the bootstrap tool's distinct product, not boilerplate every
     subcommand prints.

What is and is NOT runtime-observable (documented honestly): M1 does NOT make
the engine refuse to answer until staged recall has run — there is no runtime
gate that blocks a single-shot recall. The "enforcement" is (1) the structural
token discipline of arm A (the engine genuinely returns cheaper rows at Step 1
than Step 3) and (2) the bootstrap-contract emission of arm B (the literal
ALWAYS-FOLLOW marker a client surfaces). Both are real module outputs measured
byte-for-byte; the softer "the LLM then obeys the contract" half is a
prompt-level effect outside any deterministic assertion, so it is deliberately
NOT asserted here.

Why no LLM: the module is driven as its real CLI subprocess against the
fixture's hermetic seeded KB (the same ``GLOBAL_LEARNINGS_PATH`` the staged
module resolves ``docs_root()`` from). The seed body, the literal token cap,
recall.py's deterministic ``_est_tokens`` estimator, and the static contract
string fully determine every assertion. No assertion reads anything a model
produced.

Isolation: each live arm seeds its OWN fresh ``behavioral_kb`` (per-test
``tmp_path``), so no GraphRAG/embedding state is shared across arms. The
bootstrap arm needs no KB at all (pure static emission) and shares nothing.

PORT: M1
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

# Resolve the M1 module (recall_stages.py) and recall.py the same way conftest
# resolves recall.py, so this runs from either repo layout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_RECALL_SCRIPTS_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "skills" / "recall" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "skills" / "recall" / "scripts",
]
_RECALL_SCRIPTS = next(
    (p for p in _RECALL_SCRIPTS_CANDIDATES if (p / "recall_stages.py").exists()),
    _RECALL_SCRIPTS_CANDIDATES[0],
)
RECALL_STAGES_PY = _RECALL_SCRIPTS / "recall_stages.py"
RECALL_PY = _RECALL_SCRIPTS / "recall.py"
assert RECALL_STAGES_PY.exists(), f"recall_stages.py not found at {RECALL_STAGES_PY}"

# The real module's literal cap and the real bootstrap marker. Imported as the
# oracle (not hand-copied magic numbers) so the proof tracks the port if it
# moves. Loaded out-of-process to avoid importing recall.py's dataclass module
# into the pytest process (the documented spec-loading hazard).
_ROW_TOKEN_CAP = 100        # recall_stages.ROW_TOKEN_CAP — asserted against the source below
_BOOTSTRAP_MARKER = "3-LAYER WORKFLOW (ALWAYS FOLLOW)"


def _est_tokens(text: str) -> int:
    """recall.py's own estimator: ceil-ish 4-chars/token, floor 1. This is the
    EXACT function the port's _index_row shave loop uses, so measuring rows with
    it is measuring the cap the module enforces — not an independent heuristic."""
    return max(1, len(text) // 4)


def _run_stage(kb, args: list[str]) -> subprocess.CompletedProcess:
    """Drive recall_stages.py as its real CLI under the fixture's hermetic env.

    recall_stages.py resolves docs_root() from $GLOBAL_LEARNINGS_PATH/documents,
    which is exactly where behavioral_kb.seed() writes the seed docs + sidecars,
    so the staged module reads the same hermetic KB the engine indexed.
    """
    return subprocess.run(
        [sys.executable, str(RECALL_STAGES_PY), *args],
        capture_output=True,
        text=True,
        env=kb.env(),
        timeout=300,
    )


# A learning whose body is comfortably larger than the 100-token index-row cap.
# (~1100 chars of body => ~275 est-tokens for the body alone, before frontmatter
# and sidecar.) Its UNcapped representation cannot fit 100 tokens, so an index
# row that still fits 100 proves the staging compression did the work.
_BIG_DOC = dict(
    name="m1-staged-bigdoc",
    title="Stage a blue-green database migration behind a dual-write window",
    category="database",
    tags=["migration", "blue-green", "dual-write"],
    confidence="medium",
    created="2026-06-01",
    archived="2026-06-08T00:00:00",
    key_insight=(
        "Dual-write to the old and new schema during the cutover window so a "
        "rollback never loses writes that landed after the switch."
    ),
    body=(
        "To stage a blue-green database migration safely, run a dual-write "
        "window where the application writes every mutation to BOTH the old and "
        "the new schema simultaneously while reads continue to come from the old "
        "one. Backfill the new schema from a consistent snapshot, then reconcile "
        "the backfill against the live dual-write stream so no row written during "
        "the copy is missed. Once the new schema has caught up and a verification "
        "job confirms row-level parity between the two stores, flip reads to the "
        "new schema behind a feature flag, keeping the dual-write active so that "
        "a regression can instantly roll reads back to the old schema without "
        "data loss. Only after a full bake period — long enough that every "
        "in-flight transaction has drained and monitoring shows no parity drift — "
        "do you retire the old schema and tear down the dual-write path. The "
        "whole point of the window is that there is never a moment where a "
        "rollback would lose writes, because both stores stay authoritative "
        "until the very last step."
    ),
    entities=[
        ("dual-write", "pattern", "Write to old and new schema at once during cutover"),
        ("blue-green", "pattern", "Two parallel environments swapped behind a flag"),
    ],
)

_QUERY = "how to stage a blue-green database migration with a dual-write window"


def test_M1_index_rows_are_token_capped_and_hydrate_costs_strictly_more(behavioral_kb):
    """Arm A (decisive, control = hydrate): Step-1 index rows are hard-capped at
    ROW_TOKEN_CAP estimated tokens through the real engine; Step-3 hydrate of the
    SAME id over the SAME KB is strictly more expensive — the staging compression
    is real and measurable, with the uncompressed hydrate row as the control."""
    kb = behavioral_kb
    kb.seed([_BIG_DOC])

    # First, anchor the cap to the module's own source constant (no hand-magic).
    src = RECALL_STAGES_PY.read_text()
    assert f"ROW_TOKEN_CAP = {_ROW_TOKEN_CAP}" in src, (
        "the proof's token cap must match the module's ROW_TOKEN_CAP; the source "
        f"no longer declares ROW_TOKEN_CAP = {_ROW_TOKEN_CAP}"
    )

    # --- Step 1: index --------------------------------------------------------
    idx = _run_stage(kb, ["index", _QUERY, "--limit", "10"])
    assert idx.returncode == 0, f"index stage failed:\n{idx.stderr[-1200:]}"
    index_payload = json.loads(idx.stdout)
    assert index_payload.get("step") == 1, f"unexpected index payload: {index_payload}"
    rows = index_payload.get("results", [])
    assert rows, (
        "Step-1 index returned no rows for a seed that directly answers the "
        f"query — the staged module read an empty KB. Payload: {index_payload}"
    )

    # The seed must be among the indexed rows (it directly answers the query).
    ids = [r.get("id") for r in rows]
    assert _BIG_DOC["name"] in ids, (
        f"the seeded big-doc must appear in the Step-1 index; got ids {ids}"
    )

    # INVARIANT A1: EVERY index row is within the 100-token cap. Each row is the
    # compact {id,title,score,project,date} shape the port emits; the body is
    # NOT in it. Measured with recall.py's exact estimator.
    for row in rows:
        cost = _est_tokens(json.dumps(row))
        assert cost <= _ROW_TOKEN_CAP, (
            f"Step-1 index row exceeded ROW_TOKEN_CAP ({cost} > {_ROW_TOKEN_CAP}): "
            f"{row}. The port's _index_row truncation/shave guarantees this; a row "
            "over the cap means the staging compression is not being applied."
        )
        # Sanity: the row is genuinely the compact shape (no body smuggled in).
        assert "body" not in row, f"index row must not carry the body: {row}"

    # --- Step 3: hydrate (the UNCOMPRESSED control) ---------------------------
    hyd = _run_stage(kb, ["hydrate", _BIG_DOC["name"]])
    assert hyd.returncode == 0, f"hydrate stage failed:\n{hyd.stderr[-1200:]}"
    hydrate_payload = json.loads(hyd.stdout)
    assert hydrate_payload.get("step") == 3, f"unexpected hydrate payload: {hydrate_payload}"
    h_rows = hydrate_payload.get("results", [])
    assert h_rows and h_rows[0].get("found") is True, (
        f"Step-3 hydrate must return the full doc for the seeded id: {hydrate_payload}"
    )
    h_row = h_rows[0]
    # The hydrate row genuinely carries the full body + frontmatter (Step 3's job).
    assert h_row.get("body"), "hydrate row must carry the full learning body"
    assert _BIG_DOC["body"][:40] in h_row["body"], (
        "hydrate must return the SAME doc's real body, not a stub"
    )

    # INVARIANT A2 (decisive gap): the SAME id costs strictly MORE at Step 3 than
    # at Step 1. This is the port's raison d'être. The hydrate row is the control:
    # same engine, same KB, same id — only the stage differs.
    index_row = next(r for r in rows if r.get("id") == _BIG_DOC["name"])
    index_cost = _est_tokens(json.dumps(index_row))
    hydrate_cost = _est_tokens(json.dumps(h_row, default=str))
    assert hydrate_cost > index_cost, (
        "Step-3 hydrate of an id must cost strictly more than its Step-1 index "
        f"row (hydrate {hydrate_cost} tok vs index {index_cost} tok). If they were "
        "equal, the index stage would be hydrating everything up front and the "
        "M1 staging would be a no-op pass-through."
    )
    # And the uncompressed body alone already blows the index cap — proving the
    # 100-token index row could ONLY have been produced by the staging
    # compression, not by the doc being small.
    assert _est_tokens(_BIG_DOC["body"]) > _ROW_TOKEN_CAP, (
        "test seed is too small to be decisive — its body must exceed the index "
        "row cap so a fitting index row demonstrably required compression"
    )


def test_M1_bootstrap_emits_the_always_follow_marker_and_index_does_not(behavioral_kb):
    """Arm B (presence, control = index payload): Step-0 `workflow` emits the
    literal __IMPORTANT-style ``3-LAYER WORKFLOW (ALWAYS FOLLOW)`` contract — both
    in its printed body and its CLI tool description — while the Step-1 index
    payload carries NO such marker, proving the contract is the bootstrap tool's
    distinct product, not boilerplate every subcommand prints."""
    kb = behavioral_kb  # fresh KB; the workflow arm needs no seeds but stays isolated

    # Step 0: the bootstrap contract body.
    wf = _run_stage(kb, ["workflow"])
    assert wf.returncode == 0, f"workflow stage failed:\n{wf.stderr[-1200:]}"
    contract = wf.stdout
    assert "3-Layer Pattern (ALWAYS follow this)" in contract, (
        "the Step-0 bootstrap must emit the staged-recall contract body "
        f"(claude-mem __IMPORTANT surface); got:\n{contract[:600]}"
    )
    # The contract must instruct staged order before hydration (the enforcement
    # text). These are literal substrings of the module's WORKFLOW_CONTRACT.
    for needle in ("Index", "Timeline", "Hydrate", "Never hydrate full details without filtering first"):
        assert needle in contract, f"bootstrap contract missing {needle!r}"

    # The CLI tool description (what any client — --help, MCP listing — renders)
    # carries the literal ALWAYS-FOLLOW marker.
    help_out = subprocess.run(
        [sys.executable, str(RECALL_STAGES_PY), "--help"],
        capture_output=True, text=True, env=kb.env(), timeout=60,
    )
    assert help_out.returncode == 0, f"--help failed:\n{help_out.stderr[-600:]}"
    # argparse may wrap the description across lines; collapse whitespace before
    # matching the marker substring.
    flat_help = " ".join(help_out.stdout.split())
    assert _BOOTSTRAP_MARKER in flat_help, (
        f"the CLI tool description must surface the literal {_BOOTSTRAP_MARKER!r} "
        f"bootstrap marker; got:\n{help_out.stdout[:800]}"
    )

    # Control: the Step-1 index payload (the working data stage) does NOT carry
    # the workflow-contract marker — it is the bootstrap tool's distinct product.
    kb.seed([_BIG_DOC])
    idx = _run_stage(kb, ["index", _QUERY, "--limit", "5"])
    assert idx.returncode == 0, f"index stage failed:\n{idx.stderr[-1200:]}"
    assert _BOOTSTRAP_MARKER not in idx.stdout and "ALWAYS follow" not in idx.stdout, (
        "the Step-1 index payload must NOT carry the bootstrap ALWAYS-FOLLOW "
        f"marker — that marker is the Step-0 contract's distinct product; got:\n"
        f"{idx.stdout[:600]}"
    )
