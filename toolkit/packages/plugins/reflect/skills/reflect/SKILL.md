---
name: reflect
description: |
  Full conversation scan for self-improvement. Detects behavioral corrections and
  knowledge signals, classifies them, proposes agent updates and knowledge notes
  with entity sidecars for GraphRAG indexing. Correct once, never again.
version: "3.1.0"
user-invocable: true
triggers:
  - reflect
  - self-reflect
  - review session
  - what did I learn
  - extract learnings
  - analyze corrections
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
metadata:
  stateDirs: ["~/.reflect"]
---

# Reflect - Agent Self-Improvement Skill

Transform your AI assistant into a continuously improving partner. Every correction
becomes a permanent improvement that persists across all future sessions.

## Backwards Compatibility

This skill handles the base `/reflect` command. If invoked with sub-command flags,
redirect the user to the appropriate sub-skill:

| Flag | Redirect |
|------|----------|
| `--consolidate` | "Use `/reflect:consolidate` instead." |
| `--ingest-memories` | "Use `/reflect:ingest` instead." |
| `--status` | "Use `/reflect-status` instead." |
| `--review` | "Use `/reflect-status` instead (status includes review)." |
| `--behavioral` | Proceed normally -- behavioral-only scan (skip knowledge extraction). |
| `--knowledge` | Proceed normally -- knowledge-only scan (skip behavioral). |

For `reflect on` and `reflect off`, handle inline (toggle auto-reflect state).
For `reflect [agent-name]`, run behavioral scan scoped to that agent file only.

## Quick Reference

| Command | Action |
|---------|--------|
| `/reflect` | Full conversation scan: behavioral + knowledge extraction |
| `/reflect --behavioral` | Behavioral corrections only → agent file diffs |
| `/reflect --knowledge` | Knowledge capture only → learning notes + sidecars |
| `/reflect [agent]` | Focus behavioral scan on a specific agent file |
| `/reflect on` | Enable auto-reflection (PreCompact hook) |
| `/reflect off` | Disable auto-reflection |
| `/reflect:consolidate` | Merge orphaned worktree memories → .agents/MEMORY.md |
| `/reflect:ingest` | Global indexer: sweep ALL sources → GraphRAG + QMD |
| `/reflect-status` | Dashboard: metrics, pending reviews, coverage, health |

## When to Use

- After completing complex tasks
- When user explicitly corrects behavior ("never do X", "always Y")
- At session boundaries or before context compaction
- When successful patterns are worth preserving
- When a solved problem should be captured as knowledge

## Workflow

### Step 1: Scan Conversation for Signals

Analyze the conversation for **two types** of signals:

1. **Behavioral signals** -- corrections, preferences, rules about how to act
2. **Knowledge signals** -- solved problems, root causes, discovered patterns, decisions

**Signal Confidence Levels:**

| Confidence | Behavioral Triggers | Knowledge Triggers |
|------------|--------------------|--------------------|
| **HIGH** | "never", "always", "wrong", "stop", "the rule is" | "root cause was", "fixed by", "the solution was", "chose X over Y" |
| **MEDIUM** | "perfect", "exactly", "that's right", accepted output | "spent 2 hours", "the docs say X but", "misleading error" |
| **LOW** | Patterns that worked but not explicitly validated | "seems to work", "so far so good", implicit success |

See `references/signal_patterns.md` for full detection rules.

### Step 2: Classify & Match to Targets (MANDATORY)

**CRITICAL**: Every detected signal MUST be routed to at least one of:
(a) an existing agent/CLAUDE.md file edit, (b) a new skill proposal, or
(c) a knowledge note. Knowledge notes are the **fallback**, not the default.
If a signal has clear behavioral implications, you MUST propose an edit to an
existing agent target — do not skip straight to knowledge-note output.

**Behavioral signals** map to agent files OR existing skills (preferred when domain matches):

