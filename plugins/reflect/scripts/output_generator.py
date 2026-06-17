#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Output Generator for Reflect Skill

Generates reflection output files and manages indexes:
- Project knowledge notes: docs/solutions/{category}/{name}.md
- Entity sidecars: docs/solutions/{category}/{name}.entities.yaml
- Episode notes: ~/.reflect/episodes/ep-{date}-{hash}.md
- Project skills: .claude/skills/{name}/SKILL.md

All output now includes provenance metadata for traceability.

Note: v1/v2 .claude/reflections/ paths are deprecated. All knowledge output
now routes through docs/solutions/ (project-scoped) and the learnings CLI
(global GraphRAG indexing).

Usage:
    python output_generator.py --reflection-data '{"signals": [...], "changes": [...]}'
    python output_generator.py --create-skill skill-name --content '...'
"""

import argparse
import hashlib
import json
import os
import sys
from datetime import datetime
from pathlib import Path
from typing import Optional

# Ensure sibling imports work when run standalone
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from reflect_config import get_config

try:
    import yaml
except ImportError:
    yaml = None


# ---------------------------------------------------------------------------
# Provenance
# ---------------------------------------------------------------------------


def _build_provenance(
    source_tool: str = "",
    source_path: str = "",
    session_id: str = "",
    content_hash: str = "",
) -> dict:
    """Build a provenance metadata block for output traceability."""
    return {
        "source_tool": source_tool or "reflect",
        "source_path": source_path,
        "session_id": session_id or os.environ.get("REFLECT_SESSION_ID", ""),
        "content_hash": content_hash,
        "detected_at": datetime.now().isoformat(),
    }


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------


def get_project_dir() -> Path:
    """Get the project root directory."""
    if os.environ.get('CLAUDE_PROJECT_DIR'):
        return Path(os.environ['CLAUDE_PROJECT_DIR'])

    cwd = Path.cwd()
    for parent in [cwd] + list(cwd.parents):
        if (parent / '.git').exists():
            return parent

    return cwd


def get_project_name() -> str:
    """Get the project name from directory."""
    return get_project_dir().name


def get_solutions_dir() -> Path:
    """Get the project's docs/solutions directory."""
    cfg = get_config()
    artifacts = cfg.get("storage", {}).get("artifacts_dir", "docs/solutions")
    return get_project_dir() / artifacts


def get_project_skills_dir() -> Path:
    """Get the project skills directory."""
    return get_project_dir() / '.claude' / 'skills'


def get_episodes_dir() -> Path:
    """Get the episodes directory."""
    return Path.home() / '.reflect' / 'episodes'


def ensure_directories(*dirs: Path):
    """Ensure all required directories exist."""
    for d in dirs:
        d.mkdir(parents=True, exist_ok=True)


# ---------------------------------------------------------------------------
# History sidecars (S6: non-destructive note updates)
# ---------------------------------------------------------------------------


def get_history_sidecar_path(note_path: Path) -> Path:
    """`docs/solutions/{cat}/{slug}.md` -> `{slug}.history.yaml` sibling."""
    return note_path.with_name(f"{note_path.stem}.history.yaml")


def append_history_sidecar(
    note_path: Path,
    previous_content: str,
    reason: str = "update",
) -> Optional[Path]:
    """S6: archive the previous form of a knowledge note before overwrite.

    Appends one YAML list item to ``{slug}.history.yaml`` next to the note.
    The sidecar is append-only and hand-emitted (deterministic literal block
    scalars, no yaml-lib reflow), so a git diff of an UPDATE shows exactly
    one added entry — the audit trail is reviewable in the PR itself.

    Returns the sidecar path, or None on failure (silent-fail shaped:
    a broken snapshot must never block the note write itself).
    """
    try:
        sidecar = get_history_sidecar_path(note_path)
        content_hash = hashlib.sha256(
            previous_content.encode("utf-8")
        ).hexdigest()[:16]
        # Literal block scalar: every content line indented 4 spaces;
        # whitespace-only lines emitted empty to keep the block unambiguous.
        indented = "\n".join(
            f"    {line}" if line.strip() else ""
            for line in previous_content.splitlines()
        )
        entry = (
            f'- snapshot_at: "{datetime.now().isoformat()}"\n'
            f'  reason: "{reason}"\n'
            f'  content_hash: "{content_hash}"\n'
            f"  previous: |\n"
            f"{indented}\n"
        )
        with open(sidecar, "a", encoding="utf-8") as fh:
            fh.write(entry)
        return sidecar
    except Exception:
        return None


