# ABOUTME: Behavioral proof for A2 — bitemporal graph edges (tcommit + tvalid).
# ABOUTME: Edges carry two clocks; supersession sets tvalid_end without delete; a
# ABOUTME: date-range query filters graph edges to those VALID in the window.
"""A2 bitemporal graph-edge proof.

Port A2 extends the S2 typed-causal-link schema so every relationship carries
TWO clocks (agentmemory GraphEdge shape):

  * tcommit    — transaction time: when reflect LEARNED the edge. Defaults to
                 the sidecar's ingest time (extracted_at) when omitted.
  * tvalid     — valid time: when the edge became true IN THE WORLD. Defaults
                 to tcommit when omitted.
  * tvalid_end — when the edge stopped being true. Set on SUPERSESSION (with
                 superseded_by) instead of deleting the edge, preserving history.

This separates "what was true in April?" (tvalid filter) from "what did we KNOW
in April?" (tcommit filter). The proof drives the REAL production code on both
surfaces with NO LLM and NO torch engine — every verdict is fully determined by
the fixtures plus the shipped pure functions, so it is deterministic.

Two arms, each seeding its OWN fresh state:

STORAGE/VALIDATION ARM (validate_sidecar.py — the linter the ingest pipeline
runs before `reflect add --entities`):
  A1. A relationship carrying tcommit + tvalid + tvalid_end (ISO dates)
      validates with ZERO bitemporal errors — the fields are accepted.
  A2. A MALFORMED timestamp ("not-a-date") is REJECTED, naming the field — a
      bad clock can't silently corrupt the graph filter.
  A3. tcommit DEFAULTS to ingest time: an edge that omits tcommit, run through
      the documented `--backfill-tcommit` knob (backfill_tcommit()), is stamped
      with the sidecar's extracted_at; an edge that already declares tcommit is
      left untouched (override is honoured).
  A4. SUPERSESSION sets tvalid_end + superseded_by WITHOUT deleting the edge:
      both the superseded edge and its replacement remain in the sidecar and
      still validate clean; a backwards window (tvalid_end < tvalid) is rejected.

RETRIEVAL ARM (recall.filter_edges_by_tvalid — the graph-arm temporal filter,
fed by the REAL R6 date parser extract_temporal_constraint):
  B1. A query that parses an APRIL window surfaces ONLY the edge valid in April
      and drops the edge superseded in March and the edge that begins in June.
  B2. WIDENING/removing the date filter (a date-free query => temporal is None)
      returns BOTH edges — the filter is a windowed booster, never a blocker.
  B3. The kill-switch (RECALL_BITEMPORAL_EDGES=0) makes a windowed query return
      every edge, proving the April result is the filter's doing, not the seed's.

Falsifiability: if tvalid were ignored, B1 would keep all edges and FAIL. If
supersession deleted instead of flipping tvalid_end, A4 would not find the old
edge and FAIL. If the timestamp validation were absent, A2 would pass with zero
errors and FAIL. If tcommit didn't default to ingest time, A3 would FAIL.

Coverage note: the retrieval arm proves the graph-arm edge filter directly via
its pure production function + the real R6 parser, NOT through the full
`reflect search --mode local` subprocess. That is deliberate: the live graph
mode synthesises community-report context and DROPS per-doc/edge ids (see
conftest's recall() docstring), so an edge-id assertion through the engine is
not observable. Driving filter_edges_by_tvalid with the real parser is the
decisive, deterministic surface for the temporal knob; the storage arm covers
on-disk persistence. NOT covered: end-to-end fusion of the filtered edge set
into served learnings (untestable without per-edge ids from the engine).

PORT: A2
"""
from __future__ import annotations

import sys
from datetime import datetime
from pathlib import Path

import pytest
import yaml

