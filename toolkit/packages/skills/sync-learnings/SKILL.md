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

### Tool-Specific Config Files

These are NOT in `packages/` — they live in per-tool directories:

| Source (user-level) | Target (toolkit source) |
|---------------------|------------------------|
| `{{HOME_TOOL_DIR}}/CLAUDE.md` | `toolkit/claude-code-4.5/CLAUDE.md` |
| `{{HOME_TOOL_DIR}}/settings.json` | `toolkit/claude-code-4.5/settings.json` |
| `{{HOME_TOOL_DIR}}/statusline.sh` | `toolkit/claude-code-4.5/statusline.sh` (+x preserved) |

**CLAUDE.md sync requires reverse template interpolation** — when copying TO_REPO,
replace interpolated paths back to template placeholders:
- `{{HOME_TOOL_DIR}}/` or `/.claude/` → `{{HOME_TOOL_DIR}}/`
- `.claude/` (in path context) → `.claude/`

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

### Category 5b: Bundled vs Pulled — presence in the manifest is NOT "external"

CRITICAL (a misclassification cost real cycles 2026-06-05): a skill appearing in
`external-dependencies.yaml` does **not** mean "external, don't sync". The
*section* and *key* discriminate — not mere presence in the file:

| Manifest shape | Meaning | Sync? |
|----------------|---------|-------|
| `bundled-skills:` entry with `path: packages/skills/<name>` | committed in repo, deployed **FROM** repo by bootstrap | **YES — sync both directions** |
| `agent-skills:` entry with `repo:` + `multi-subpath:` | git-cloned into `~/.{tool}/skills` at bootstrap | NO — external; update via update-externals |
| `nanoclaw-skills:` entry | synced from the nanoclaw fork (`container/skills/`) | NO — external; edit in the fork |

- `source:` is **provenance only**. It can sit on a *bundled* (`path:`) skill —
  e.g. `media-processing` has both a `source:` URL AND `path:`; it is bundled
  and DOES sync. `source:` alone never means "don't sync".
- The discriminator is **`path:` (bundled → sync) vs `repo:`/`multi-subpath:`
  (cloned at bootstrap → skip)**.
- Before dropping a *common* skill (one that exists in BOTH `~/.claude` and
  `packages/`) as "external", grep its manifest entry and confirm whether it
  has `path:`. If it does, it is bundled — sync it.

## Reverse Scan Phase (Orphan Detection)

CRITICAL: Before comparing files, enumerate items in {{HOME_TOOL_DIR}}/ that aren't part of this toolkit's internal set, then surface ONLY genuine orphans (candidates for SYNC TO REPO or DEPRECATED).

**Internal set definition (filesystem-derived):**

The internal set is everything this repo owns. It is the union of:
- `toolkit/packages/skills/*/` — bundled skills
- `toolkit/packages/plugins/*/skills/*/` — plugin sub-skills (e.g. reflect ships 5)
- `toolkit/packages/agents/**/*.md` — agents (root + categorized)

A snapshot of this set lives in `toolkit/catalog.yaml`, regenerated by `bash toolkit/bin/generate-catalog.sh`. **Run the generator first** so the snapshot reflects HEAD before scanning.

Anything in `{{HOME_TOOL_DIR}}/skills/` that is NOT in the internal set is EXTERNAL by construction — installed by a plugin (caveman, beads, reflect runtime, etc.), via `npx skills add`, by nanoclaw's native-runner sync, or as a personal CLI wrapper. These should be silently filtered, not classified by hand each run.

```bash
# Refresh the internal manifest first
bash toolkit/bin/generate-catalog.sh

# Build the internal skill set from filesystem (authoritative)
internal_skills=$(
  {
    find toolkit/packages/skills -mindepth 1 -maxdepth 1 -type d -exec basename {} \;
    find toolkit/packages/plugins/*/skills -mindepth 1 -maxdepth 1 -type d -exec basename {} \; 2>/dev/null
  } | sort -u
)

# Reverse scan: find genuine orphans (in $HOME but not internal AND not in external manifest)
echo "=== Orphan Detection: Skills ==="
for skill_dir in $HOME/.claude/skills/*/; do
  [ -d "$skill_dir" ] || continue
  skill_name=$(basename "$skill_dir")

  # Internal? Skip silently — it's accounted for.
  if grep -qx "$skill_name" <<< "$internal_skills"; then continue; fi

  # External (tracked)? Skip silently — install reproducibility lives in external-dependencies.yaml.
  if grep -q "^[[:space:]]*-[[:space:]]*name:[[:space:]]*$skill_name\b\|^[[:space:]]*-[[:space:]]*$skill_name:" \
     toolkit/external-dependencies.yaml 2>/dev/null; then continue; fi

  # Genuine orphan — surface for classification.
  echo "ORPHAN: skills/$skill_name (not internal, not tracked in external-dependencies.yaml)"
done

echo "=== Orphan Detection: Agents ==="
for agent_file in $HOME/.claude/agents/**/*.md; do
  rel_path="${agent_file#$HOME/.claude/}"
  if [ ! -f "toolkit/packages/$rel_path" ]; then
    echo "ORPHAN: $rel_path (in $HOME/.claude only, missing from toolkit/packages/)"
  fi
done
```

