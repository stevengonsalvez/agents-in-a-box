---
name: sync-learnings
description: Sync user-level agent config changes back to toolkit repository (works for Claude, Codex, Copilot)
user-invocable: true
---

# Sync Learnings

Bidirectional sync between `{{HOME_TOOL_DIR}}/` (user-level) and toolkit `packages/` (canonical source).

## Purpose

When working on projects, learnings get captured in user-level agent files via `/reflect`. This command syncs improvements bidirectionally:
- **TO_REPO**: New/updated files in {{HOME_TOOL_DIR}} -> packages/ (canonical)
- **TO_HOME**: Newer files in packages/ -> {{HOME_TOOL_DIR}}

## Architecture

```
{{HOME_TOOL_DIR}}/  <--sync-->  packages/  --generates-->  claude-code-4.5/ (thin layer)
                             |
                    create-rule.js installs
```

**packages/** is the canonical source. **claude-code-4.5/CLAUDE.md** is the canonical agent instructions file; codex and copilot symlink their AGENTS.md to it.

## Workflow

1. **Assess**: Compare directories and categorize differences
2. **Reverse Scan**: Enumerate all items in {{HOME_TOOL_DIR}}/ and flag orphans missing from packages/
3. **Plugin Audit**: Cross-check installed plugins against external-dependencies.yaml manifest
4. **Route**: Determine target package directory for each file
5. **Plan**: Generate sync plan table with actions and rationale
6. **Execute**: Copy files in parallel where possible
7. **Commit**: Single commit for TO_REPO changes

## Directory Mappings

### Agents (direct mapping)

| Source (user-level) | Target (packages) |
|---------------------|-------------------|
| `{{HOME_TOOL_DIR}}/agents/engineering/` | `packages/agents/engineering/` |
| `{{HOME_TOOL_DIR}}/agents/universal/` | `packages/agents/universal/` |
| `{{HOME_TOOL_DIR}}/agents/orchestrators/` | `packages/agents/orchestrators/` |
| `{{HOME_TOOL_DIR}}/agents/design/` | `packages/agents/design/` |
| `{{HOME_TOOL_DIR}}/agents/meta/` | `packages/agents/meta/` |
| `{{HOME_TOOL_DIR}}/agents/swarm/` | `packages/agents/swarm/` |
| `{{HOME_TOOL_DIR}}/agents/*.md` (root) | `packages/agents/` |

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
| `{{HOME_TOOL_DIR}}/utils/` | `packages/utilities/utils/` |

### Other Files

| Source (user-level) | Target (packages) |
|---------------------|-------------------|
| `{{HOME_TOOL_DIR}}/skills/` | `packages/skills/` |
| `{{HOME_TOOL_DIR}}/templates/` | `packages/utilities/templates/` |
| `{{HOME_TOOL_DIR}}/hooks/` | `packages/utilities/hooks/` |
| `{{HOME_TOOL_DIR}}/output-styles/` | `packages/utilities/output-styles/` |

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

### Category 4: User Settings
Personal configuration:
- `settings.json`, `settings.local.json`

### Category 5: Plugin-Managed Content
Content managed by external plugins (installed via `claude plugin install`):
- `plugins/` - Plugin cache and state
- Skills/commands provided by plugins (e.g., Beads provides its own skill in `plugins/cache/`)
- These are tracked in `external-dependencies.yaml`, NOT in `packages/`

## Reverse Scan Phase (Orphan Detection)

CRITICAL: Before comparing files, enumerate ALL items in {{HOME_TOOL_DIR}}/ and check each one has a counterpart in packages/. This catches skills/agents that were created directly in {{HOME_TOOL_DIR}}/ but never added to the toolkit.

```bash
# Reverse scan: find skills in $HOME/{{TOOL_DIR}} with no packages/ counterpart
echo "=== Orphan Detection: Skills ==="
for skill_dir in $HOME/{{TOOL_DIR}}/skills/*/; do
  skill_name=$(basename "$skill_dir")
  if [ ! -d "packages/skills/$skill_name" ]; then
    echo "ORPHAN: skills/$skill_name (in $HOME/{{TOOL_DIR}} only, missing from packages/)"
  fi