# Import BOTH real production modules directly (no engine, no LLM). Path
# resolution mirrors proof_S2 (validator) and proof_R5 (recall.py): parents[3]
# of the behavioral dir is the repo root where plugins/ sits alongside
# reflect-kb/; the fallback handles a reflect-kb-as-root checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_SCRIPTS_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_RECALL_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "skills" / "recall" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "skills" / "recall" / "scripts",
]
_SCRIPTS = next((p for p in _SCRIPTS_CANDIDATES if p.exists()), _SCRIPTS_CANDIDATES[0])
_RECALL = next((p for p in _RECALL_CANDIDATES if p.exists()), _RECALL_CANDIDATES[0])
for _p in (str(_SCRIPTS), str(_RECALL)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import validate_sidecar as V  # noqa: E402
from temporal_extraction import TemporalRange, extract_temporal_constraint  # noqa: E402

# recall.py reads argv at import for nothing critical, but guard anyway.
_saved_argv = sys.argv
sys.argv = ["recall.py"]
try:
    import recall as R  # noqa: E402
finally:
    sys.argv = _saved_argv


# --- shared fixtures -------------------------------------------------------

_INGEST = "2026-06-15T00:00:00"

_ENTITIES = [
    {"name": "auth service", "type": "technology",
     "description": "the authentication layer"},
    {"name": "jwt tokens", "type": "concept",
     "description": "stateless bearer tokens"},
    {"name": "server sessions", "type": "concept",
     "description": "server-side session store"},
]


def _write(tmp_path: Path, name: str, doc: dict) -> Path:
    p = tmp_path / name
    p.write_text(yaml.safe_dump(doc, default_flow_style=False, allow_unicode=True))
    return p


def _bitemporal_errors(errors: list[str]) -> list[str]:
    """Filter validator errors down to A2 bitemporal-clock violations."""
    keys = ("tcommit", "tvalid", "tvalid_end")
    return [
        e for e in errors
        if any(k in e for k in keys)
        and ("not a valid ISO" in e or "precedes" in e)
    ]


# ===========================================================================
# STORAGE / VALIDATION ARM — drive validate_sidecar.py directly (fast, no model)
# ===========================================================================

def test_A2_storage_accepts_full_bitemporal_edge(tmp_path):
    """A1: an edge carrying tcommit + tvalid + tvalid_end (all ISO) validates
    with zero bitemporal errors — the fields are accepted additively on top of
    the S2 typed-enum schema."""
    doc = {
        "document_id": "a2-accept-doc",
        "extracted_at": _INGEST,
        "entities": _ENTITIES,
        "relationships": [
            {
                "source": "auth service", "target": "jwt tokens",
                "type": "uses",
                "description": "auth issues JWTs",
                "tcommit": "2026-04-01",
                "tvalid": "2026-04-01",
                "tvalid_end": "2026-06-01",
            }
        ],
    }
    path = _write(tmp_path, "accept.entities.yaml", doc)
    errors = V.validate(path, strict=False)
    assert _bitemporal_errors(errors) == [], (
        f"a fully-stamped bitemporal edge must validate clean; got {errors}"
    )
    # And it is STILL a clean sidecar overall (no incidental schema breakage).
    assert errors == [], errors


@pytest.mark.parametrize("bad_field", ["tcommit", "tvalid", "tvalid_end"])
def test_A2_storage_rejects_malformed_timestamp(tmp_path, bad_field):
    """A2: a malformed ISO timestamp on any bitemporal field is rejected — the
    error names the offending field. A bad clock cannot silently break the
    tvalid graph filter downstream."""
    rel = {
        "source": "auth service", "target": "jwt tokens",
        "type": "uses", "description": "auth issues JWTs",
        bad_field: "not-a-date",
    }
    # tvalid_end alone needs a tvalid to avoid an unrelated ordering check path;
    # give it a legal partner so ONLY the malformed clock can trip the gate.
    if bad_field == "tvalid_end":
        rel["tvalid"] = "2026-04-01"
    doc = {
        "document_id": "a2-bad-ts-doc",
        "extracted_at": _INGEST,
        "entities": _ENTITIES,
        "relationships": [rel],
    }
    path = _write(tmp_path, "bad.entities.yaml", doc)
    errors = V.validate(path, strict=False)
    bad = _bitemporal_errors(errors)
    assert len(bad) == 1, (
        f"expected exactly one bitemporal error for malformed {bad_field!r}; "
        f"got {errors}"
    )
    assert bad_field in bad[0] and "not a valid ISO" in bad[0], bad[0]


def test_A2_storage_tcommit_defaults_to_ingest_time(tmp_path):
    """A3: tcommit defaults to ingest time. An edge omitting tcommit, run
    through the documented --backfill-tcommit knob, is stamped with the
    sidecar's extracted_at; an edge that already declares tcommit keeps its
    explicit override (the acceptance criterion: default, but accept an
    explicit override)."""
    doc = {
        "document_id": "a2-default-doc",
        "extracted_at": _INGEST,
        "entities": _ENTITIES,
        "relationships": [
            # no tcommit -> must inherit ingest time
            {"source": "auth service", "target": "jwt tokens",
             "type": "uses", "description": "edge A"},
            # explicit tcommit -> must be left untouched (override honoured)
            {"source": "auth service", "target": "server sessions",
             "type": "uses", "description": "edge B",
             "tcommit": "2026-01-15"},
        ],
    }
    path = _write(tmp_path, "default.entities.yaml", doc)

    # default_tcommit() resolves the ingest time the engine would stamp.
    assert V.default_tcommit(doc) == _INGEST

    stamped = V.backfill_tcommit(path)
    assert stamped == 1, f"exactly the one tcommit-less edge is stamped; got {stamped}"

    after = yaml.safe_load(path.read_text())
    by_desc = {r["description"]: r for r in after["relationships"]}
    assert by_desc["edge A"]["tcommit"] == _INGEST          # defaulted
    assert by_desc["edge B"]["tcommit"] == "2026-01-15"     # override preserved

    # Idempotent: a second pass stamps nothing.
    assert V.backfill_tcommit(path) == 0
    # Still validates clean after stamping.
    assert V.validate(path) == []


def test_A2_storage_supersession_flips_tvalid_end_without_delete(tmp_path):
    """A4: supersession sets tvalid_end + superseded_by on the OLD edge WITHOUT
    deleting it — both the superseded edge and its replacement persist and
    validate clean. A backwards window (tvalid_end < tvalid) is rejected."""
    doc = {
        "document_id": "a2-supersede-doc",
        "extracted_at": _INGEST,
        "entities": _ENTITIES,
        "relationships": [
            # OLD edge: auth used JWTs from April, superseded end of May.
            {"source": "auth service", "target": "jwt tokens",
             "type": "uses", "description": "auth used JWTs",
             "tcommit": "2026-04-01", "tvalid": "2026-04-01",
             "tvalid_end": "2026-05-31",
             "superseded_by": "auth-uses-sessions"},
            # NEW edge: auth uses server sessions from June, still valid.
            {"source": "auth service", "target": "server sessions",
             "type": "uses", "description": "auth uses sessions",
             "tcommit": "2026-06-01", "tvalid": "2026-06-01"},
        ],
    }
    path = _write(tmp_path, "supersede.entities.yaml", doc)
    errors = V.validate(path)
    assert errors == [], f"supersession sidecar must validate clean; got {errors}"

    # The superseded edge is NOT deleted — it survives with its death stamp.
    after = yaml.safe_load(path.read_text())
    assert len(after["relationships"]) == 2
    old = next(r for r in after["relationships"] if r["description"] == "auth used JWTs")
    assert old["tvalid_end"] == "2026-05-31"
    assert old["superseded_by"] == "auth-uses-sessions"

    # A backwards validity window is malformed and rejected.
    bad = dict(old)
    bad["tvalid_end"] = "2026-03-01"  # before tvalid 2026-04-01
    bad_doc = {**doc, "relationships": [bad]}
    bad_path = _write(tmp_path, "backwards.entities.yaml", bad_doc)
    bad_errors = V.validate(bad_path)
    assert any("precedes" in e for e in bad_errors), bad_errors


# ===========================================================================
# RETRIEVAL ARM — recall.filter_edges_by_tvalid fed by the REAL R6 date parser
# ===========================================================================

# Three edges with distinct validity windows. Only the first is valid in April.
_APRIL_EDGE = {
    "source": "auth service", "target": "jwt tokens", "type": "uses",
    "description": "JWT era", "tvalid": "2026-04-01",
}
_SUPERSEDED_EDGE = {  # died in March — invisible to an April window
    "source": "auth service", "target": "ldap", "type": "uses",
    "description": "LDAP era", "tvalid": "2026-01-01", "tvalid_end": "2026-03-15",
}
_FUTURE_EDGE = {  # born in June — not yet true in April
    "source": "auth service", "target": "server sessions", "type": "uses",
    "description": "sessions era", "tvalid": "2026-06-01",
}
_EDGES = [_APRIL_EDGE, _SUPERSEDED_EDGE, _FUTURE_EDGE]

# Explicit ISO range => the REAL R6 parser yields a fixed April window
# regardless of wall-clock, so the proof is clock-stable.
_APRIL_QUERY = "what architecture between 2026-04-01 and 2026-04-30"


def _targets(edges: list[dict]) -> set[str]:
    return {e["target"] for e in edges}


def test_A2_retrieval_window_surfaces_only_in_window_edge():
    """B1: a query parsing an APRIL window (via the real R6 parser) keeps ONLY
    the edge valid in April — the March-superseded edge and the June edge are
    filtered out by their tvalid windows."""
    temporal = extract_temporal_constraint(_APRIL_QUERY)
    assert temporal is not None, "R6 must parse the explicit ISO range"
    assert temporal.start.month == 4 and temporal.end.month == 4, temporal.to_dict()

    kept = R.filter_edges_by_tvalid(_EDGES, temporal)
    assert _targets(kept) == {"jwt tokens"}, (
        f"only the April-valid edge must survive the window; kept {_targets(kept)}"
    )


def test_A2_retrieval_date_free_query_returns_all_edges():
    """B2: a date-free query (temporal is None — the R6 parser found no phrase)
    returns EVERY edge. The temporal filter is a windowed booster, never a
    blocker: with no window there is nothing to filter against."""
    temporal = extract_temporal_constraint("how does auth work")
    assert temporal is None, "a date-free query must yield no temporal range"

    kept = R.filter_edges_by_tvalid(_EDGES, temporal)
    assert _targets(kept) == {"jwt tokens", "ldap", "server sessions"}, (
        f"a date-free query must keep all edges; kept {_targets(kept)}"
    )


def test_A2_retrieval_widening_window_returns_both():
    """B2 (cont.): widening the window to span both eras returns BOTH the JWT
    and the sessions edge — the filter respects the *queried* window, not a
    fixed cutoff."""
    wide = TemporalRange(
        start=datetime(2026, 4, 1), end=datetime(2026, 6, 30), confidence=1.0,
    )
    kept = R.filter_edges_by_tvalid(_EDGES, wide)
    # April edge (open-ended, still valid) AND June edge both overlap the wide
    # window; the March-superseded edge still does not.
    assert _targets(kept) == {"jwt tokens", "server sessions"}, _targets(kept)


def test_A2_retrieval_tcommit_defaults_tvalid():
    """An edge with only tcommit (no explicit tvalid) is windowed by tcommit —
    the documented valid<-transaction default chain. The April window keeps a
    tcommit-only April edge."""
    tcommit_only = {
        "source": "auth service", "target": "oauth", "type": "uses",
        "description": "oauth", "tcommit": "2026-04-10",
    }
    temporal = extract_temporal_constraint(_APRIL_QUERY)
    kept = R.filter_edges_by_tvalid([tcommit_only], temporal)
    assert _targets(kept) == {"oauth"}, _targets(kept)


def test_A2_retrieval_kill_switch_disables_filter(monkeypatch):
    """B3: with RECALL_BITEMPORAL_EDGES=0 the filter is a pass-through — a
    windowed query returns EVERY edge. This proves the April result in B1 is
    the filter's doing, not an artefact of the seed set. The knob is re-read at
    call time so the monkeypatched env governs this call."""
    monkeypatch.setenv("RECALL_BITEMPORAL_EDGES", "0")
    monkeypatch.setattr(R, "RECALL_BITEMPORAL_ENABLED", False, raising=False)

    temporal = extract_temporal_constraint(_APRIL_QUERY)
    kept = R.filter_edges_by_tvalid(_EDGES, temporal)
    assert _targets(kept) == {"jwt tokens", "ldap", "server sessions"}, (
        f"with the knob OFF a windowed query must keep all edges; "
        f"kept {_targets(kept)}"
    )
