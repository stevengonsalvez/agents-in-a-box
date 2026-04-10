---
name: plugins
description: Manage and track installed plugins, skills, and extensions across all sources
user-invocable: true
---

# /plugins - Extension Manager

Unified view and management of all external dependencies across the toolkit ecosystem.

## Subcommands

- `/plugins` or `/plugins list` - Show all installed extensions
- `/plugins add <name>` - Add a new skill to the ecosystem
- `/plugins sync` - Update manifest from current installed state
- `/plugins export` - Generate reproducible install script
- `/plugins status` - Check versions against manifest

## Universal Skill Lifecycle

Skills follow a single universal format (SKILL.md) across all agents. The toolkit provides
a closed-loop system: skills enter from any tool, get canonicalized, and deploy everywhere.

```
                        SKILL ENTRY POINTS
                        ==================

    Claude Code              Codex CLI              Copilot CLI
    ───────────              ─────────              ───────────
    claude plugin install    $skill-installer       npx add-skill
    npx add-skill            npx add-skill          gh copilot skill
    manual copy              manual copy             manual copy
         │                       │                       │
         ▼                       ▼                       ▼
    {{HOME_TOOL_DIR}}/skills/       ~/.codex/skills/       ~/.copilot/skills/
         │                       │                       │
         └───────────┬───────────┘───────────────────────┘
                     │
                     ▼
              /sync-learnings          ◄── syncs new skills TO toolkit
                     │
                     ▼
         ┌───────────────────────┐
         │   packages/skills/    │     ◄── CANONICAL SOURCE (git-tracked)
         │   (toolkit repo)      │
         └───────────┬───────────┘
                     │
                     ▼
         ┌───────────────────────┐
         │ external-dependencies │     ◄── /plugins add or /plugins sync
         │       .yaml           │         updates the manifest
         └───────────┬───────────┘
                     │
                     ▼
              bootstrap.js             ◄── generates tool-specific output
                     │
          ┌──────────┼──────────┐
          ▼          ▼          ▼
     {{HOME_TOOL_DIR}}/  ~/.codex/  ~/.copilot/
      skills/     skills/     skills/     ◄── ALL tools get ALL skills
          │          │          │
          ▼          ▼          ▼
    setup-external.sh          ◄── git clone for agent-skills (external repos)


                     SKILL TYPES
                     ===========

    ┌─────────────────┬────────────────────┬──────────────────────┐
    │ Bundled          │ Agent-Skill         │ External (plugins)   │
    │ (we own it)      │ (3rd-party git repo)│ (tool-native)        │
    ├─────────────────┼────────────────────┼──────────────────────┤
    │ Lives in         │ SKILL.md stub in   │ Tracked in manifest  │
    │ packages/skills/ │ packages/skills/   │ only (not in repo)   │
    │                  │ + external-source  │                      │
    │ Deployed by      │ Deployed by        │ Installed by         │
    │ bootstrap copy   │ setup-external.sh  │ tool-native command  │
    │                  │ (git clone)        │                      │
    └─────────────────┴────────────────────┴──────────────────────┘


                     CLOSED LOOP
                     ===========

    1. Install skill via ANY tool ──► lands in ~/.{tool}/skills/
    2. /sync-learnings ──────────────► copies to packages/skills/ (canonical)
    3. /plugins sync ────────────────► updates external-dependencies.yaml
    4. bootstrap.js ─────────────────► deploys to ALL tools universally
    5. Repeat from any tool
```

### Skill Type Decision

When adding a new skill, determine its type:

| Question | If YES | If NO |
|----------|--------|-------|
| Do we own/maintain the source? | **Bundled** | next question |
| Is it a public git repo with SKILL.md? | **Agent-skill** | next question |
| Installed via tool-native command? | **External** (manifest-only) | copy as bundled |

## Data Sources

| Source | Location | Format |
|--------|----------|--------|
| Claude Plugins | `{{HOME_TOOL_DIR}}/plugins/installed_plugins.json` | JSON |
| Marketplaces | `{{HOME_TOOL_DIR}}/plugins/known_marketplaces.json` | JSON |
| Bundled Skills | `packages/skills/*/SKILL.md` | Markdown |
| Unified Manifest | `toolkit/external-dependencies.yaml` | YAML |

## /plugins list

Display all installed extensions grouped by category.

### Output Format

```markdown
# Installed Extensions

## Bundled Skills (5)
| Name | Path | Purpose |
|------|------|---------|
| webapp-testing | packages/skills/webapp-testing | Playwright-based testing |
| crypto-research | packages/skills/crypto-research | Cryptocurrency analysis |
| frontend-design | packages/skills/frontend-design | UI/UX generation |
| tmux-monitor | packages/skills/tmux-monitor | Session monitoring |
| retro-pdf | packages/skills/retro-pdf | Markdown to PDF |

## Claude Plugins (4)
| Name | Marketplace | Version | Installed |
|------|-------------|---------|-----------|
| beads | steveyegge/beads | 0.49.0 | 2026-01-24 |
| debug-bridge | stevengonsalvez/agent-bridge | 0.2.0 | 2026-01-11 |
| ralph-loop | anthropics/claude-plugins-official | e30768372b41 | 2026-01-11 |
| code-review | anthropics/claude-plugins-official | e30768372b41 | 2026-01-15 |

## MCP Servers (1)
| Name | Purpose |
|------|---------|
| context7 | Documentation lookup |

## npx Skills (0)
None installed

## Codex Skills (0)
None installed
```