Genuine orphans (those that survive the filter) should be classified as:
- **SYNC TO REPO** — Generic, reusable skill/agent → copy to `toolkit/packages/`, then re-run the catalog generator.
- **EXTERNAL (untracked)** — Real external skill that's missing from `external-dependencies.yaml` → add an entry there.
- **PERSONAL** — User-specific tooling that shouldn't be shared → ignore.
- **DEPRECATED** — No longer needed → candidate for removal from `{{HOME_TOOL_DIR}}/`.

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
| mystery-skill | In {{HOME_TOOL_DIR}} only, not in packages | SYNC or EXCLUDE |
```

## Assessment Phase — Tidied Output Contract

The assessment output MUST follow this dense, scannable format. Stevie has
explicitly requested terse output — do NOT regress to multi-bullet per-file
blocks or always-on summary tables.

### Rules

1. **One combined plan table.** A single table with `dir | file | target | note`
   covers both directions (`→repo`, `→home`). No separate `SYNC TO REPO` /
   `SYNC TO HOME` sub-sections.
2. **One line per file.** No multi-bullet per-item blocks
   (no `**Purpose:**`/`**Action:**` lists). The `note` column carries the
   one-word rationale: `new`, `updated`, `newer`, `older`, etc.
3. **Hide empty sections.** If orphans, plugin audit, or don't-sync have zero
   items, OMIT those sections entirely — no "✓ none" lines, no empty tables.
   The plan table itself is always shown (even if empty → emit one line:
   "Nothing to sync.").
4. **Single Y/n gate.** Show the plan, then ask `Proceed? [Y/n]` once.
   Per-section confirmation is forbidden.
5. **Silent execution.** Do NOT echo each `cp` operation. Run the sync, then
   print a 3-line receipt: counts per direction + final commit SHA.
6. **Orphans + plugin audit are INFORMATIONAL, not actionable.** The Y/n gate
   covers ONLY the file-sync rows in the plan table. Orphans (live-only
   skills) and untracked-plugin rows go in a `## Informational` section
   *below* the plan table and are NOT gated by the Y/n. If the user wants
   to act on them, they ask explicitly ("classify the orphans", "add
   untracked plugins to the manifest"). Treating them as part of the plan
   is a regression — Stevie corrected this on 2026-05-19.

### Output template

```markdown
# Sync Assessment

| dir   | file                                | target                           | note    |
|-------|-------------------------------------|----------------------------------|---------|
| →repo | skills/foo/SKILL.md                 | packages/skills/foo/             | new     |
| →repo | agents/engineering/bar.md           | packages/agents/engineering/     | updated |
| →home | packages/skills/baz/SKILL.md        | ~/.claude/skills/baz/            | newer   |

Proceed? [Y/n]

## Informational (NOT part of the gate above)

[Orphans / Plugin audit / Drift — ONLY if non-empty. Each as a small table.
 These are review-later items, not "do this now". The Y/n above does not
 cover them. Surface them so they're visible; don't bundle into the plan.]
```

### Receipt template (after execution)

```
Receipt:
  N files → repo
  M files → home
  commit <sha> on <branch>
```

### Anti-patterns (do NOT emit)

- ❌ Top-level summary table that just counts what's below it (duplicate signal).
- ❌ `### 1. file.md (NEW)` with `**Purpose:**` / `**Target:**` / `**Action:**`
  multi-bullet blocks.
- ❌ "✓ none" placeholder lines for empty sections.
- ❌ Per-section Y/n prompts.
- ❌ Echoing every `\cp` line during execution.

## Execution

### Reverse Template Interpolation (TO_REPO direction — CRITICAL)

**When copying FROM user-level TO packages/toolkit, REVERSE the interpolation.**
User-level files have resolved paths (`{{HOME_TOOL_DIR}}/`, `/.claude/`, `.claude/`).
These MUST be converted back to template placeholders before writing to the repo.

> **CRITICAL**: The replacement side of every reverse-interpolation regex must
> emit the literal placeholder text `{{HOME_TOOL_DIR}}` / `{{TOOL_DIR}}`. In this
> SKILL.md source we write those placeholders ESCAPED (`\{\{HOME_TOOL_DIR\}\}`)
> so that when the skill itself is deployed via the TO_HOME interpolation
> below, the deploy-time perl substitution does NOT eat the example. After
> deploy, users see `\{\{HOME_TOOL_DIR\}\}` — which is a valid perl replacement
> producing literal `{{HOME_TOOL_DIR}}`. If you ever see `s|...|~/.claude|g`
> in this section, the example has been corrupted by a buggy reverse-interp
> pass — restore it from git history.

```bash
# Reverse interpolation: user-level → packages/toolkit source
# For .md files (documentation references use ~/ form):
perl -pe 's|~/\.claude|\{\{HOME_TOOL_DIR\}\}|g' \
  "$HOME/.claude/CLAUDE.md" > toolkit/claude-code-4.5/CLAUDE.md

# For .sh/.py files (bash code uses $HOME form):
perl -pe 's|\$HOME/\.claude\b|\$HOME/\{\{TOOL_DIR\}\}|g; s|~/\.claude\b|\{\{HOME_TOOL_DIR\}\}|g' \
  "$HOME/.claude/skills/some-skill/scripts/script.sh" \
  > toolkit/packages/skills/some-skill/scripts/script.sh

# For mixed files (both doc text AND bash code blocks — like CLAUDE.md):
# Order matters: $HOME first (most specific), then ~/, then bare .claude/
perl -pe '
  s|\$HOME/\.claude\b|\$HOME/\{\{TOOL_DIR\}\}|g;
  s|~/\.claude\b|\{\{HOME_TOOL_DIR\}\}|g;
  s|`\.claude/|`\{\{TOOL_DIR\}\}/|g;
  s|"\.claude/|"\{\{TOOL_DIR\}\}/|g;
' SOURCE > DEST
```

**Always verify** after reverse interpolation — these greps should return ZERO
hits in the toolkit-side file (except inside intentional examples / quoted
docs, which is why we use `\b` word-boundary in the regex above):

```bash
grep -nE '~/\.claude\b|\$HOME/\.claude\b' toolkit/claude-code-4.5/CLAUDE.md
grep -nE '~/\.claude\b|\$HOME/\.claude\b' toolkit/packages/skills/<skill>/SKILL.md
```

**Bulk fixup** (when a sync regression slipped in and many files lost their
placeholders — e.g. PR #70):

```bash
# Re-templatize a list of already-synced skill files.
# In-place edit; review with `git diff` before committing.
perl -i -pe '
  s|\$HOME/\.claude\b|\$HOME/\{\{TOOL_DIR\}\}|g;
  s|~/\.claude\b|\{\{HOME_TOOL_DIR\}\}|g;
' toolkit/packages/skills/*/SKILL.md toolkit/packages/skills/*/scripts/*.sh
```

**Do NOT reverse-interpolate**:
- The string `.claude` when it's a directory name in a path context (e.g., `Check .claude/session/`) — this should become `.claude/session/`
- But `.claude` as part of a domain name or unrelated word — leave alone

### Template Interpolation (TO_HOME direction — CRITICAL)

**Packages files use `{{HOME_TOOL_DIR}}` as a cross-tool placeholder.** When syncing
TO `{{HOME_TOOL_DIR}}` (or any tool's home dir), these MUST be substituted before the file is
written — never leave them as literal strings.

| Template | Claude Code | Codex | Copilot |
|----------|-------------|-------|---------|
| `{{HOME_TOOL_DIR}}` | `{{HOME_TOOL_DIR}}/` | `~/.codex/` | `~/.copilot/` |
| `.claude` | `.claude` | `.codex` | `.copilot` |

Use `perl` for safe substitution (avoids shell expansion issues with `~`):

```bash
# After copying FROM packages TO /.claude, interpolate templates:
perl -pi -e 's/\{\{HOME_TOOL_DIR\}\}/$ENV{HOME}\/.claude/g; s/\{\{TOOL_DIR\}\}/.claude/g' /.claude/path/to/SKILL.md

# Or with explicit paths:
perl -pe 's/\{\{HOME_TOOL_DIR\}\}/\/Users\/stevengonsalvez\/.claude/g; s/\{\{TOOL_DIR\}\}/.claude/g' \
  /tmp/SKILL.md > /.claude/path/to/SKILL.md
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
# CANONICALIZE BOTH SIDES BEFORE ANY DIFF. A bare diff shows `~/.claude` vs
# `/Users/x/.claude` vs an unsubstituted template placeholder as false
# divergences — a polluted first pass cost real cycles 2026-06-05. This helper
# collapses every home-path form (and a leaked, unsubstituted placeholder) to
# one token, on BOTH the repo and home side, so only TRUE content diffs remain.
canon() { perl -pe '
  s|\{\{HOME_TOOL_DIR\}\}|@H@|g; s|\$HOME/\{\{TOOL_DIR\}\}|@H@|g;
  s|/Users/[^/]+/\.claude|@H@|g; s|~/\.claude|@H@|g; s|\$HOME/\.claude|@H@|g;
  s|\{\{TOOL_DIR\}\}|.claude|g;' "$1"; }
# Usage:  diff <(canon REPO_FILE) <(canon HOME_FILE)   # → only real diffs
# DIRECTION (mtime is unreliable — bulk deploys cluster-touch mtimes): decide by
# CONTENT ASYMMETRY, not timestamps. Count lines unique to each side:
#   only_repo=$(diff <(canon REPO) <(canon HOME) | grep -c '^<')
#   only_home=$(diff <(canon REPO) <(canon HOME) | grep -c '^>')
# only_home > only_repo → home richer → →repo · only_repo > only_home → →home.
# If both sides have unique lines, it is a real bidirectional divergence →
# inspect/merge, never blind-overwrite (see Safety Checks).

# CLAUDE.md diff (tool-specific config file) — uses the canon() helper above
diff <(canon toolkit/claude-code-4.5/CLAUDE.md) <(canon /.claude/CLAUDE.md)

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
- **Decide direction by CONTENT ASYMMETRY, never mtime.** Bulk deploys
  cluster-touch mtimes, so "newer to older" is wrong. Canonicalize both sides
  (see `canon()` in Quick Diff Commands), then count lines unique to each side:
  more-unique-on-home → `→repo`; more-unique-on-repo → `→home`; unique on BOTH
  → real bidirectional divergence (inspect/merge, do not blind-copy).
- **Bundled vs pulled before any drop.** A skill in `external-dependencies.yaml`
  is NOT automatically external — `bundled-skills` + `path:` = committed in repo
  and DOES sync; only `agent-skills`/`nanoclaw-skills` (git-cloned at bootstrap)
  are external. `source:` is provenance, not an exclusion marker. See Category 5b.
- **Compare against `origin/main`, not just local HEAD, when local is behind.**
  `git fetch` first; a TO_REPO target may already have moved on origin (re-check
  before proposing, to avoid stale or clobbering syncs).
- **Tool-specific files (statusline.sh, settings.json, CLAUDE.md) can diverge
  BOTH ways.** Diff against origin and, if each side has unique blocks, do a
  surgical per-block merge (port the new block by content anchor) — never
  overwrite, or you silently drop the other side's improvements.
- **Deleting a bundled skill = four steps, not one:** (1) remove the skill dir,
  (2) remove its `bundled-skills` entry from `external-dependencies.yaml`,
  (3) remove any skill-specific handling in `bootstrap.js` (e.g. a compile
  step), (4) regenerate `catalog.yaml`. Then grep the toolkit for dangling refs.
- **Skip binaries** and non-text files (never run `perl -i` interpolation on a
  compiled binary — copy it raw)
- **Validate** markdown frontmatter before copying agent files
- **Route commands** to correct package directory
- **Exclude** files matching exclusion categories

## Example Session (tidied format)

```
User: /sync-learnings

Claude:
# Sync Assessment

| dir   | file                                       | target                              | note    |
|-------|--------------------------------------------|-------------------------------------|---------|
| →repo | agents/engineering/new-validator.md        | packages/agents/engineering/        | new     |
| →repo | commands/custom-workflow.md                | packages/utilities/commands/        | new     |
| →home | packages/utilities/commands/sync-learnings.md | {{HOME_TOOL_DIR}}/commands/      | newer   |

Proceed? [Y/n]

User: Y

Claude:
Receipt:
  2 files → repo
  1 file  → home
  commit a1b2c3d on main
```

Notice what is NOT in the output: no top-level summary table, no ORPHANS /
PLUGIN AUDIT / DON'T SYNC sections (all empty this run), no per-file
multi-bullet blocks, no per-`\cp` echo during execution.

If orphans or untracked plugins DO exist, append them as small tables UNDER
the plan table — still one row per item, no nested bullets.

```
# Sync Assessment

| dir   | file                       | target                          | note    |
|-------|----------------------------|---------------------------------|---------|
| →repo | skills/new-helper/SKILL.md | packages/skills/new-helper/     | new     |

## Orphans (need classification)

| item                      | suggestion              |
|---------------------------|-------------------------|
| skills/mystery-skill/     | PERSONAL or DEPRECATED? |

Proceed? [Y/n]
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