| Category | Target Files |
|----------|--------------|
| **Existing skill match** | **`.claude/skills/<name>/skill.md` or `~/.claude/skills/<name>/SKILL.md` (EDIT in-place — see Step 2.5)** |
| Code Style | `code-reviewer`, `backend-developer`, `frontend-developer` |
| Architecture | `solution-architect`, `api-architect`, `architecture-reviewer` |
| Process | Agent config file (`CLAUDE.md`), orchestrator agents |
| Domain | Domain-specific agents, agent config file |
| Tools | Agent config file, relevant specialists |
| Security | `security-agent`, `code-reviewer` |
| New Skill | Create new skill file (only when no existing skill matches and quality gates pass — see Step 3) |

See `references/agent_mappings.md` for detailed mapping rules.
See `references/classification_rules.md` for behavioral vs knowledge routing.

### Step 2.5: Match Against Existing Skills (BEFORE falling through to memory)

**Why this step exists**: Operational gotchas (e.g. "publish AD_ID declaration must
be Yes when Firebase is a dep") frequently belong in an existing skill (`publish`,
`shot-testing`, `stripe-webhook-debug`) rather than `.agents/MEMORY.md`. Without
this step, signals default to memory files and skills go stale.

**Build skill index** (cached for 24h, refreshed on `/reflect-status`):

```bash
# Walk skill directories and extract searchable metadata
for skill_md in ~/.claude/skills/*/SKILL.md ~/.claude/skills/*/skill.md \
                .claude/skills/*/skill.md .claude/skills/*/SKILL.md; do
  [ -f "$skill_md" ] || continue
  # Extract: name (from frontmatter or dirname), description (frontmatter),
  # triggers (frontmatter.triggers), first H1 heading, first paragraph of body
done
```

Output one record per skill:
```yaml
name: publish
path: .claude/skills/publish/skill.md
keywords:
  - publish, testflight, play store, ios, android
  - app store connect, fastlane, xcode, capacitor
  - cap sync, version bump, ad_id, manifest
description: "Publish SHOT mobile apps to TestFlight and Play Store..."
```

**Score signals against each skill**:

For each detected signal, compute keyword overlap with each skill's keyword set
(token Jaccard or embedding cosine if QMD available).

**Routing decision**:

| Match score | Action |
|-------------|--------|
| **≥ 0.6** (strong overlap) | **Propose EDIT to that skill** (additive section). Do NOT route to agent or memory file. |
| 0.4–0.6 (candidate) | Present skill edit AND agent edit as alternatives — let user pick. |
| < 0.4 (no match) | Fall through to existing routing (agent file → memory file). |

**Important reuse rule**: A short bullet rule (≤3 lines) STILL routes to a matching
skill. The "≥10 min debugging / Problem-Solution writeup" criterion in Step 3
applies only to NEW skill *creation*, not to edits of existing skills. Skills
naturally accumulate operational bullets in their gotchas / known issues sections.

**What this prevents**: 5 publish-domain learnings landing in `.agents/MEMORY.md`
when `.claude/skills/publish/skill.md` is the natural canonical home for them.

**Knowledge signals** become learning notes:

| Category | Indicators |
|----------|------------|
| `build-errors` | Compile errors, CI failures, bundling |
| `performance-issues` | Slowdowns, memory leaks, optimization |
| `security-fixes` | Vulnerabilities, auth issues, secrets |
| `testing-patterns` | Test strategies, flaky tests |
| `debugging-sessions` | Complex investigations |
| `architecture-decisions` | Design choices, patterns |
| `api-integrations` | Third-party APIs, SDKs |
| `dependency-issues` | Package conflicts, upgrades |
| `deployment-fixes` | Production incidents |
| `database-migrations` | Schema changes, data fixes |
| `ui-patterns` | Frontend patterns, CSS |
| `tooling-setup` | Dev environment, configs |