# ---------------------------------------------------------------------------
# Knowledge notes
# ---------------------------------------------------------------------------


# S3: display tier → numeric confidence midpoint (the reflect.db backfill
# mapping). The float is what recall ranks by; tiers are display buckets.
_CONFIDENCE_TIER_NUMS = {"HIGH": 0.9, "MEDIUM": 0.6, "MED": 0.6, "LOW": 0.3}
_DEFAULT_CONFIDENCE_NUM = 0.6


def _confidence_num(confidence: str, confidence_num: Optional[float]) -> float:
    """S3: resolve the numeric confidence for a new note.

    An explicit caller value wins (clamped to [0, 1]); otherwise the tier
    is mapped to its bucket midpoint so every new note carries BOTH fields.
    """
    if confidence_num is not None and not isinstance(confidence_num, bool):
        try:
            num = float(confidence_num)
            if num == num:  # NaN guard
                return min(1.0, max(0.0, num))
        except (TypeError, ValueError):
            pass
    return _CONFIDENCE_TIER_NUMS.get(
        str(confidence or "").strip().upper(), _DEFAULT_CONFIDENCE_NUM
    )


def _one_liner(text: str, cap: int = 200) -> str:
    """S1: distil prose to a single-line summary for structured frontmatter.

    First non-empty line, then first sentence of it, capped. The full prose
    stays in the body — this is the context-cheap projection recall returns
    when asked for `--field problem`.
    """
    for line in (text or "").splitlines():
        line = line.strip()
        if not line:
            continue
        # First sentence boundary (". " keeps versions like "v1.2" intact).
        idx = line.find(". ")
        if idx != -1:
            line = line[: idx + 1]
        return line[:cap].strip()
    return ""