done

echo "=== Orphan Detection: Agents ==="
for agent_file in $HOME/{{TOOL_DIR}}/agents/**/*.md; do
  rel_path="${agent_file#$HOME/{{TOOL_DIR}}/}"
  if [ ! -f "packages/$rel_path" ]; then
    echo "ORPHAN: $rel_path (in $HOME/{{TOOL_DIR}} only, missing from packages/)"
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
for plugin_dir in $HOME/{{TOOL_DIR}}/plugins/cache/*/; do
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
| mystery-skill | In {{HOME_TOOL_DIR}} only, not in packages | SYNC or EXCLUDE |
```

## Assessment Phase

Before syncing, generate an assessment table:

```markdown
# Sync Assessment: {{HOME_TOOL_DIR}} <-> packages/

## Summary

| Action | Files | Reason |
|--------|-------|--------|
| **SYNC TO REPO** | N files | Useful generic additions |
| **SYNC TO {{HOME_TOOL_DIR}}** | N files | Repo has newer versions |
| **ORPHANS** | N items | In {{HOME_TOOL_DIR}} only, need classification |
| **PLUGIN AUDIT** | N plugins | Cross-check with manifest |
| **DON'T SYNC** | Multiple | Project-specific or session data |

---

## ORPHANS (in {{HOME_TOOL_DIR}} only)

### 1. `skills/mystery-skill/` (ORPHAN)
- **Source**: {{HOME_TOOL_DIR}}/skills/mystery-skill/
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

## SYNC TO REPO (from {{HOME_TOOL_DIR}})

### 1. `path/to/file.md` (NEW|UPDATED)
- **Purpose**: [what this file does]
- **Target**: `packages/[routed-path]/`
- **Assessment**: Valuable addition

---

## SYNC TO {{HOME_TOOL_DIR}} (from packages/)

**⚠️ Always interpolate templates after copying** — see Template Interpolation section above.

### 1. `packages/path/to/file.md`
- **Status**: Packages version is NEWER
- **What's new**: [description of changes]
- **Assessment**: Copy to {{HOME_TOOL_DIR}}
- **Action**: Copy then interpolate `{{HOME_TOOL_DIR}}` → `{{HOME_TOOL_DIR}}/`

---

## DON'T SYNC

### [Category Name]
- `file1.md` - [reason]
```

## Execution

### Template Interpolation (CRITICAL)

**Packages files use `{{HOME_TOOL_DIR}}` as a cross-tool placeholder.** When syncing
TO `{{HOME_TOOL_DIR}}` (or any tool's home dir), these MUST be substituted before the file is
written — never leave them as literal strings.

| Template | Claude Code | Codex | Copilot |
|----------|-------------|-------|---------|
| `{{HOME_TOOL_DIR}}` | `{{HOME_TOOL_DIR}}/` | `~/.codex/` | `~/.copilot/` |
| `{{TOOL_DIR}}` | `.claude` | `.codex` | `.copilot` |

Use `perl` for safe substitution (avoids shell expansion issues with `~`):

```bash
# After copying FROM packages TO $HOME/{{TOOL_DIR}}, interpolate templates:
perl -pi -e 's/\{\{HOME_TOOL_DIR\}\}/$ENV{HOME}\/.claude/g; s/\{\{TOOL_DIR\}\}/.claude/g' $HOME/{{TOOL_DIR}}/path/to/SKILL.md

# Or with explicit paths:
perl -pe 's/\{\{HOME_TOOL_DIR\}\}/\/Users\/stevengonsalvez\/.claude/g; s/\{\{TOOL_DIR\}\}/.claude/g' \
  /tmp/SKILL.md > $HOME/{{TOOL_DIR}}/path/to/SKILL.md
```

**Always verify** after substitution — `grep -n "HOME_TOOL_DIR\|TOOL_DIR" {{HOME_TOOL_DIR}}/...` should return nothing.

### Shell Alias Workaround

Many shells alias `cp` to `cp -i`. Always use `\cp` to bypass:

```bash
# Bypasses alias
\cp source dest
```

### Example Sync Operations

```bash
# Agent sync (direct)
\cp $HOME/{{TOOL_DIR}}/agents/engineering/new-agent.md packages/agents/engineering/

# Command sync (routed)
\cp $HOME/{{TOOL_DIR}}/commands/m-plan.md packages/workflows/multi-agent/commands/
\cp $HOME/{{TOOL_DIR}}/commands/plan.md packages/workflows/single-agent/commands/
\cp $HOME/{{TOOL_DIR}}/commands/session-info.md packages/utilities/commands/

# Skill sync
\cp -R $HOME/{{TOOL_DIR}}/skills/new-skill/ packages/skills/

# Template sync
\cp $HOME/{{TOOL_DIR}}/templates/new-template.md packages/utilities/templates/
```

### Commit Format

```
chore: sync learnings to packages

- Add [new-file]: [brief description]
- Update [updated-file]: [what changed]
```

## Quick Diff Commands

```bash
# Find all differences (agents)
diff -rq $HOME/{{TOOL_DIR}}/agents/ packages/agents/ 2>/dev/null

# Find all differences (commands - check all locations)
for dir in packages/utilities/commands packages/workflows/*/commands; do
  diff -rq $HOME/{{TOOL_DIR}}/commands/ "$dir" 2>/dev/null | head -5
