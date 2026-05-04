---
name: sync-learnings
description: Sync user-level agent config changes back to toolkit repository (works for Claude, Codex, Copilot)
user-invocable: true
---

# Sync Learnings

Bidirectional sync between `~/.claude/` (user-level) and toolkit `packages/` (canonical source).

## Purpose

When working on projects, learnings get captured in user-level agent files via `/reflect`. This command syncs improvements bidirectionally:
- **TO_REPO**: New/updated files in ~/.claude -> packages/ (canonical)
- **TO_HOME**: Newer files in packages/ -> ~/.claude

## Architecture

```
~/.claude/  <--sync-->  packages/  --generates-->  claude-code-4.5/ (thin layer)
                             |
                    create-rule.js installs
```

**packages/** is the canonical source. **claude-code-4.5/CLAUDE.md** is the canonical agent instructions file; codex and copilot symlink their AGENTS.md to it.

## Workflow

1. **Assess**: Compare directories and categorize differences
2. **Reverse Scan**: Enumerate all items in ~/.claude/ and flag orphans missing from packages/
3. **Plugin Audit**: Cross-check installed plugins against external-dependencies.yaml manifest
4. **Route**: Determine target package directory for each file
5. **Plan**: Generate sync plan table with actions and rationale
6. **Execute**: Copy files in parallel where possible
7. **Commit**: Single commit for TO_REPO changes

## Directory Mappings

### Agents (direct mapping)

| Source (user-level) | Target (packages) |
|---------------------|-------------------|
| `~/.claude/agents/engineering/` | `packages/agents/engineering/` |
| `~/.claude/agents/universal/` | `packages/agents/universal/` |
| `~/.claude/agents/orchestrators/` | `packages/agents/orchestrators/` |
| `~/.claude/agents/design/` | `packages/agents/design/` |
| `~/.claude/agents/meta/` | `packages/agents/meta/` |
| `~/.claude/agents/swarm/` | `packages/agents/swarm/` |
| `~/.claude/agents/*.md` (root) | `packages/agents/` |

### Commands (routed by type)

Commands are routed based on their purpose:

| Command Pattern | Target |
|-----------------|--------|
| `m-*` (m-plan, m-implement, m-monitor, m-workflow) | `packages/workflows/multi-agent/commands/` |
| `swarm-*` (swarm-create, swarm-status, swarm-inbox, swarm-join, swarm-shutdown) | `packages/workflows/multi-agent/commands/` |
| `spawn-agent`, `*-agent-worktree`, `merge-agent-work`, `recover-sessions` | `packages/workflows/multi-agent/commands/` |
| `plan`, `implement`, `validate`, `research`, `workflow` | `packages/workflows/single-agent/commands/` |
| All other commands | `packages/utilities/commands/` |

### Utils (shell libraries)

| Source (user-level) | Target (packages) |
|---------------------|-------------------|
| `~/.claude/utils/` | `packages/utilities/utils/` |

### Tool-Specific Config Files

These are NOT in `packages/` — they live in per-tool directories:

| Source (user-level) | Target (toolkit source) |
|---------------------|------------------------|
| `~/.claude/CLAUDE.md` | `toolkit/claude-code-4.5/CLAUDE.md` |
| `~/.claude/settings.json` | `toolkit/claude-code-4.5/settings.json` |
| `~/.claude/statusline.sh` | `toolkit/claude-code-4.5/statusline.sh` (+x preserved) |

**CLAUDE.md sync requires reverse template interpolation** — when copying TO_REPO,
replace interpolated paths back to template placeholders:
- `~/.claude/` or `/.claude/` → `~/.claude/`
- `.claude/` (in path context) → `.claude/`

### Other Files

| Source (user-level) | Target (packages) |
|---------------------|-------------------|
| `~/.claude/skills/` | `packages/skills/` |
| `~/.claude/templates/` | `packages/utilities/templates/` |
| `~/.claude/hooks/` | `packages/utilities/hooks/` |
| `~/.claude/output-styles/` | `packages/utilities/output-styles/` |

## Command Routing Logic

```bash
# Determine target directory for a command
route_command() {
  local cmd="$1"
  case "$cmd" in
    m-*|swarm-*)
      echo "packages/workflows/multi-agent/commands/"
      ;;
    spawn-agent.md|*-agent-worktree.md|merge-agent-work.md|recover-sessions.md)
      echo "packages/workflows/multi-agent/commands/"
      ;;
    plan.md|implement.md|validate.md|research.md|workflow.md)
      echo "packages/workflows/single-agent/commands/"
      ;;
    *)
      echo "packages/utilities/commands/"
      ;;
  esac
}
```

## Exclusion Categories (Never Sync)

### Category 1: Personal Instrumentation
Files with personal/optional integrations:
- `hooks/*.py` with Langfuse, telemetry, or personal API integrations
- Keep separate - don't pollute shared repo with personal tooling

### Category 2: Project-Specific Commands
Commands for specific private projects:
- `commands/data-setup-*.md` - Project-specific data setup
- `commands/load-frameworks.md` - Project-specific framework loading

### Category 3: Session/Ephemeral Data
Runtime state:
- `session/` - Session state files
- `reflections/` - Reflection logs
- `plans/` - Temporary plan files
- `*.json` session files

### Category 4: Machine-Local Settings Overrides
Per-host user overrides (not shared):
- `settings.local.json` — NEVER sync (machine-specific permission prompts, etc.)

NOTE: `settings.json` IS synced — it's the canonical shared config. Reverse-
interpolate paths when copying TO repo. See "Tool-Specific Config Files" above.

### Category 5: Plugin-Managed Content
Content managed by external plugins (installed via `claude plugin install`):
- `plugins/` - Plugin cache and state
- Skills/commands provided by plugins (e.g., Beads provides its own skill in `plugins/cache/`)
- These are tracked in `external-dependencies.yaml`, NOT in `packages/`

## Reverse Scan Phase (Orphan Detection)

CRITICAL: Before comparing files, enumerate ALL items in ~/.claude/ and check each one has a counterpart in packages/. This catches skills/agents that were created directly in ~/.claude/ but never added to the toolkit.

```bash
# Reverse scan: find skills in /.claude with no packages/ counterpart
echo "=== Orphan Detection: Skills ==="
for skill_dir in /.claude/skills/*/; do
  skill_name=$(basename "$skill_dir")
  if [ ! -d "packages/skills/$skill_name" ]; then
    echo "ORPHAN: skills/$skill_name (in /.claude only, missing from packages/)"
  fi