def create_knowledge_note(
    title: str,
    category: str,
    tags: list[str],
    symptoms: list[str],
    root_cause: str,
    key_insight: str,
    problem: str,
    solution: str,
    context: str = "",
    confidence: str = "high",
    confidence_num: Optional[float] = None,
    language: str = "",
    framework: str = "",
    source_tool: str = "",
    source_path: str = "",
    session_id: str = "",
    content_hash: str = "",
    fix: str = "",
    rule: str = "",
    entities: Optional[list] = None,
    causal_relations: Optional[list] = None,
) -> tuple[Path, str]:
    """
    Create a knowledge note in docs/solutions/{category}/.

    S1 (Hindsight fact_extraction shape): typed fields — ``problem`` (one-liner
    derived from the prose), ``root_cause``, ``fix``, ``rule``, ``category``,
    ``entities``, ``causal_relations`` — land in structured frontmatter so
    recall can return just one field instead of the whole note. The prose body
    remains the human-readable rationale. ``problem`` is auto-derived from the
    prose; the other new fields are optional and omitted from frontmatter when
    empty, so notes from legacy callers gain only the derived ``problem`` line.

    Returns:
        Tuple of (file_path, filename_stem) for sidecar generation
    """
    category_dir = get_solutions_dir() / category
    ensure_directories(category_dir)

    # Generate filename from title
    slug = title.lower()
    slug = slug.replace(' ', '-').replace('/', '-').replace('.', '-')
    slug = ''.join(c for c in slug if c.isalnum() or c == '-')
    slug = slug[:60].rstrip('-')

    filepath = category_dir / f"{slug}.md"

    # Build frontmatter
    frontmatter: dict = {
        'title': title,
        'category': category,
        'tags': tags,
        'symptoms': symptoms,
        'root_cause': root_cause,
        'key_insight': key_insight,
        'created': datetime.now().strftime('%Y-%m-%d'),
        'confidence': confidence,
        # S3: continuous 0–1 confidence beside the display tier — recall
        # ranks by this float; omitted callers get the tier midpoint.
        'confidence_num': _confidence_num(confidence, confidence_num),
    }
    # S1: structured extraction fields — only written when populated so legacy
    # callers keep producing the exact same frontmatter as before.
    problem_line = _one_liner(problem)
    if problem_line:
        frontmatter['problem'] = problem_line
    if fix:
        frontmatter['fix'] = fix
    if rule:
        frontmatter['rule'] = rule
    if entities:
        frontmatter['entities'] = entities
    if causal_relations:
        frontmatter['causal_relations'] = causal_relations
    if language:
        frontmatter['language'] = language
    if framework:
        frontmatter['framework'] = framework

    # Provenance block
    frontmatter['provenance'] = _build_provenance(
        source_tool=source_tool,
        source_path=source_path,
        session_id=session_id,
        content_hash=content_hash,
    )

    if yaml:
        fm_str = yaml.dump(frontmatter, default_flow_style=False, sort_keys=False)
    else:
        # Fallback: manual YAML
        fm_lines = []
        for k, v in frontmatter.items():
            if isinstance(v, list):
                if any(isinstance(i, (dict, list)) for i in v):
                    # S1: causal_relations is a list of dicts — JSON is valid
                    # YAML, unlike str() of a python dict.
                    fm_lines.append(f"{k}: {json.dumps(v)}")
                else:
                    fm_lines.append(f"{k}: [{', '.join(str(i) for i in v)}]")
            elif isinstance(v, dict):
                fm_lines.append(f"{k}:")
                for dk, dv in v.items():
                    fm_lines.append(f'  {dk}: "{dv}"' if isinstance(dv, str) else f"  {dk}: {dv}")
            else:
                fm_lines.append(f'{k}: "{v}"' if isinstance(v, str) else f"{k}: {v}")
        fm_str = '\n'.join(fm_lines)

    content = f"---\n{fm_str}---\n\n## Problem\n\n{problem}\n\n## Solution\n\n{solution}\n"
    if context:
        content += f"\n## Context\n\n{context}\n"

    # M5: verify commit-like refs in the LLM-authored body against the local
    # repo before persistence. Hallucinated refs get recorded in frontmatter
    # (so recall can downrank / warn); a note where EVERY ref is fabricated is
    # rejected outright.
    try:
        from commit_verifier import verify_refs
        # Verify against the PROJECT repo (CLAUDE_PROJECT_DIR / nearest .git),
        # not Path.cwd() — the drain runs with a neutral $HOME cwd.
        # S1: rule/fix are LLM-authored too — a fabricated SHA there must not
        # bypass the hallucination check.
        report = verify_refs(
            f"{problem}\n{solution}\n{context}\n{fix}\n{rule}",
            repo_dir=get_project_dir(),
        )
        if report.all_unverified:
            raise ValueError(
                "all_refs_hallucinated: every commit ref in this note "
                f"({', '.join(report.unverified)}) is absent from the repo"
            )
        if report.checked and report.unverified:
            frontmatter["unverified_refs"] = report.unverified
            if yaml:
                fm_str = yaml.dump(frontmatter, default_flow_style=False, sort_keys=False)
                content = f"---\n{fm_str}---\n\n## Problem\n\n{problem}\n\n## Solution\n\n{solution}\n"
                if context:
                    content += f"\n## Context\n\n{context}\n"
    except ImportError:  # pragma: no cover — verifier is best-effort
        pass

    # S6: UPDATE (note already on disk, new form differs) → snapshot the old
    # form into the git-readable `.history.yaml` sidecar before overwriting.
    if filepath.exists():
        try:
            previous = filepath.read_text()
        except OSError:
            previous = ""
        if previous and previous != content:
            append_history_sidecar(filepath, previous, reason="update")

    filepath.write_text(content)
    return filepath, slug


# ---------------------------------------------------------------------------
# Episode notes
# ---------------------------------------------------------------------------