**Knowledge note references** (absorbed from compound-docs):
- `references/docs-solutions-template.md` -- template for project-local
  `docs/solutions/{category}/{filename}.md` notes with YAML frontmatter
- `references/critical-patterns.md` -- check for critical patterns that
  must always be flagged (auth, data integrity, security)
- `references/schema.yaml` -- JSON-schema describing valid knowledge notes
- `assets/learning_template.md` -- canonical template for new learnings

### Step 3: Check for NEW Skill Creation

This step decides whether to **CREATE a brand-new skill**. Note: edits to existing
skills are handled in Step 2.5 above; this step only fires when no existing skill
matched and the signal is substantial enough to warrant a new top-level skill.

**Skill-Worthy Criteria (for NEW skill creation):**
- Non-obvious debugging (>10 min investigation)
- Misleading error (root cause different from message)
- Workaround discovered through experimentation
- Configuration insight (differs from documented)
- Reusable pattern (helps in similar situations)

**Quality Gates (must pass all):**
- [ ] Reusable: Will help with future tasks
- [ ] Non-trivial: Requires discovery, not just docs
- [ ] Specific: Can describe exact trigger conditions
- [ ] Verified: Solution actually worked
- [ ] No duplication: Doesn't exist already

See `references/skill_template.md` for skill creation guidelines.

### Step 4: Generate Proposals

Present findings using the reflection template at `assets/reflection_template.md`.
For each knowledge note, use `assets/learning_template.md` to structure the
individual `.md` file (fields: `id`, `scope`, `confidence`, `learning_type`,
`source_episodes`, `superseded_by`, `provenance`, plus Problem/Solution/
Anti-Pattern/Context sections).

The output must include:

1. **Signals table** -- all detected signals with confidence and category
2. **Proposed skill edits** (NEW in v3.1) -- when Step 2.5 found a matching skill
   (score ≥ 0.6), show the additive section diff for that skill file
3. **Proposed agent updates** -- diffs for each behavioral change that fell
   through to agents
4. **Proposed knowledge notes** -- for each knowledge signal, show:
   - YAML frontmatter preview
   - Entity sidecar preview (entities + relationships)
   - Target path: `docs/solutions/{category}/{filename}.md`
5. **Proposed new skills** -- with quality gate checklist
6. **Conflict check** -- warn if new rules contradict existing
7. **Review prompt** -- allow selective approval

### Step 5: MANDATORY Entity Sidecar Generation

**CRITICAL**: When creating ANY knowledge note, you MUST also generate the
`.entities.yaml` sidecar file alongside the `.md` file. This is the single
most important step for knowledge searchability.

**Entity sidecar format** (see `references/knowledge_format.md` for details):

```yaml
document_id: lrn-{slug}-{hash6}
extracted_at: "{ISO timestamp}"
entities:
  - name: "{entity name}"
    type: technology | error | pattern | function | concept | tool
    description: "{brief description}"
relationships:
  - source: "{entity A}"
    target: "{entity B}"
    type: caused_by | solves | requires | relates_to
    description: "{how they relate}"
    strength: 1-10
```

**Rules:**
- Extract 3-8 entities per learning (focused, not exhaustive)
- Always include at least one `solves` relationship for bug-fix type
- Strength: 9-10 direct/causal, 5-7 moderate, 1-4 weak
- Entity names normalized to lowercase canonical form
- Use the most specific entity type available

### Step 6: Apply with User Approval

