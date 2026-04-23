#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Inject the Step 0 recall preamble into Tier 1 + 2 skills.

Idempotent: skipped if <!-- recall:begin --> already present.
Inserts the block immediately before the first H2 heading in the file.
"""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]  # repo root
SKILLS = ROOT / "toolkit" / "packages" / "skills"

# (skill_name, tier, query_hint) — query_hint is skill-specific guidance
# on how to build the recall query from user input / context.
TARGETS = [
    # ─── Tier 1 — mandatory recall preamble ────────────────────────────
    ("plan", 1, "concatenate the user's task description + any file paths + domain keywords (e.g. `\"user auth OAuth migration\"`)"),
    ("plan-tdd", 1, "task description + `\"testing strategy\"` + target module name (e.g. `\"payment webhook testing strategy\"`)"),
    ("plan-gh", 1, "task scope + `\"github issue\"` + repo/project name (e.g. `\"dashboard rewrite github issue frontend\"`)"),
    ("research", 1, "the research topic verbatim + domain keywords (e.g. `\"redis connection pooling node\"`)"),
    ("brainstorm", 1, "the topic or problem being brainstormed + any constraint hints from the user (e.g. `\"caching strategy mobile offline\"`)"),
    ("critique", 1, "a short summary of the approach/code being critiqued + relevant domain keywords (e.g. `\"event-sourced migration rollback strategy\"`)"),

    # ─── Tier 2 — recommended recall preamble ──────────────────────────
    ("implement", 2, "the plan's current phase name + touched module names (e.g. `\"auth middleware session token storage\"`). Skip if the calling /plan already ran recall and surfaced findings."),
    ("gh-issue", 2, "issue title + top labels + repo name (e.g. `\"flaky signup test frontend auth\"`)"),
    ("find-missing-tests", 2, "target module/package name + `\"testing\"` (e.g. `\"payment webhook handler testing\"`)"),
    ("validate", 2, "the feature/change being validated + relevant verification keywords (e.g. `\"OAuth callback validation edge cases\"`)"),
    # `review` skill is provided by an external plugin (not in toolkit/packages/skills/) — skipped.
]


BEGIN_MARK = "<!-- recall:begin -->"
END_MARK = "<!-- recall:end -->"


def build_block(skill: str, tier: int, query_hint: str) -> str:
    tier_word = "MANDATORY" if tier == 1 else "RECOMMENDED"
    verb = {
        "plan": "planning",
        "plan-tdd": "writing the TDD plan",
        "plan-gh": "generating issues",
        "research": "researching",
        "brainstorm": "brainstorming",
        "critique": "critiquing",
        "implement": "implementing",
        "gh-issue": "fixing the issue",
        "find-missing-tests": "identifying test gaps",
        "validate": "validating",
    }[skill]
    return f"""{BEGIN_MARK}

## Step 0: Prior-art check ({tier_word})

Before {verb}, recall prior learnings from the global knowledge base so we don't re-learn or re-decide something already captured:

```bash
uv run "{{{{HOME_TOOL_DIR}}}}/skills/recall/scripts/recall.py" \\
  "<QUERY>" \\
  --limit 5 --format markdown
```

**Query construction for `/{skill}`**: {query_hint}.

**What to do with results:**

- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant to the task — surface it to the user BEFORE proceeding with this skill's main flow.
- If nothing relevant returns — proceed silently, no need to mention the check.
- Never block on recall failure. Empty output / non-zero exit is expected when the KB is absent or the subprocess errors — treat it as "no prior art found", not as an error.

{END_MARK}

"""


def inject(skill: str, tier: int, query_hint: str) -> str:
    """Return 'ok' / 'skipped' / 'missing'."""
    path = SKILLS / skill / "SKILL.md"
    if not path.exists():
        return "missing"
    content = path.read_text()
    if BEGIN_MARK in content:
        return "skipped"

    # Insert before the first H2 after the frontmatter.
    # The frontmatter is a `---` … `---` block at the very top.
    lines = content.splitlines(keepends=True)
    insert_at = None
    in_frontmatter = False
    seen_frontmatter_close = False
    for i, line in enumerate(lines):
        if i == 0 and line.startswith("---"):
            in_frontmatter = True
            continue
        if in_frontmatter and line.startswith("---"):
            in_frontmatter = False
            seen_frontmatter_close = True
            continue
        if in_frontmatter:
            continue
        # After frontmatter, find first H2 (or H1 that starts the body)
        if line.startswith("## "):
            insert_at = i
            break
        # Or first numbered step (skills that use bare `1. ...` lists)
        if line.lstrip().startswith("1. ") and i > 0:
            insert_at = i
            break

    if insert_at is None:
        # No H2 found — append after the H1 intro, or at end
        return "no-insertion-point"

    block = build_block(skill, tier, query_hint)
    new_content = "".join(lines[:insert_at]) + block + "".join(lines[insert_at:])
    path.write_text(new_content)
    return "ok"


def main() -> int:
    results = {}
    for skill, tier, hint in TARGETS:
        results[skill] = inject(skill, tier, hint)
    print(f"{'skill':<20}  {'result':<10}")
    print("─" * 32)
    for skill, result in results.items():
        marker = "✓" if result in ("ok", "skipped") else "✗"
        print(f"{marker} {skill:<18}  {result}")
    bad = [s for s, r in results.items() if r not in ("ok", "skipped")]
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