done

echo "=== Orphan Detection: Agents ==="
for agent_file in /.claude/agents/**/*.md; do
  rel_path="${agent_file#/.claude/}"
  if [ ! -f "packages/$rel_path" ]; then
    echo "ORPHAN: $rel_path (in /.claude only, missing from packages/)"
  fi
done
```

Orphans should be classified as:
- **SYNC TO REPO** — Generic, reusable skill/agent → copy to packages/
- **EXTERNAL** — Installed by plugin/npx → verify in external-dependencies.yaml
- **PERSONAL** — User-specific, project-specific → add to exclusion list
- **DEPRECATED** — No longer needed → candidate for removal

## Plugin & External Dependency Audit

Cross-check installed plugins and external skills against `external-dependencies.yaml`:

```bash
MANIFEST="toolkit/external-dependencies.yaml"

# 1. Check installed claude plugins vs manifest
echo "=== Plugin Audit ==="
for plugin_dir in /.claude/plugins/cache/*/; do
  plugin_name=$(basename "$plugin_dir" | sed 's/-marketplace$//')
  if ! grep -q "name: $plugin_name" "$MANIFEST" 2>/dev/null; then
    echo "UNTRACKED PLUGIN: $plugin_name (installed but not in manifest)"
  fi
done

# 2. Check manifest plugins are actually installed
# (Parse claude-plugins entries from YAML and verify each exists)

# 3. Check skills that reference external CLIs
echo "=== Dependency Cross-Check ==="
for skill_dir in packages/skills/*/; do
  skill_file="$skill_dir/SKILL.md"
  [ -f "$skill_file" ] || continue

  # Look for references to external CLIs (bd, npx, brew, etc.)
  if grep -qE '\bbd\b.*show|bd\b.*ready|bd\b.*create|bd\b.*swarm' "$skill_file"; then
    echo "EXTERNAL DEP: $(basename $skill_dir) → requires 'bd' CLI (Beads)"
    if ! grep -q "name: beads" "$MANIFEST" 2>/dev/null; then
      echo "  WARNING: beads not tracked in external-dependencies.yaml"
    fi
  fi
