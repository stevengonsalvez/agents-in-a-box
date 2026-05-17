#!/usr/bin/env python3
"""Validate YAML frontmatter in team-kb learning documents against the v4 schema.

Used as a pre-commit hook (see ``.pre-commit-hooks.yaml``) and the equivalent
CI workflow. Given one or more markdown paths on the CLI, parses each file's
YAML frontmatter and validates against ``schemas/frontmatter.schema.json``.

Exit codes:
    0 — every file validated cleanly
    1 — at least one file has invalid frontmatter
    2 — usage error (missing schema, unreadable path)

Only the CPython stdlib plus ``jsonschema`` and ``pyyaml`` are required, so
this script runs standalone in a pre-commit environment without installing
the full ``reflect-kb`` package.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Optional

try:
    import yaml  # type: ignore[import-untyped]
    from jsonschema import Draft202012Validator, FormatChecker
except ImportError as exc:  # pragma: no cover - deps enforced by pre-commit env
    sys.stderr.write(
        f"[validate-frontmatter] missing dependency: {exc}\n"
        "Install with: pip install 'jsonschema>=4.20' pyyaml\n"
    )
    sys.exit(2)


FRONTMATTER_RE = re.compile(r"^---\s*\n(.*?)\n---\s*\n?", re.DOTALL)

DEFAULT_SCHEMA = Path(__file__).resolve().parents[1] / "schemas" / "frontmatter.schema.json"


def extract_frontmatter(path: Path) -> Optional[dict]:
    """Pull the YAML frontmatter block from a markdown file.

    Returns ``None`` if there is no frontmatter block — callers treat this as
    a hard error because every team-kb document must carry metadata.
    """
    text = path.read_text(encoding="utf-8")
    match = FRONTMATTER_RE.match(text)
    if not match:
        return None
    loaded = yaml.safe_load(match.group(1))
    if loaded is None:
        return {}
    if not isinstance(loaded, dict):
        raise ValueError(f"frontmatter must be a mapping, got {type(loaded).__name__}")
    return loaded


def build_validator(schema_path: Path) -> Draft202012Validator:
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    Draft202012Validator.check_schema(schema)
    return Draft202012Validator(schema, format_checker=FormatChecker())


def validate_file(path: Path, validator: Draft202012Validator) -> list[str]:
    """Return a list of human-readable error strings for ``path``. Empty == valid."""
    try:
        frontmatter = extract_frontmatter(path)
    except (yaml.YAMLError, ValueError) as exc:
        return [f"{path}: failed to parse frontmatter: {exc}"]

    if frontmatter is None:
        return [f"{path}: no YAML frontmatter block found (missing `---` fences)"]

    errors: list[str] = []
    for err in sorted(validator.iter_errors(frontmatter), key=lambda e: list(e.path)):
        location = ".".join(str(p) for p in err.path) or "<root>"
        errors.append(f"{path}: {location}: {err.message}")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Validate reflect-kb learning frontmatter against the v4 JSON Schema.",
    )
    parser.add_argument(
        "paths",
        nargs="+",
        help="Markdown files (or directories to recurse) to validate.",
    )
    parser.add_argument(
        "--schema",
        type=Path,
        default=DEFAULT_SCHEMA,
        help=f"Path to frontmatter JSON Schema (default: {DEFAULT_SCHEMA}).",
    )
    args = parser.parse_args(argv)

    if not args.schema.exists():
        sys.stderr.write(f"[validate-frontmatter] schema not found: {args.schema}\n")
        return 2

    validator = build_validator(args.schema)

    targets: list[Path] = []
    for raw in args.paths:
        path = Path(raw)
        if path.is_dir():
            targets.extend(sorted(path.rglob("*.md")))
        elif path.suffix == ".md":
            targets.append(path)
        # Non-markdown paths are ignored silently — pre-commit may hand us
        # non-doc files if the filter drifts; don't FAIL for those.

    if not targets:
        return 0

    any_errors = False
    for target in targets:
        errors = validate_file(target, validator)
        if errors:
            any_errors = True
            for err in errors:
                sys.stderr.write(f"{err}\n")

    return 1 if any_errors else 0


if __name__ == "__main__":
    sys.exit(main())
