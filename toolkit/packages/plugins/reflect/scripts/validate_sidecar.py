#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyyaml"]
# ///
"""
Validate an .entities.yaml sidecar against the reflect-kb sidecar schema.

Mirrors ``reflect_kb.entity_store::DocumentEntities.from_yaml`` (originally
``~/.learnings/cli/entity_store.py`` pre-migration) so that sidecars emitted
by reflect:ingest will not fail downstream when fed to
``reflect add --entities``.

Exit 0 = valid, 1 = invalid (with specific errors on stderr).

Usage:
    python3 validate_sidecar.py path/to/doc.entities.yaml
    python3 validate_sidecar.py --strict path/to/doc.entities.yaml
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: pyyaml required. Run via `uv run validate_sidecar.py ...`",
          file=sys.stderr)
    sys.exit(2)


ENTITY_TYPES = {"technology", "error", "pattern", "function", "concept", "tool"}
RELATIONSHIP_TYPES = {"caused_by", "solves", "requires", "relates_to"}

REQUIRED_ENTITY_FIELDS = {"name", "type", "description"}
REQUIRED_RELATIONSHIP_FIELDS = {"source", "target", "type", "description"}


def validate(path: Path, *, strict: bool = False) -> list[str]:
    """Return a list of error strings. Empty list = valid."""
    errors: list[str] = []

    if not path.exists():
        return [f"file not found: {path}"]

    try:
        data = yaml.safe_load(path.read_text())
    except yaml.YAMLError as e:
        return [f"invalid YAML: {e}"]

    if not isinstance(data, dict):
        return [f"top-level must be a mapping, got {type(data).__name__}"]

    # Top-level optional but recommended
    for key in ("document_id", "extracted_at"):
        if key not in data and strict:
            errors.append(f"strict: missing recommended top-level key `{key}`")

    # Entities
    entities = data.get("entities", [])
    if not isinstance(entities, list):
        errors.append("`entities` must be a list")
    else:
        for i, e in enumerate(entities):
            if not isinstance(e, dict):
                errors.append(f"entities[{i}] must be a mapping")
                continue
            missing = REQUIRED_ENTITY_FIELDS - set(e.keys())
            if missing:
                errors.append(
                    f"entities[{i}] missing required keys: "
                    f"{sorted(missing)} (got {sorted(e.keys())})"
                )
            if "type" in e and e["type"] not in ENTITY_TYPES:
                errors.append(
                    f"entities[{i}].type = {e['type']!r} not in {sorted(ENTITY_TYPES)}"
                )

    # Relationships
    rels = data.get("relationships", [])
    if not isinstance(rels, list):
        errors.append("`relationships` must be a list")
    else:
        entity_names = {
            e.get("name") for e in entities if isinstance(e, dict)
        }
        for i, r in enumerate(rels):
            if not isinstance(r, dict):
                errors.append(f"relationships[{i}] must be a mapping")
                continue
            missing = REQUIRED_RELATIONSHIP_FIELDS - set(r.keys())
            if missing:
                errors.append(
                    f"relationships[{i}] missing required keys: "
                    f"{sorted(missing)} (got {sorted(r.keys())})"
                )
            if "type" in r and r["type"] not in RELATIONSHIP_TYPES:
                errors.append(
                    f"relationships[{i}].type = {r['type']!r} "
                    f"not in {sorted(RELATIONSHIP_TYPES)}"
                )
            # Catch the common mistake of using from/to
            if "from" in r or "to" in r:
                errors.append(
                    f"relationships[{i}] uses `from`/`to` — the CLI requires "
                    f"`source`/`target`"
                )
            if strict and "strength" in r:
                s = r["strength"]
                if not isinstance(s, int) or not 1 <= s <= 10:
                    errors.append(
                        f"relationships[{i}].strength = {s!r}, must be int 1-10"
                    )
            # Referential integrity: source/target should be known entity names
            if strict:
                for end in ("source", "target"):
                    if end in r and r[end] not in entity_names:
                        errors.append(
                            f"strict: relationships[{i}].{end} = {r[end]!r} "
                            f"not in entity names"
                        )

    return errors


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("paths", nargs="+", type=Path,
                    help="one or more .entities.yaml paths")
    ap.add_argument("--strict", action="store_true",
                    help="also check recommended fields + strength bounds + "
                         "source/target referential integrity")
    args = ap.parse_args()

    total_errors = 0
    for p in args.paths:
        errs = validate(p, strict=args.strict)
        if errs:
            total_errors += len(errs)
            print(f"{p}: INVALID ({len(errs)} error(s))", file=sys.stderr)
            for e in errs:
                print(f"  - {e}", file=sys.stderr)
        else:
            print(f"{p}: OK")
    return 0 if total_errors == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