done
```

The audit report should include:

```markdown
## Plugin & External Dependency Audit

| Item | Status | Action |
|------|--------|--------|
| beads plugin | Installed v0.49.0, tracked in manifest | OK |
| debug-bridge plugin | Installed v0.2.0, tracked in manifest | OK |
| swarm-create skill | References `bd` CLI → beads tracked | OK |
| mystery-skill | In ~/.claude only, not in packages | SYNC or EXCLUDE |
```

## Assessment Phase

Before syncing, generate an assessment table:

```markdown
# Sync Assessment: ~/.claude <-> packages/

## Summary

| Action | Files | Reason |
|--------|-------|--------|
| **SYNC TO REPO** | N files | Useful generic additions |
| **SYNC TO ~/.claude** | N files | Repo has newer versions |
| **ORPHANS** | N items | In ~/.claude only, need classification |
| **PLUGIN AUDIT** | N plugins | Cross-check with manifest |
| **DON'T SYNC** | Multiple | Project-specific or session data |

---

## ORPHANS (in ~/.claude only)

### 1. `skills/mystery-skill/` (ORPHAN)
- **Source**: ~/.claude/skills/mystery-skill/
- **Classification**: [SYNC TO REPO | EXTERNAL | PERSONAL | DEPRECATED]
- **Action**: [Copy to packages/ | Verify in manifest | Add to exclusions | Remove]

---

## PLUGIN & EXTERNAL DEPENDENCY AUDIT

| Plugin/Dep | Installed | In Manifest | Status |
|------------|-----------|-------------|--------|
| beads | v0.49.0 | Yes | OK |
| debug-bridge | v0.2.0 | Yes | OK |
| untracked-plugin | v1.0.0 | No | ADD TO MANIFEST |

### Skills with External Dependencies

| Skill | Requires | Dependency Tracked | Status |
|-------|----------|-------------------|--------|
| swarm-create | bd (Beads) | Yes | OK |
| [skill] | [cli] | No | NEEDS TRACKING |

---

## SYNC TO REPO (from ~/.claude)

### 1. `path/to/file.md` (NEW|UPDATED)
- **Purpose**: [what this file does]
- **Target**: `packages/[routed-path]/`
- **Assessment**: Valuable addition

---

## SYNC TO ~/.claude (from packages/)

**⚠️ Always interpolate templates after copying** — see Template Interpolation section above.

### 1. `packages/path/to/file.md`
- **Status**: Packages version is NEWER
- **What's new**: [description of changes]
- **Assessment**: Copy to ~/.claude
- **Action**: Copy then interpolate `~/.claude` → `~/.claude/`

---

## DON'T SYNC

### [Category Name]
- `file1.md` - [reason]
```

## Execution

### Reverse Template Interpolation (TO_REPO direction — CRITICAL)

**When copying FROM user-level TO packages/toolkit, REVERSE the interpolation.**
User-level files have resolved paths (`~/.claude/`, `/.claude/`, `.claude/`).
These MUST be converted back to template placeholders before writing to the repo.

```bash
# Reverse interpolation: user-level → packages/toolkit source
# For .md files (documentation references use ~/ form):
perl -pe 's|~/\.claude|~/.claude|g' ~/.claude/CLAUDE.md > toolkit/claude-code-4.5/CLAUDE.md

# For .sh/.py files (bash code uses $HOME form):
perl -pe 's|\$HOME/\.claude|\/.claude|g; s|~/\.claude|~/.claude|g' \
  ~/.claude/skills/some-skill/scripts/script.sh > toolkit/packages/skills/some-skill/scripts/script.sh