### Implementation

```bash
# Read Claude plugins
jq -r '.plugins | to_entries[] | "\(.key): \(.value[0].version)"' \
  $HOME/{{TOOL_DIR}}/plugins/installed_plugins.json

# Read marketplaces
jq -r 'to_entries[] | "\(.key): \(.value.source.repo)"' \
  $HOME/{{TOOL_DIR}}/plugins/known_marketplaces.json

# Read bundled skills
for skill in packages/skills/*/SKILL.md; do
  dirname "$skill" | xargs basename
done
```

## /plugins add

Add a new skill to the toolkit ecosystem. Handles all three skill types.

### Usage

```
/plugins add <name>                     # interactive — detects source
/plugins add <name> --from {{HOME_TOOL_DIR}}/skills/<name>   # from installed skill
/plugins add <name> --from https://github.com/org/repo  # from git repo
```

### Workflow

#### Step 1: Detect Source and Type

```
Source provided?
  ├─ Local path (~/.*skills/<name>/) ──► check for .git/
  │    ├─ Has .git/ ──► Agent-skill (external repo)
  │    └─ No .git/  ──► Bundled (copy to packages/)
  │
  ├─ Git URL ──► Agent-skill
  │
  └─ No source ──► Scan all tool skill dirs for <name>:
       {{HOME_TOOL_DIR}}/skills/<name>/
       ~/.codex/skills/<name>/
       ~/.copilot/skills/<name>/
       If found: use that path and re-enter detection
       If not found: error — provide --from
```

#### Step 2: Validate SKILL.md

```bash
# Must have valid SKILL.md with frontmatter
SKILL_FILE="<source>/SKILL.md"
# Check: name and description fields present
# Check: no syntax errors in YAML frontmatter
# Extract: name, description, features for manifest entry
```

#### Step 3: Execute by Type

**Bundled skill** (we own it — copy into repo):

1. Copy skill directory to `packages/skills/<name>/`
2. Add entry to `external-dependencies.yaml` under `bundled-skills`
3. Re-run `node toolkit/bootstrap.js` for each tool
4. Verify skill appears in `~/.{tool}/skills/<name>/`

**Agent-skill** (external git repo):

1. Create stub `packages/skills/<name>/SKILL.md` with `external-source:` frontmatter
2. Add entry to `external-dependencies.yaml` under `agent-skills` with repo URL
3. Re-run `node toolkit/bootstrap.js` for each tool
4. Run `setup-external.sh` for each tool (or just git clone directly)
5. Verify skill cloned into `~/.{tool}/skills/<name>/`

**External plugin/tool-native** (already installed via tool command):

1. Add entry to appropriate manifest section (`claude-plugins`, `codex-skills`, `copilot-skills`)
2. Record version and install command
3. No bootstrap needed — already installed by tool

#### Step 4: Report

```markdown
# Skill Added: <name>

| Field | Value |
|-------|-------|
| Type | Bundled / Agent-skill / External |
| Source | <path or URL> |
| Manifest | external-dependencies.yaml updated |
| Deployed to | claude, codex, copilot |

Run `/plugins status` to verify.
```

### Examples

```bash
# Skill discovered in $HOME/{{TOOL_DIR}}/skills/ during /plugins list drift check
/plugins add remotion-best-practices
# → Detects in $HOME/{{TOOL_DIR}}/skills/, no .git → copies to packages/skills/ as bundled

# Skill from a public git repo
/plugins add ui-ux-pro-max --from https://github.com/nextlevelbuilder/ui-ux-pro-max-skill
# → Creates stub SKILL.md with external-source, adds to agent-skills manifest

# Skill just installed via claude plugin install
/plugins add code-review --type external
# → Reads installed_plugins.json for version, adds to claude-plugins manifest
```

## /plugins sync

Update `toolkit/external-dependencies.yaml` from current installed state.

### Workflow

1. **Read Current State**
   - Parse `{{HOME_TOOL_DIR}}/plugins/installed_plugins.json`
   - Parse `{{HOME_TOOL_DIR}}/plugins/known_marketplaces.json`
   - Scan `packages/skills/*/SKILL.md`

2. **Compare with Manifest**
   - Identify new installations
   - Identify removed plugins
   - Identify version changes

3. **Generate Diff Report**
   ```markdown
   # Plugin Sync Report

   ## Changes Detected

   | Plugin | Status | Details |
   |--------|--------|---------|
   | new-plugin | NEW | Installed from marketplace X |
   | old-plugin | REMOVED | No longer in installed_plugins.json |
   | beads | UPDATED | 0.48.0 -> 0.49.0 |

   Update manifest? [Y/n]
   ```

