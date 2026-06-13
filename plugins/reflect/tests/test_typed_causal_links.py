# ABOUTME: Regression tests for port S2 — typed causal links between learnings.
# ABOUTME: Pins the Hindsight memory_links shape: closed relationship-type enum
# ABOUTME: (caused_by/causes/enables/prevents/contradicts/supersedes/part_of/uses
# ABOUTME: + legacy types), validator acceptance, drain prompt, backfill.
"""Port S2: typed causal links between learnings.

Acceptance bullets pinned here:
  1. validator accepts new enum (each typed causal link type validates;
     legacy types stay valid; unknown types are rejected; the plugin enum
     stays in sync with the engine's and with the format reference)
  2. drain emits typed edges (the reflect SKILL.md — the drain's prompt —
     lists the typed enum in the sidecar schema and instructs typed causal
     edges over flat relates_to)
  3. backfill: sidecars with missing/unknown relationship types are
     rewritten to `relates_to` via `validate_sidecar.py --backfill`

(Acceptance bullet "graph-expansion arm (R1) can filter by type" is pinned
engine-side in reflect-kb/tests/test_typed_links_engine.py.)
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest
import yaml

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS_DIR = PLUGIN_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))

import validate_sidecar  # noqa: E402

TYPED_CAUSAL = sorted(validate_sidecar.TYPED_CAUSAL_LINK_TYPES)
LEGACY = sorted(validate_sidecar.LEGACY_RELATIONSHIP_TYPES)

BEAD_ENUM = {
    "caused_by", "causes", "enables", "prevents",
    "contradicts", "supersedes", "part_of", "uses",
}


def _sidecar(rel_type: str | None = "relates_to", *, drop_type: bool = False) -> dict:
    rel = {
        "source": "block_on",
        "target": "nested runtime panic",
        "description": "calling block_on inside async context",
        "strength": 9,
    }
    if not drop_type:
        rel["type"] = rel_type
    return {
        "document_id": "lrn-test-abc123",
        "extracted_at": "2026-06-10T00:00:00",
        "entities": [
            {"name": "block_on", "type": "function", "description": "blocks"},
            {"name": "nested runtime panic", "type": "error", "description": "panic"},
        ],
        "relationships": [rel],
    }


def _write(tmp_path: Path, data: dict) -> Path:
    p = tmp_path / "doc.entities.yaml"
    p.write_text(yaml.dump(data))
    return p


# ---------------------------------------------------------------------------
# Acceptance 1: validator accepts the new enum
# ---------------------------------------------------------------------------

def test_bead_enum_is_in_the_closed_enum():
    """All 8 typed causal link types from the S2 bead are valid."""
    assert BEAD_ENUM <= validate_sidecar.RELATIONSHIP_TYPES
    assert BEAD_ENUM == validate_sidecar.TYPED_CAUSAL_LINK_TYPES


@pytest.mark.parametrize("rel_type", sorted(BEAD_ENUM))
def test_validator_accepts_each_typed_causal_type(tmp_path, rel_type):
    p = _write(tmp_path, _sidecar(rel_type))
    assert validate_sidecar.validate(p) == []


@pytest.mark.parametrize("rel_type", LEGACY)
def test_validator_still_accepts_legacy_types(tmp_path, rel_type):
    """Pre-S2 sidecars must never start failing validation."""
    p = _write(tmp_path, _sidecar(rel_type))
    assert validate_sidecar.validate(p) == []


def test_validator_rejects_unknown_type(tmp_path):
    """The enum is CLOSED — arbitrary strings are invalid."""
    p = _write(tmp_path, _sidecar("vibes_with"))
    errs = validate_sidecar.validate(p)
    assert len(errs) == 1
    assert "vibes_with" in errs[0]


def test_plugin_enum_matches_engine_enum():
    """validate_sidecar.py and the engine's entity_store must agree —
    a sidecar that validates here must index there."""
    from reflect_kb.cli import entity_store

    assert validate_sidecar.RELATIONSHIP_TYPES == entity_store.RELATIONSHIP_TYPES
    assert (validate_sidecar.TYPED_CAUSAL_LINK_TYPES
            == entity_store.TYPED_CAUSAL_LINK_TYPES)


def test_schema_yaml_enum_matches_validator():
    """references/schema.yaml's sidecar relationship enum is the machine-
    readable schema of record — it must carry exactly the closed enum."""
    schema = yaml.safe_load(
        (PLUGIN_ROOT / "references" / "schema.yaml").read_text()
    )
    rel_props = (schema["sidecar_schema"]["properties"]["relationships"]
                 ["items"]["properties"])
    assert set(rel_props["type"]["enum"]) == validate_sidecar.RELATIONSHIP_TYPES


def test_knowledge_format_documents_every_enum_member():
    """references/knowledge_format.md is the schema of record — every valid
    type must be documented there (and the schema line lists the enum)."""
    text = (PLUGIN_ROOT / "references" / "knowledge_format.md").read_text()
    for rel_type in validate_sidecar.RELATIONSHIP_TYPES:
        assert f"`{rel_type}`" in text, f"{rel_type} missing from knowledge_format.md"
    # The sidecar schema block lists the typed enum inline.
    assert "caused_by | causes | enables | prevents" in text


# ---------------------------------------------------------------------------
# Acceptance 2: drain emits typed edges (the SKILL.md drives the drain agent)
# ---------------------------------------------------------------------------

def test_drain_prompt_asks_for_typed_edges():
    text = (PLUGIN_ROOT / "skills" / "reflect" / "SKILL.md").read_text()
    # Sidecar schema in the drain prompt carries the full typed enum...
    assert ("type: caused_by | causes | enables | prevents | contradicts | "
            "supersedes | part_of | uses") in text
    # ...and the rules instruct typed causal edges over flat relates_to.
    assert "Emit typed causal edges" in text
    # causal_relations frontmatter mirrors the typed causal subset.
    assert "type: caused_by | causes | enables | prevents" in text


# ---------------------------------------------------------------------------
# Acceptance 3 (bead "HOW TO PORT"): backfill existing sidecars
# ---------------------------------------------------------------------------

def test_backfill_rewrites_unknown_and_missing_types(tmp_path):
    data = _sidecar("caused_by")
    data["relationships"].append({
        "source": "block_on", "target": "tokio",
        "type": "totally_invalid", "description": "x", "strength": 3,
    })
    data["relationships"].append({
        "source": "tokio", "target": "async context",
        "description": "no type at all", "strength": 2,
    })
    p = _write(tmp_path, data)
    assert validate_sidecar.validate(p) != []  # invalid before backfill

    changed = validate_sidecar.backfill(p)
    assert changed == 2

    after = yaml.safe_load(p.read_text())
    types = [r["type"] for r in after["relationships"]]
    assert types == ["caused_by", "relates_to", "relates_to"]
    assert validate_sidecar.validate(p) == []  # valid after backfill


def test_backfill_is_a_noop_on_fully_typed_sidecars(tmp_path):
    p = _write(tmp_path, _sidecar("enables"))
    before = p.read_text()
    assert validate_sidecar.backfill(p) == 0
    assert p.read_text() == before  # untouched — no spurious rewrites


def test_backfill_cli_flag(tmp_path):
    p = _write(tmp_path, _sidecar("legacy_unknown"))
    script = SCRIPTS_DIR / "validate_sidecar.py"
    proc = subprocess.run(
        [sys.executable, str(script), "--backfill", str(p)],
        capture_output=True, text=True,
    )
    assert proc.returncode == 0, proc.stderr
    assert "backfilled 1 relationship type(s)" in proc.stdout
    after = yaml.safe_load(p.read_text())
    assert after["relationships"][0]["type"] == "relates_to"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