# For mixed files (both doc text AND bash code blocks — like CLAUDE.md):
# Step 1: Replace /.claude → /.claude (bash-safe form)
# Step 2: Replace ~/.claude → ~/.claude (doc-safe form)
# Step 3: Replace quoted `.claude/` and "\.claude/" → .claude/ (backtick and double-quote contexts)
# Order matters: $HOME first (most specific), then ~/, then bare .claude/
perl -pe 's|\$HOME/\.claude|\/.claude|g; s|~/\.claude|~/.claude|g; s|`\.claude/|`.claude/|g; s|"\.claude/|".claude/|g' \
  SOURCE > DEST
```

**Always verify** after reverse interpolation:
```bash
# Should find ZERO literal .claude references (except in comments/descriptions):
grep -n '~/\.claude\|\$HOME/\.claude' toolkit/claude-code-4.5/CLAUDE.md
```

**Do NOT reverse-interpolate**:
- The string `.claude` when it's a directory name in a path context (e.g., `Check .claude/session/`) — this should become `.claude/session/`
- But `.claude` as part of a domain name or unrelated word — leave alone

### Template Interpolation (TO_HOME direction — CRITICAL)

**Packages files use `~/.claude` as a cross-tool placeholder.** When syncing
TO `~/.claude` (or any tool's home dir), these MUST be substituted before the file is
written — never leave them as literal strings.

| Template | Claude Code | Codex | Copilot |
|----------|-------------|-------|---------|
| `~/.claude` | `~/.claude/` | `~/.codex/` | `~/.copilot/` |
| `.claude` | `.claude` | `.codex` | `.copilot` |

Use `perl` for safe substitution (avoids shell expansion issues with `~`):

```bash
# After copying FROM packages TO /.claude, interpolate templates:
perl -pi -e 's/\{\{HOME_TOOL_DIR\}\}/$ENV{HOME}\/.claude/g; s/\{\{TOOL_DIR\}\}/.claude/g' /.claude/path/to/SKILL.md

# Or with explicit paths:
perl -pe 's/\{\{HOME_TOOL_DIR\}\}/\/Users\/stevengonsalvez\/.claude/g; s/\{\{TOOL_DIR\}\}/.claude/g' \
  /tmp/SKILL.md > /.claude/path/to/SKILL.md
```

**Always verify** after substitution — `grep -n "HOME_TOOL_DIR\|TOOL_DIR" ~/.claude/...` should return nothing.

### Shell Alias Workaround

Many shells alias `cp` to `cp -i`. Always use `\cp` to bypass:

```bash
# Bypasses alias
\cp source dest
```

### Example Sync Operations

```bash
# Agent sync (direct)
\cp /.claude/agents/engineering/new-agent.md packages/agents/engineering/

# Command sync (routed)
\cp /.claude/commands/m-plan.md packages/workflows/multi-agent/commands/
\cp /.claude/commands/plan.md packages/workflows/single-agent/commands/
\cp /.claude/commands/session-info.md packages/utilities/commands/

# Skill sync
\cp -R /.claude/skills/new-skill/ packages/skills/

# Template sync
\cp /.claude/templates/new-template.md packages/utilities/templates/
```

### Commit Format

```
chore: sync learnings to packages

- Add [new-file]: [brief description]
- Update [updated-file]: [what changed]
```

## Quick Diff Commands

```bash
# CLAUDE.md diff (tool-specific config file)
# Normalize template vars before comparing to avoid false positives
diff <(perl -pe 's|\{\{TOOL_DIR\}\}|.claude|g; s|\{\{HOME_TOOL_DIR\}\}|~/.claude|g' \
  toolkit/claude-code-4.5/CLAUDE.md) /.claude/CLAUDE.md

# settings.json diff (Claude Code harness config)
# Fails if repo has drifted from /.claude — sync TO_REPO if live has newer keys
diff toolkit/claude-code-4.5/settings.json /.claude/settings.json

# statusline.sh diff (rich statusline script shipped as tool-specific file)
diff toolkit/claude-code-4.5/statusline.sh /.claude/statusline.sh

# Find all differences (agents)
diff -rq /.claude/agents/ packages/agents/ 2>/dev/null