4. **Update Manifest**
   - Merge changes into `external-dependencies.yaml`
   - Preserve comments and formatting
   - Update `updated` timestamp

### Implementation

```bash
#!/bin/bash
# Sync Claude plugins to manifest

MANIFEST="toolkit/external-dependencies.yaml"
PLUGINS_JSON="$HOME/{{TOOL_DIR}}/plugins/installed_plugins.json"
MARKETPLACES_JSON="$HOME/{{TOOL_DIR}}/plugins/known_marketplaces.json"

# Extract plugin data
jq -r '.plugins | to_entries[] | {
  name: (.key | split("@")[0]),
  marketplace_id: (.key | split("@")[1]),
  version: .value[0].version,
  installed_at: .value[0].installedAt
}' "$PLUGINS_JSON"

# Match with marketplace repos
jq -r 'to_entries[] | {
  id: .key,
  repo: .value.source.repo
}' "$MARKETPLACES_JSON"
```

## /plugins export

Generate a shell script to reproduce the current setup on a new machine.

### Output

```bash
#!/bin/bash
# Generated by /plugins export
# Reproduces toolkit extension setup
# Generated: 2026-01-24

set -e

echo "Installing Claude Code plugins..."

# Add marketplaces
claude plugin marketplace add steveyegge/beads
claude plugin marketplace add stevengonsalvez/agent-bridge
claude plugin marketplace add anthropics/claude-plugins-official

# Install plugins
claude plugin install beads
claude plugin install debug-bridge
claude plugin install ralph-loop
claude plugin install code-review

echo "Plugin installation complete!"
echo ""
echo "Note: MCP servers must be configured manually in claude_desktop_config.json"
echo "Note: Bundled skills are included in toolkit/packages/skills/"
```

### Usage

```bash
# Generate and save script
/plugins export > setup-plugins.sh

# Make executable and run
chmod +x setup-plugins.sh
./setup-plugins.sh
```

## /plugins status

Check if installed versions match manifest and highlight available updates.

### Output Format

```markdown
# Extension Status Report

## Version Check

| Plugin | Manifest | Installed | Status |
|--------|----------|-----------|--------|
| beads | 0.49.0 | 0.49.0 | OK |
| debug-bridge | 0.2.0 | 0.2.0 | OK |
| ralph-loop | e30768372b41 | e30768372b41 | OK |
| code-review | e30768372b41 | e30768372b41 | OK |

## Not in Manifest
- (none)

## In Manifest but Not Installed
- (none)

All extensions are in sync.
```

### Implementation

```bash
# Compare manifest versions with installed
yq '.claude-plugins[] | "\(.name)=\(.version)"' toolkit/external-dependencies.yaml > /tmp/manifest_versions

jq -r '.plugins | to_entries[] | "\(.key | split("@")[0])=\(.value[0].version)"' \
  $HOME/{{TOOL_DIR}}/plugins/installed_plugins.json > /tmp/installed_versions

diff /tmp/manifest_versions /tmp/installed_versions
```

## Integration with /sync-learnings

The `/sync-learnings` command can optionally include plugin manifest sync:

```markdown
## During /sync-learnings

If plugin changes detected:
1. Show plugin diff alongside file diff
2. Ask: "Also sync plugin manifest? [Y/n]"
3. If yes, run `/plugins sync` logic
```

## File Locations

| File | Purpose |
|------|---------|
| `toolkit/external-dependencies.yaml` | Unified manifest (versioned) |
| `{{HOME_TOOL_DIR}}/plugins/installed_plugins.json` | Claude's plugin state |
| `{{HOME_TOOL_DIR}}/plugins/known_marketplaces.json` | Registered marketplaces |
| `packages/skills/*/SKILL.md` | Bundled skill definitions |

## Best Practices

1. **Run `/plugins sync` after installing new plugins** - Keep manifest up to date
2. **Commit manifest changes** - Track extension history in git
3. **Use `/plugins export` for onboarding** - New team members can reproduce setup
4. **Check `/plugins status` periodically** - Ensure consistency

## Example Session

```
User: /plugins

Claude: # Installed Extensions

## Bundled Skills (5)
| Name | Purpose |
|------|---------|
| webapp-testing | Playwright-based testing |
| crypto-research | Cryptocurrency analysis |
| frontend-design | UI/UX generation |
| tmux-monitor | Session monitoring |
| retro-pdf | Markdown to PDF |

## Claude Plugins (4)
| Name | Version | Marketplace |
|------|---------|-------------|
| beads | 0.49.0 | steveyegge/beads |
| debug-bridge | 0.2.0 | stevengonsalvez/agent-bridge |
| ralph-loop | e30768372b41 | anthropics/claude-plugins-official |
| code-review | e30768372b41 | anthropics/claude-plugins-official |

## MCP Servers (1)
| Name | Purpose |
|------|---------|
| context7 | Documentation lookup |

Total: 10 extensions tracked
Manifest: toolkit/external-dependencies.yaml (last updated: 2026-01-24)
```