**On `Y` (approve):**
1. Apply each behavioral change using Edit tool
2. Write knowledge notes to `docs/solutions/{category}/`
3. Write entity sidecar alongside each knowledge note
4. Index globally (validates sidecar first to catch schema errors early):
   ```bash
   SIDECAR="docs/solutions/{category}/{filename}.entities.yaml"
   DOC="docs/solutions/{category}/{filename}.md"
   VALIDATE="{{HOME_TOOL_DIR}}/skills/reflect/scripts/validate_sidecar.py"

   if command -v reflect >/dev/null 2>&1; then
       # Validate before ingest — malformed sidecars fail loudly here, not
       # silently at GraphRAG time
       uv run "$VALIDATE" --strict "$SIDECAR" || {
           echo "ERROR: sidecar validation failed for $SIDECAR" >&2
           exit 1
       }
       # --force skips the interactive y/N prompt; content-hash doc_id makes
       # the call idempotent so re-runs no-op cleanly.
       reflect add "$DOC" --entities "$SIDECAR" --force
   fi
   ```
   Capture → index is now closed: every accepted learning flows into GraphRAG
   + QMD immediately, so the next session's SessionStart recall will surface
   it via the retrieval hook.
5. Create episode note (auto, no approval needed)
6. Update metrics:
   ```bash
   python {{HOME_TOOL_DIR}}/skills/reflect/scripts/metrics_updater.py \
       --accepted N --rejected M --confidence high:X,medium:Y,low:Z \
       --agents "agent1,agent2" --skills S
   ```
7. Update state:
   ```bash
   python {{HOME_TOOL_DIR}}/skills/reflect/scripts/state_manager.py status
   ```
8. Commit with descriptive message

**On `N` (reject):**
1. Discard proposed changes
2. Log rejection for analysis

**On `modify`:**
1. Present each change individually
2. Allow editing before applying

**On selective (e.g., `1,3` or `k1,k2` or `s1` or `e1,e2`):**
1. Apply only specified changes
2. `1,3` = agent changes 1 and 3
3. `k1,k2` = knowledge notes 1 and 2
4. `s1` = skill 1 (new skill creation)
5. `e1,e2` = existing skill edits 1 and 2 (Step 2.5 matches)
6. `all-knowledge` = all knowledge notes, skip others
7. `all-skills` = all new skill creations, skip agent updates
8. `all-skill-edits` = all existing skill edits, skip new skill creations

### Step 7: Episode Note (Auto)

After applying changes, automatically create an episode note.
Episode notes are raw session snapshots for provenance -- they do NOT require approval.

Use template at `assets/episode_template.md`.

## Toggle Auto-Reflect

```
reflect on   -> python {{HOME_TOOL_DIR}}/skills/reflect/scripts/state_manager.py on
reflect off  -> python {{HOME_TOOL_DIR}}/skills/reflect/scripts/state_manager.py off
```

## State Management

State is stored in `~/.reflect/` (configurable via `REFLECT_STATE_DIR`):

```yaml
# reflect-state.yaml
auto_reflect: false
last_reflection: "2026-01-26T10:30:00Z"
pending_low_confidence: []
```

### Metrics Tracking

```yaml
# reflect-metrics.yaml
total_sessions_analyzed: 42
total_signals_detected: 156
total_changes_accepted: 89
acceptance_rate: 78%
confidence_breakdown:
  high: 45
  medium: 32
  low: 12
most_updated_agents:
  code-reviewer: 23
  backend-developer: 18
skills_created: 5
```

## Safety Guardrails

### Human-in-the-Loop
- NEVER apply changes without explicit user approval
- Always show full diff before applying
- Allow selective application

### Incremental Updates
- ONLY add to existing sections
- NEVER delete or rewrite existing rules
- Preserve original structure

### Conflict Detection
- Check if proposed rule contradicts existing
- Warn user if conflict detected
- Suggest resolution strategy

## Output Locations

**Project-level (versioned with repo):**
- `docs/solutions/{category}/{name}.md` - Knowledge notes
- `docs/solutions/{category}/{name}.entities.yaml` - Entity sidecars
- `.claude/skills/{name}/SKILL.md` - New skills

**Global (user-level):**
- `~/.reflect/learnings.yaml` - Learning log
- `~/.reflect/reflect-metrics.yaml` - Aggregate metrics
- Via `learnings add` CLI - GraphRAG indexed knowledge

## Examples