done

# Find files only in $HOME/{{TOOL_DIR}} (candidates for TO_REPO)
diff -rq $HOME/{{TOOL_DIR}}/agents/ packages/agents/ 2>/dev/null | grep "Only in /Users"

# Show actual diff for a specific file
diff $HOME/{{TOOL_DIR}}/agents/engineering/example.md packages/agents/engineering/example.md

# REVERSE SCAN: Find orphaned skills (in $HOME/{{TOOL_DIR}} but not in packages)
comm -23 \
  <(ls -1 $HOME/{{TOOL_DIR}}/skills/ 2>/dev/null | sort) \
  <(ls -1 packages/skills/ 2>/dev/null | sort)

# REVERSE SCAN: Find orphaned agents
comm -23 \
  <(find $HOME/{{TOOL_DIR}}/agents/ -name '*.md' -exec basename {} \; 2>/dev/null | sort) \
  <(find packages/agents/ -name '*.md' -exec basename {} \; 2>/dev/null | sort)

# PLUGIN AUDIT: List installed plugins not in manifest
for d in $HOME/{{TOOL_DIR}}/plugins/cache/*/; do
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

Claude: # Sync Assessment: {{HOME_TOOL_DIR}} <-> packages/

## Summary

| Action | Files | Reason |
|--------|-------|--------|
| **SYNC TO REPO** | 3 files | Useful generic additions |
| **SYNC TO {{HOME_TOOL_DIR}}** | 1 file | Packages has newer version |
| **ORPHANS** | 0 items | All skills/agents accounted for |
| **PLUGIN AUDIT** | 6 plugins | All tracked in manifest |
| **DON'T SYNC** | Multiple | Project-specific or session data |

## ORPHANS

No orphaned skills or agents detected. All items in {{HOME_TOOL_DIR}}/ have
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

## SYNC TO {{HOME_TOOL_DIR}}

### 1. `packages/utilities/commands/sync-learnings.md`
- **Status**: Packages version is NEWER
- **What's new**: Added command routing logic
- **Assessment**: Copy to {{HOME_TOOL_DIR}}

Proceed with sync? [Y/n]

User: Y

Claude: Executing sync...

new-validator.md -> packages/agents/engineering/
custom-workflow.md -> packages/utilities/commands/
sync-learnings.md -> {{HOME_TOOL_DIR}}/commands/

Committed: chore: sync learnings to packages
```

## Automation Tip

Add to session start hook for automatic detection:

```bash
# In hooks/session-start
DIFF_COUNT=$(diff -rq $HOME/{{TOOL_DIR}}/agents/ packages/agents/ 2>/dev/null | grep "differ" | wc -l)
if [ "$DIFF_COUNT" -gt 0 ]; then
  echo "  $DIFF_COUNT agent files differ from packages. Run /sync-learnings to sync."
fi
```