def create_episode_note(
    signals: list,
    learnings: list,
    session_narrative: str = "",
    source_tool: str = "",
    source_path: str = "",
    session_id: str = "",
) -> Path:
    """Create an episode note in ~/.reflect/episodes/."""
    episodes_dir = get_episodes_dir()
    ensure_directories(episodes_dir)

    date_str = datetime.now().strftime('%Y%m%d')
    hash_str = datetime.now().strftime('%H%M%S')
    episode_id = f"ep-{date_str}-{hash_str}"
    filepath = episodes_dir / f"{episode_id}.md"

    learning_ids = [l.get('id', 'unknown') for l in learnings]

    provenance = _build_provenance(
        source_tool=source_tool,
        source_path=source_path,
        session_id=session_id,
    )

    # Format provenance as YAML-ish block
    prov_lines = "\n".join(f"  {k}: {v}" for k, v in provenance.items())

    content = f"""---
type: episode
id: {episode_id}
created: {datetime.now().isoformat()}
project: {get_project_name()}
tags: []
extracted_learnings: {learning_ids}
provenance:
{prov_lines}
---

## Session Context

{session_narrative or 'Reflection session.'}

## Signals Detected

"""
    for signal in signals:
        content += f"- [{signal.get('confidence', 'LOW')}] {signal.get('signal', '')}\n"

    content += "\n## Learnings Extracted\n\n"
    for lid in learning_ids:
        content += f"- {lid}\n"

    filepath.write_text(content)
    return filepath


# ---------------------------------------------------------------------------
# Skills
# ---------------------------------------------------------------------------


def create_skill_file(skill_name: str, skill_content: str) -> Path:
    """Create a new skill file in the project's .claude/skills/ directory."""
    skill_dir = get_project_skills_dir() / skill_name
    ensure_directories(skill_dir)

    skill_path = skill_dir / 'SKILL.md'
    skill_path.write_text(skill_content)

    return skill_path


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main():
    """CLI entry point."""
    parser = argparse.ArgumentParser(description='Generate reflection outputs')
    parser.add_argument('--reflection-data', type=str,
                       help='JSON string with reflection data')
    parser.add_argument('--create-skill', type=str,
                       help='Create a new skill with this name')
    parser.add_argument('--content', type=str,
                       help='Content for skill file')
    parser.add_argument('--show-paths', action='store_true',
                       help='Show all output paths')
    parser.add_argument('--json', action='store_true',
                       help='Output as JSON')

    args = parser.parse_args()

    if args.show_paths:
        paths = {
            'project_solutions': str(get_solutions_dir()),
            'project_skills': str(get_project_skills_dir()),
            'episodes': str(get_episodes_dir()),
        }
        if args.json:
            print(json.dumps(paths, indent=2))
        else:
            print("\n=== Output Paths ===\n")
            for key, path in paths.items():
                print(f"{key}: {path}")
        return

    if args.create_skill:
        if not args.content:
            print("Error: --content required when creating a skill", file=sys.stderr)
            sys.exit(1)

        skill_path = create_skill_file(args.create_skill, args.content)
        if args.json:
            print(json.dumps({'skill_path': str(skill_path)}))
        else:
            print(f"Created skill at: {skill_path}")
        return

    if args.reflection_data:
        try:
            data = json.loads(args.reflection_data)
        except json.JSONDecodeError as e:
            print(f"Error parsing JSON: {e}", file=sys.stderr)
            sys.exit(1)

        # Extract provenance fields from input data
        prov = data.get("provenance", {})

        # Create episode note
        episode_path = create_episode_note(
            signals=data.get('signals', []),
            learnings=data.get('learnings', []),
            session_narrative=data.get('session_narrative', ''),
            source_tool=prov.get("source_tool", ""),
            source_path=prov.get("source_path", ""),
            session_id=prov.get("session_id", ""),
        )

        result = {
            'episode_file': str(episode_path),
            'knowledge_notes': [],
            'skills_created': [],
        }

        if args.json:
            print(json.dumps(result, indent=2))
        else:
            print(f"\n=== Reflection Generated ===\n")
            print(f"Episode: {result['episode_file']}")
        return

    parser.print_help()


if __name__ == '__main__':
    main()