### Example 1: Behavioral Correction

**User says**: "Never use `var` in TypeScript, always use `const` or `let`"

**Signal detected**:
- Type: Behavioral
- Confidence: HIGH (explicit "never" + "always")
- Category: Code Style
- Target: `frontend-developer.md`

**Proposed change**:
```diff
## Style Guidelines
+ * Use `const` or `let` instead of `var` in TypeScript
```

### Example 2: Knowledge Signal - Solved Bug

**Context**: Spent 30 minutes debugging a React hydration mismatch

**Signal detected**:
- Type: Knowledge
- Confidence: HIGH (non-trivial debugging with confirmed fix)
- Category: debugging-sessions
- Learning Type: bug-fix

**Proposed knowledge note**: `docs/solutions/debugging-sessions/react-hydration-mismatch.md`

**Proposed entity sidecar**: `docs/solutions/debugging-sessions/react-hydration-mismatch.entities.yaml`
```yaml
entities:
  - name: "react"
    type: technology
    description: "UI library for building component-based interfaces"
  - name: "hydration mismatch"
    type: error
    description: "Server-rendered HTML doesn't match client render"
  - name: "suppressHydrationWarning"
    type: function
    description: "React prop to suppress hydration mismatch warnings"
relationships:
  - source: "dynamic content in SSR"
    target: "hydration mismatch"
    type: caused_by
    description: "Server and client render different content for dynamic values"
    strength: 9
  - source: "useEffect mounted check"
    target: "hydration mismatch"
    type: solves
    description: "Defer dynamic content to client-only rendering"
    strength: 10
```

### Example 3: Both Types in One Session

**Session**: User corrects TypeScript style AND solves a tricky async bug

**Output**:
1. Agent update proposal (behavioral) for `frontend-developer.md`
2. Knowledge note + entity sidecar (knowledge) for `docs/solutions/build-errors/`
3. Episode note linking both learnings

### Example 4: Existing Skill Match (NEW in v3.1)

**Context**: While shipping a Play Store release, user discovered that
"Advertising ID" declaration must be set to "Yes + Analytics" when Firebase
is a dep, and that `npx cap sync` updates `build.gradle` versionCode (which
must be committed alongside `npm version` bumps).

**Step 2.5 skill index** finds:
```yaml
name: publish
path: .claude/skills/publish/skill.md
keywords: [publish, play store, testflight, cap sync, version bump,
           ad_id, manifest, fastlane, app store, capacitor]
```

**Score**: signal keywords {ad_id, declaration, cap sync, versionCode, capacitor}
overlap heavily with the `publish` skill keyword set → **score 0.78** ≥ 0.6.

**Proposed skill edit** (NOT a memory file edit, NOT a new skill):

```diff
# .claude/skills/publish/skill.md
## Android Build Gotchas
+ * `npx cap sync` updates `android/app/build.gradle` versionCode/versionName.
+   Commit alongside `npm version` bumps — otherwise build.gradle drifts.
+ * Capacitor plugin install ≠ registration: cap sync writes 3 files
+   (capacitor.build.gradle, settings.gradle, plugins.json) — all required.

## Play Store Submission Quirks
+ * Advertising ID declaration MUST be "Yes + Analytics use case" when
+   Firebase Analytics / RevenueCat / Crashlytics are deps. Mismatch →
+   upload rejected.
```

This routing prevents 5 publish-domain learnings from accumulating in
`.agents/MEMORY.md` when the canonical skill home already exists.

## Troubleshooting

**No signals detected:**
- Session may not have had corrections or discoveries
- Check if using natural language corrections

**Conflict warning:**
- Review the existing rule cited
- Decide if new rule should override
- Can modify before applying

**Agent file not found:**
- Check agent name spelling
- May need to create agent file first

**Sidecar not generated:**
- This is a critical bug -- sidecars MUST be generated for every knowledge note
- Re-run reflect and ensure Step 5 is followed
