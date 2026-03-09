---
name: prime
description: |
  Load context for a new agent session by analyzing codebase structure, README,
  and relevant past learnings from the knowledge base.
user-invocable: true
allowed-tools:
  - Bash
  - Read
---

# Prime

This command loads essential context for a new agent session by examining the codebase structure, reading the project README, and loading relevant past learnings from the knowledge base.

## Instructions
- Run `git ls-files` to understand the codebase structure and file organization
- Read the README.md to understand the project purpose, setup instructions, and key information
- Provide a concise overview of the project based on the gathered context

## Knowledge Context Loading

After analyzing the codebase structure, load relevant learnings from the knowledge base.

### Step 1: Detect Project Tech Stack
From the codebase structure and README, identify:
- Primary language(s)
- Frameworks in use
- Key technologies (databases, message queues, etc.)

### Step 2: Load Relevant Learnings

```bash
LEARNINGS_HOME="${LEARNINGS_HOME:-$HOME/.learnings}"

# Search for learnings related to detected tech stack
if command -v qmd &>/dev/null; then
    qmd query --collection learnings --json "{detected stack keywords}" 2>/dev/null
fi

# Get critical patterns for the detected language/domain
if [ -x "$LEARNINGS_HOME/cli/learnings" ]; then
    "$LEARNINGS_HOME/cli/learnings" critical-patterns --language {lang} --domain {domain} 2>/dev/null
fi
```

### Step 3: Present Context
- Include the top 3-5 most relevant learnings in the session overview
- Highlight any critical patterns that apply to this project's tech stack
- Note: If no learnings are found, skip silently (don't clutter output)

## Context
- Codebase structure git accessible: !`git ls-files`
- Codebase structure all: !`eza . --tree`
- Project README: @README.md
- Past learnings: !`qmd query --collection learnings --json "$(basename $(pwd))" 2>/dev/null || true`