# Find all differences (commands - check all locations)
for dir in packages/utilities/commands packages/workflows/*/commands; do
  diff -rq /.claude/commands/ "$dir" 2>/dev/null | head -5
done

# Find files only in /.claude (candidates for TO_REPO)
diff -rq /.claude/agents/ packages/agents/ 2>/dev/null | grep "Only in /Users"

# Show actual diff for a specific file
diff /.claude/agents/engineering/example.md packages/agents/engineering/example.md

# REVERSE SCAN: Find orphaned skills (in /.claude but not in packages)
comm -23 \
  <(ls -1 /.claude/skills/ 2>/dev/null | sort) \
  <(ls -1 packages/skills/ 2>/dev/null | sort)

# REVERSE SCAN: Find orphaned agents
comm -23 \
  <(find /.claude/agents/ -name '*.md' -exec basename {} \; 2>/dev/null | sort) \
  <(find packages/agents/ -name '*.md' -exec basename {} \; 2>/dev/null | sort)

# PLUGIN AUDIT: List installed plugins not in manifest
for d in /.claude/plugins/cache/*/; do
  name=$(basename "$d" | sed 's/-marketplace$//')
  grep -q "name: $name" toolkit/external-dependencies.yaml 2>/dev/null || echo "UNTRACKED: $name"
done

# DEPENDENCY CHECK: Skills that reference 'bd' (Beads)
grep -rl '\bbd\b' packages/skills/*/SKILL.md 2>/dev/null
```

## Safety Checks

- **Never overwrite** without assessment first
- **Check modification times** when both versions exist - sync newer to older
- **Skip binaries** and non-text files
- **Validate** markdown frontmatter before copying agent files
- **Route commands** to correct package directory
- **Exclude** files matching exclusion categories

## Example Session

```
User: /sync-learnings

Claude: # Sync Assessment: ~/.claude <-> packages/

## Summary

| Action | Files | Reason |
|--------|-------|--------|
| **SYNC TO REPO** | 3 files | Useful generic additions |
| **SYNC TO ~/.claude** | 1 file | Packages has newer version |
| **ORPHANS** | 0 items | All skills/agents accounted for |
| **PLUGIN AUDIT** | 6 plugins | All tracked in manifest |
| **DON'T SYNC** | Multiple | Project-specific or session data |

## ORPHANS

No orphaned skills or agents detected. All items in ~/.claude/ have
counterparts in packages/ or are tracked in external-dependencies.yaml.

## PLUGIN & EXTERNAL DEPENDENCY AUDIT

| Plugin | Installed | In Manifest | Status |
|--------|-----------|-------------|--------|
| beads | v0.49.0 | Yes | OK |
| debug-bridge | v0.2.0 | Yes | OK |
| ralph-loop | v55b58ec | Yes | OK |
| code-review | v55b58ec | Yes | OK |
| playground | latest | Yes | OK |
| dev-browser | v66682fb | Yes | OK |

| Skill | Requires | Tracked | Status |
|-------|----------|---------|--------|
| swarm-create | bd (Beads) | Yes | OK |

## SYNC TO REPO

### 1. `agents/engineering/new-validator.md` (NEW)
- **Purpose**: Custom validation agent
- **Target**: `packages/agents/engineering/`
- **Assessment**: Generic utility

### 2. `commands/custom-workflow.md` (NEW)
- **Purpose**: New workflow command
- **Target**: `packages/utilities/commands/` (routed)
- **Assessment**: Generic utility

## SYNC TO ~/.claude

### 1. `packages/utilities/commands/sync-learnings.md`
- **Status**: Packages version is NEWER
- **What's new**: Added command routing logic
- **Assessment**: Copy to ~/.claude

Proceed with sync? [Y/n]

User: Y

Claude: Executing sync...

new-validator.md -> packages/agents/engineering/
custom-workflow.md -> packages/utilities/commands/
sync-learnings.md -> ~/.claude/commands/

Committed: chore: sync learnings to packages
```

## Automation Tip

Add to session start hook for automatic detection:

```bash
# In hooks/session-start
DIFF_COUNT=$(diff -rq /.claude/agents/ packages/agents/ 2>/dev/null | grep "differ" | wc -l)
if [ "$DIFF_COUNT" -gt 0 ]; then
  echo "  $DIFF_COUNT agent files differ from packages. Run /sync-learnings to sync."
fi
```
