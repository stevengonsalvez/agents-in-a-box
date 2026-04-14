#!/usr/bin/env python3
"""
Output Generator for Reflect Skill

Generates reflection output files and manages indexes:
- Project knowledge notes: docs/solutions/{category}/{name}.md
- Entity sidecars: docs/solutions/{category}/{name}.entities.yaml
- Episode notes: ~/.reflect/episodes/ep-{date}-{hash}.md
- Project skills: .claude/skills/{name}/SKILL.md

Note: v1/v2 .claude/reflections/ paths are deprecated. All knowledge output
now routes through docs/solutions/ (project-scoped) and the learnings CLI
(global GraphRAG indexing).

Usage:
    python output_generator.py --reflection-data '{"signals": [...], "changes": [...]}'
    python output_generator.py --create-skill skill-name --content '...'
"""

import argparse
import json
import os
import sys
from datetime import datetime
from pathlib import Path
from typing import Optional

try:
    import yaml
except ImportError:
    yaml = None


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
    return get_project_dir() / 'docs' / 'solutions'


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
    language: str = "",
    framework: str = "",
) -> tuple[Path, str]:
    """
    Create a knowledge note in docs/solutions/{category}/.

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
    frontmatter = {
        'title': title,
        'category': category,
        'tags': tags,
        'symptoms': symptoms,
        'root_cause': root_cause,
        'key_insight': key_insight,
        'created': datetime.now().strftime('%Y-%m-%d'),
        'confidence': confidence,
    }
    if language:
        frontmatter['language'] = language
    if framework:
        frontmatter['framework'] = framework

    if yaml:
        fm_str = yaml.dump(frontmatter, default_flow_style=False, sort_keys=False)
    else:
        # Fallback: manual YAML
        fm_lines = []
        for k, v in frontmatter.items():
            if isinstance(v, list):
                fm_lines.append(f"{k}: [{', '.join(str(i) for i in v)}]")
            else:
                fm_lines.append(f'{k}: "{v}"' if isinstance(v, str) else f"{k}: {v}")
        fm_str = '\n'.join(fm_lines)

    content = f"---\n{fm_str}---\n\n## Problem\n\n{problem}\n\n## Solution\n\n{solution}\n"
    if context:
        content += f"\n## Context\n\n{context}\n"

    filepath.write_text(content)
    return filepath, slug


def create_episode_note(
    signals: list,
    learnings: list,
    session_narrative: str = "",
) -> Path:
    """Create an episode note in ~/.reflect/episodes/."""
    episodes_dir = get_episodes_dir()
    ensure_directories(episodes_dir)

    date_str = datetime.now().strftime('%Y%m%d')
    hash_str = datetime.now().strftime('%H%M%S')
    episode_id = f"ep-{date_str}-{hash_str}"
    filepath = episodes_dir / f"{episode_id}.md"

    learning_ids = [l.get('id', 'unknown') for l in learnings]

    content = f"""---
type: episode
id: {episode_id}
created: {datetime.now().isoformat()}
project: {get_project_name()}
tags: []
extracted_learnings: {learning_ids}
---

## Session Context

{session_narrative or 'Reflection session.'}

## Signals Detected

"""
    for i, signal in enumerate(signals, 1):
        content += f"- [{signal.get('confidence', 'LOW')}] {signal.get('signal', '')}\n"

    content += "\n## Learnings Extracted\n\n"
    for lid in learning_ids:
        content += f"- {lid}\n"

    filepath.write_text(content)
    return filepath


def create_skill_file(skill_name: str, skill_content: str) -> Path:
    """Create a new skill file in the project's .claude/skills/ directory."""
    skill_dir = get_project_skills_dir() / skill_name
    ensure_directories(skill_dir)

    skill_path = skill_dir / 'SKILL.md'
    skill_path.write_text(skill_content)

    return skill_path


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

        # Create episode note
        episode_path = create_episode_note(
            signals=data.get('signals', []),
            learnings=data.get('learnings', []),
            session_narrative=data.get('session_narrative', ''),
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
