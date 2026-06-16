# ABOUTME: Behavioral proof for S2 — typed causal links between learnings are gated
# ABOUTME: by a CLOSED relationship-type enum in the real sidecar validator: legal
# ABOUTME: typed edges (causes/enables/prevents/...) pass, out-of-enum edges are
# ABOUTME: rejected, and the documented --backfill knob flips a rejected edge to pass.
"""S2 typed causal-link closed-enum proof.

Port S2 is a STORAGE/FORMAT port (surface=storage). Corrected against the real
diff at 33a25843 (`feat(reflect): typed causal-link enum in sidecar validator +
drain (S2 plugin half)`), the true invariant lives in the plugin's
``validate_sidecar.py`` — NOT in recall.py, so there is no recall ranking knob to
toggle. The port replaces the old open-ended pair-of-strings relationship `type`
with a CLOSED enum:

    RELATIONSHIP_TYPES = TYPED_CAUSAL_LINK_TYPES | LEGACY_RELATIONSHIP_TYPES
      TYPED_CAUSAL_LINK_TYPES = {caused_by, causes, enables, prevents,
                                 contradicts, supersedes, part_of, uses}
      LEGACY_RELATIONSHIP_TYPES = {solves, requires, relates_to,
                                   implements, configures, triggers}

``validate()`` (the real function the ingest pipeline calls before
``reflect add --entities``) emits an error for any relationship whose ``type`` is
outside that set. We drive the REAL validator on fixtures we build, with NO LLM
and NO torch engine — the validator is a pure YAML linter.

Invariant (each arm's seed + the validator fully determine the verdict — no LLM
in the assertion):

  A. ACCEPT (typed-enum membership). For EVERY one of the 8 S2 typed causal link
     types, a sidecar declaring a single relationship of that type validates with
     ZERO type errors. This pins that the closed enum *contains* the typed causal
     vocabulary the port shipped (the "graph queries gain meaning" half).

  B. REJECT (closed gate, decisive). An OTHERWISE-IDENTICAL sidecar whose only
     change is an out-of-enum relationship `type` (a value the LLM drain could
     plausibly hallucinate — e.g. `frobnicates`, `relatedto`, `CAUSES`) is
     REJECTED: validate() returns an error that names the offending type and the
     enum. Same entities, same fields, same source/target — ONLY the type differs.
     This proves the *closedness* of the enum, not incidental schema failure.

  C. KNOB TOGGLE (--backfill flips the verdict). The SAME rejected sidecar, run
     through the port's documented ``backfill()`` (the ``--backfill`` CLI flag from
     the diff), rewrites the unknown type to the ``relates_to`` default and the
     file then validates CLEAN. backfill() reports exactly 1 rewritten edge, and a
     legal typed edge in the same file is NEVER touched (0 extra rewrites). This is
     the decisive flag proof: the enum gate (not some unrelated field) owns the
     arm-B rejection, because toggling the type back inside the enum — and nothing
     else — is exactly what clears it.

Falsifiability: if the enum were still open (pre-S2), arm B would pass with zero
errors and the assertion would FAIL. If backfill did not normalize the bad type,
arm C's post-backfill validate would still error and FAIL. If backfill clobbered
legal typed edges, arm C's "typed edge untouched" assertion would FAIL.

PORT: S2
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest
import yaml

# The validator lives in the reflect plugin; import the REAL module directly so
# we exercise the shipped closed-enum gate, not a copy. Path resolution mirrors
# proof_S7_chunk_hash_dedup.py: parents[3] is the repo root where plugins/ sits
# alongside reflect-kb/; the fallback handles a reflect-kb-as-root checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import validate_sidecar as V  # noqa: E402


# --- seeds -----------------------------------------------------------------

# Two entities so source/target are well-formed; entity types are legal so the
# ONLY thing under test is the relationship `type` enum membership.
_ENTITIES = [
    {"name": "race condition", "type": "error",
     "description": "concurrent map writes corrupt state"},
    {"name": "mutex guard", "type": "pattern",
     "description": "serialize access with a lock"},
]

# The 8 typed causal link types the S2 port shipped. Pinned as a literal here so
# the proof FAILS LOUDLY if the production set ever shrinks below these — the
# assertion below independently re-derives them from the module and cross-checks.
_S2_TYPED_CAUSAL = [
    "caused_by", "causes", "enables", "prevents",
    "contradicts", "supersedes", "part_of", "uses",
]

# Out-of-enum types an LLM drain could plausibly emit. None of these are in the
# closed enum; each must be rejected. Includes a case-variant and a no-underscore
# variant to show the gate is exact-match, not fuzzy.
_OUT_OF_ENUM = ["frobnicates", "relatedto", "CAUSES", "caused-by", "led_to"]


def _sidecar(rel_type: str) -> dict:
    """Build a minimal, otherwise-valid sidecar whose single relationship
    carries ``rel_type``. Only ``rel_type`` varies across arms."""
    return {
        "document_id": "s2-proof-doc",
        "extracted_at": "2026-06-14T00:00:00Z",
        "entities": _ENTITIES,
        "relationships": [
            {
                "source": "mutex guard",
                "target": "race condition",
                "type": rel_type,
                "description": "the guard addresses the race",
            }
        ],
    }


def _write(tmp_path: Path, name: str, doc: dict) -> Path:
    p = tmp_path / name
    p.write_text(yaml.safe_dump(doc, default_flow_style=False, allow_unicode=True))
    return p


def _type_errors(errors: list[str]) -> list[str]:
    """Filter validator errors down to relationship-type enum violations."""
    return [e for e in errors if ".type =" in e and "not in" in e]


# --- arm A: every typed causal link type is accepted -----------------------

@pytest.mark.parametrize("rel_type", _S2_TYPED_CAUSAL)
def test_typed_causal_link_accepted(tmp_path, rel_type):
    """A sidecar declaring a single S2 typed causal relationship validates with
    zero type errors — the closed enum CONTAINS the typed causal vocabulary."""
    # Guard: the production module must actually contain this type. If S2 were
    # reverted the import-time set would lose it and this cross-check would fail
    # before we even validate — keeping the literal list honest.
    assert rel_type in V.TYPED_CAUSAL_LINK_TYPES, (
        f"{rel_type!r} missing from production TYPED_CAUSAL_LINK_TYPES — "
        f"S2 enum regressed"
    )
    assert rel_type in V.RELATIONSHIP_TYPES

    path = _write(tmp_path, "good.entities.yaml", _sidecar(rel_type))
    errors = V.validate(path, strict=False)
    assert _type_errors(errors) == [], (
        f"typed causal link {rel_type!r} should pass the enum gate; "
        f"got type errors: {_type_errors(errors)}"
    )


# --- arm B: out-of-enum type is rejected (closed gate) ----------------------

@pytest.mark.parametrize("bad_type", _OUT_OF_ENUM)
def test_out_of_enum_type_rejected(tmp_path, bad_type):
    """An otherwise-identical sidecar whose ONLY change is an out-of-enum
    relationship type is rejected: validate() names the bad type and the enum.
    Proves the enum is CLOSED, not open."""
    assert bad_type not in V.RELATIONSHIP_TYPES, (
        f"test bug: {bad_type!r} unexpectedly in the enum"
    )

    path = _write(tmp_path, "bad.entities.yaml", _sidecar(bad_type))
    errors = V.validate(path, strict=False)

    type_errs = _type_errors(errors)
    assert len(type_errs) == 1, (
        f"expected exactly one type-enum error for {bad_type!r}, got: {errors}"
    )
    # The error must name the offending value (decisive: it's THIS type that
    # tripped the gate, not some unrelated schema failure).
    assert repr(bad_type) in type_errs[0], type_errs[0]
    assert "not in" in type_errs[0]


def test_accept_vs_reject_differ_only_by_type(tmp_path):
    """Decisiveness: a legal-typed and an illegal-typed sidecar are byte-for-byte
    identical except for the single relationship `type`. The legal one passes,
    the illegal one fails — so the relationship-type enum, and nothing else, owns
    the verdict difference."""
    good = _sidecar("enables")
    bad = _sidecar("frobnicates")

    # Confirm the docs differ ONLY at relationships[0].type.
    g = {**good}
    b = {**bad}
    assert g.pop("relationships")[0]["type"] == "enables"
    assert b.pop("relationships")[0]["type"] == "frobnicates"
    assert g == b  # everything else is identical

    good_path = _write(tmp_path, "good.entities.yaml", good)
    bad_path = _write(tmp_path, "bad.entities.yaml", bad)

    assert _type_errors(V.validate(good_path)) == []
    assert len(_type_errors(V.validate(bad_path))) == 1


# --- arm C: --backfill knob flips a rejected sidecar to pass -----------------

def test_backfill_knob_flips_rejection_to_pass(tmp_path):
    """The documented ``--backfill`` knob (backfill()) rewrites the out-of-enum
    type to the ``relates_to`` default; the file then validates clean. A legal
    typed edge in the same file is left untouched. This is the decisive flag
    proof: re-typing the bad edge INTO the enum — and nothing else — clears the
    arm-B rejection, so the enum gate caused it."""
    doc = {
        "document_id": "s2-backfill-doc",
        "extracted_at": "2026-06-14T00:00:00Z",
        "entities": _ENTITIES,
        "relationships": [
            # legal typed edge — must survive backfill verbatim
            {"source": "mutex guard", "target": "race condition",
             "type": "prevents", "description": "guard prevents the race"},
            # illegal edge — must be rewritten to the default
            {"source": "race condition", "target": "mutex guard",
             "type": "frobnicates", "description": "bogus edge type"},
        ],
    }
    path = _write(tmp_path, "mixed.entities.yaml", doc)

    # Pre-backfill: rejected (the illegal edge trips the gate).
    pre = V.validate(path)
    assert len(_type_errors(pre)) == 1, pre

    # Toggle the knob: backfill rewrites EXACTLY the one unknown-type edge.
    rewritten = V.backfill(path)
    assert rewritten == 1, f"expected 1 rewrite, got {rewritten}"

    # Post-backfill: validates clean — the verdict flipped.
    post = V.validate(path)
    assert _type_errors(post) == [], post

    # The legal typed edge was NOT clobbered; the illegal one became the default.
    after = yaml.safe_load(path.read_text())
    rels = {r["description"]: r["type"] for r in after["relationships"]}
    assert rels["guard prevents the race"] == "prevents"   # untouched
    assert rels["bogus edge type"] == V.BACKFILL_DEFAULT_TYPE  # normalized
    assert V.BACKFILL_DEFAULT_TYPE == "relates_to"

    # Idempotence: a second backfill is a no-op (nothing left out-of-enum).
    assert V.backfill(path) == 0


def test_backfill_leaves_all_legal_typed_sidecar_untouched(tmp_path):
    """Control: a sidecar with only legal typed edges is never rewritten by the
    knob (0 changes) — backfill normalizes ONLY out-of-enum types."""
    doc = _sidecar("causes")
    path = _write(tmp_path, "all-good.entities.yaml", doc)
    before = path.read_text()
    assert V.backfill(path) == 0
    assert path.read_text() == before  # file untouched on disk
