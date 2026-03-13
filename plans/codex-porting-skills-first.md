# Plan: Port Toolkit to Codex CLI (Skills-First Architecture)

**Date**: 2026-02-19
**Repository**: agents-in-a-box
**Branch**: main
**Status**: Planned

## Context

The `toolkit/` directory contains our full Claude Code configuration ecosystem: 28 commands, 14 skills, 37+ agents, 15 hooks, 2 workflow configs, and 2 knowledge packages. Currently only 2 files have been ported to `toolkit/codex/` (`AGENTS.md` + `config.toml`), while the deployed `~/.codex/` has 38 commands and 6 skills manually copied.

### Why Skills-Only?

**OpenAI has explicitly deprecated custom prompts (commands) in Codex CLI:**
> "Custom prompts are deprecated. Use skills for reusable instructions that Codex can invoke explicitly or implicitly." - [OpenAI Codex Docs](https://developers.openai.com/codex/custom-prompts/)

**Both Anthropic and OpenAI converged on the same open skills standard** (Dec 2025). Skills are the cross-platform portable unit. Converting everything to skills means:
- Single format works on **both** Claude Code and Codex CLI
- Skills support bundled resources (scripts/, references/, assets/) that commands can't
- Skills support progressive disclosure (lower token cost - metadata loaded first, full instructions on demand)
- Skills support automatic invocation (Claude/Codex can auto-select based on context)
- A simple command is just a skill with only a `SKILL.md` and no extras

### Architecture Mapping

| Claude Concept | Codex Equivalent | Port Strategy |
|---------------|-----------------|---------------|
| `CLAUDE.md` | `AGENTS.md` | Already ported |
| `settings.json` | `config.toml` | Already ported |
| Commands (`~/.claude/commands/`) | **Skills** (`~/.codex/skills/`) | Convert all to skills |
| Skills (`~/.claude/skills/`) | Skills (`~/.codex/skills/`) | Direct port |
| Agents (Task tool + YAML) | **No equivalent** | Document gap; convert knowledge to skills |
| Hooks (8 lifecycle events) | **No equivalent** | Document gap |
| Multi-agent workflows | External Agents SDK | Document gap |
| Templates | Bundled in skills as `assets/` | Embed in relevant skills |

---

## Phase 1: Directory Structure & Deploy Script

### 1.1 Create `toolkit/codex/` layout

```
toolkit/codex/
├── AGENTS.md                        # Already exists
├── config.toml                      # Already exists
├── skills/                          # ALL portable capabilities go here
│   ├── brainstorm/SKILL.md
│   ├── commit/SKILL.md
│   ├── research/
│   │   ├── SKILL.md
│   │   └── scripts/search-learnings.sh
│   ├── crypto-research/
│   │   ├── SKILL.md
│   │   ├── agent-prompts/
│   │   └── scripts/
│   ├── ... (all other skills)
│   └── workflow/
│       ├── SKILL.md
│       └── references/single-agent-workflow.md
├── PORTING_GAPS.md                  # Agents, hooks, multi-agent gaps
└── scripts/
    └── deploy.sh                    # Sync to ~/.codex/
```

### 1.2 Deploy script (`toolkit/codex/scripts/deploy.sh`)

- Syncs `AGENTS.md` -> `~/.codex/AGENTS.md`
- Syncs `config.toml` -> `~/.codex/config.toml`
- Syncs `skills/` -> `~/.codex/skills/`
- Removes stale `~/.codex/commands/` (deprecated)
- Preserves: `auth.json`, `sessions/`, `history.jsonl`, `sqlite/`, `vendor_imports/`
- Path transform: `~/.claude/` -> `~/.codex/`, `CLAUDE.md` -> `AGENTS.md`

---

## Phase 2: Convert Commands to Skills (28 -> ~25 skills)

Each command becomes a skill directory with a `SKILL.md`. Simple commands get just a `SKILL.md`; complex ones get supporting files.

### 2.1 Simple conversions (command.md -> skill-name/SKILL.md) - 18 skills

These are straightforward wraps - the command markdown becomes the SKILL.md content with added frontmatter:

| Command | Skill Name | Notes |
|---------|-----------|-------|
| `brainstorm.md` | `brainstorm/` | Add `description`, `user-invocable: true` frontmatter |
| `commit.md` | `commit/` | Same |
| `critique.md` | `critique/` | Same |
| `expose.md` | `expose/` | Same |
| `handover.md` | `handover/` | Move `handover-template.md` into `assets/` |
| `health-check.md` | `health-check/` | Same |
| `session-metrics.md` | `session-metrics/` | Same |
| `session-summary.md` | `session-summary/` | Same |
| `prime.md` | `prime/` | Same |
| `plan-gh.md` | `plan-gh/` | Same |
| `plan-tdd.md` | `plan-tdd/` | Same |
| `make-github-issues.md` | `make-github-issues/` | Same |
| `gh-issue.md` | `gh-issue/` | Same |
| `do-issues.md` | `do-issues/` | Same |
| `find-missing-tests.md` | `find-missing-tests/` | Same |
| `start-local.md` | `start-local/` | Same |
| `start-ios.md` | `start-ios/` | Same |
| `start-android.md` | `start-android/` | Same |

**Conversion pattern:**
```markdown
---
name: brainstorm
description: Generate ideas and alternatives for a given topic
user-invocable: true
---

[existing command content, with path substitutions applied]
```

### 2.2 Rich conversions (command + supporting files -> skill directory) - 7 skills

| Command | Skill Name | Extra Files |
|---------|-----------|-------------|
| `research.md` | `research/` | `scripts/search-learnings.sh` from utils |
| `reflect.md` | Merge into existing `reflect/` skill | Already a skill, just update paths |
| `plugins.md` | `plugins/` | Path swap `~/.claude/` -> `~/.codex/` |
| `session-info.md` | `session-info/` | Path swap |
| `sync-learnings.md` | `sync-learnings/` | `scripts/` for sync logic, path swap |
| `research-cache.md` | `research-cache/` | Path swap |
| `tui-style-guide.md` | `tui-style-guide/` | Move color palette into `references/` |

### 2.3 Crypto commands -> merge into existing `crypto-research` skill

- `crypto_research.md`, `crypto_research_haiku.md`, `cook_crypto_research_only.md`
- These are thin orchestration layers that invoke the crypto-research skill
- Merge as modes/references within the existing `crypto-research/` skill
- Add `references/modes.md` describing haiku vs full vs cook-only modes

### 2.4 Skip / not applicable

- Multi-agent commands (`m-implement.md`, `m-monitor.md`, `m-plan.md`, `m-workflow.md`) - document in gaps
- Swarm commands (`swarm-create`, `swarm-status`, etc.) - Agent Teams specific, document in gaps

---

## Phase 3: Port Existing Skills (14 -> 11 portable)

### 3.1 Direct port (6 skills, already confirmed working)

Copy from `toolkit/packages/skills/` to `toolkit/codex/skills/`:
- `crypto-research/` (with crypto command modes merged in from Phase 2.3)
- `frontend-design/`
- `remotion-best-practices/`
- `retro-pdf/`
- `tmux-monitor/`
- `webapp-testing/`

### 3.2 Port with path adaptation (5 skills)

- `compound-docs/` - swap `~/.claude/` -> `~/.codex/` paths
- `oracle/` - direct port (uses external CLI)
- `reflect/` - swap paths, merge `/reflect` command content
- `skill-creator/` - direct port (meta skill, structure is identical)
- `interview/` - direct port

### 3.3 Defer (3 skills - Claude Agent Teams specific)

- `swarm-orchestration/` - requires Task tool + Agent Teams
- `swarm-agent-troubleshooting/` - same
- `debug-bridge/` - needs Codex sandbox testing

Document in `PORTING_GAPS.md`.

---

## Phase 4: Convert Workflow Commands to Skills

### 4.1 Single-agent workflow -> `workflow/` skill

- `toolkit/packages/workflows/single-agent/` has command files
- Convert to `workflow/` skill with `references/` containing the workflow steps
- This consolidates `/workflow`, `/implement`, `/validate`, `/plan` into one skill with modes

### 4.2 Multi-agent workflow -> document gap

- Cannot port without Agent Teams / Agents SDK
- Document in `PORTING_GAPS.md`

---

## Phase 5: Embed Templates in Skills

Instead of a separate `templates/` directory, embed templates where they're used:

| Template | Embed In |
|----------|----------|
| `codereview-checklist-template.md` | `code-review/assets/checklist.md` (new skill) or `commit/assets/` |
| `handover-template.md` | `handover/assets/template.md` |

This follows the skills best practice of bundling everything a skill needs inside its directory.

---

## Phase 6: Port Knowledge Packages

### 6.1 `docs-solutions-template/` -> `compound-docs/references/`

Merge into the `compound-docs` skill as a reference, since compound-docs is the skill that uses this template.

### 6.2 `global-learnings-template/` -> `global-learnings/` skill

Convert to a skill:
```
global-learnings/
├── SKILL.md              # Instructions for using the learnings system
├── scripts/              # Python CLI, graph engine
│   ├── learnings         # CLI entry point
│   ├── learnings_cli.py
│   ├── graph_engine.py
│   └── entity_store.py
└── references/
    └── setup.md          # Installation and config guide
```

Path swap: `~/.claude/global-learnings/` -> `~/.codex/global-learnings/`

---

## Phase 7: Document Gaps & Update AGENTS.md

### 7.1 Create `toolkit/codex/PORTING_GAPS.md`

Document what cannot port:
- **37 agent definitions** - no built-in sub-agent spawning in Codex
  - Workaround: Convert agent knowledge to skill `references/` files
  - Future: Agents SDK + MCP server wrapper
- **15 hooks** - Codex only supports `notify` on `agent-turn-complete`
  - No workaround available
- **Multi-agent workflows** - requires Agent Teams or Agents SDK
- **Swarm system** - tmux + JSONL messaging, Claude-specific

### 7.2 Update `toolkit/codex/AGENTS.md`

- Reference all skills at `~/.codex/skills/` (remove command references)
- Remove Claude-specific tool references (Task tool subagent_types)
- Add Codex-specific patterns (sandbox awareness, MCP tools)
- Keep shared patterns (session management, background processes, templates)

---

## Implementation Steps (Ordered)

1. Create `toolkit/codex/skills/` directory structure
2. Create `toolkit/codex/scripts/deploy.sh` with sync + path transform logic
3. Convert 18 simple commands to skill directories (SKILL.md with frontmatter)
4. Convert 7 path-dependent commands to skills with adaptation
5. Merge 3 crypto commands into `crypto-research/` skill as modes
6. Port 6 direct-copy skills from `toolkit/packages/skills/`
7. Port 5 path-adapted skills from `toolkit/packages/skills/`
8. Convert workflow commands to `workflow/` skill with references
9. Embed templates as `assets/` in their respective skills
10. Convert `global-learnings-template` to a skill
11. Merge `docs-solutions-template` into `compound-docs` skill
12. Create `toolkit/codex/PORTING_GAPS.md`
13. Update `toolkit/codex/AGENTS.md` to reference skills-only structure
14. Run deploy script and verify in Codex CLI
15. Remove stale `~/.codex/commands/` directory (deprecated)

---

## Verification

1. Run `toolkit/codex/scripts/deploy.sh` - should sync all skills to `~/.codex/skills/`
2. Open Codex CLI and verify:
   - Skills are listed (check `/skills` or equivalent)
   - `/brainstorm`, `/commit`, `/research` invoke as skills
   - `AGENTS.md` loads as system instructions
3. Verify skill count: `ls ~/.codex/skills/ | wc -l` should be ~36
4. Test path-adapted skills work (e.g., `/research` uses `~/.codex/` paths)
5. Verify `~/.codex/commands/` is empty or removed
6. Run `diff -rq toolkit/codex/skills/ ~/.codex/skills/` to verify sync
7. Test one rich skill (e.g., `crypto-research`) with scripts + references

---

## Scope Summary

| Category | Source Count | Skills Created | Notes |
|----------|------------|----------------|-------|
| Commands -> Skills | 28 | 25 | 3 crypto merged into existing skill |
| Existing Skills | 14 | 11 | 3 deferred (swarm/debug-bridge) |
| Workflow -> Skill | 2 | 1 | Multi-agent deferred |
| Knowledge -> Skills | 2 | 2 | Embedded in skills |
| Templates -> Assets | 2 | 0 | Embedded in skill assets/ |
| **Total new skills** | | **~36** | |
| Agents (gap) | 37 | 0 | Document; knowledge -> references/ |
| Hooks (gap) | 15 | 0 | Document; no workaround |

**Estimated effort:** ~3-4 hours for full skills conversion + deploy script

---

## References

- [OpenAI Codex Custom Prompts (Deprecated)](https://developers.openai.com/codex/custom-prompts/)
- [OpenAI Codex Skills](https://developers.openai.com/codex/skills/)
- [Claude Code Skills Docs](https://code.claude.com/docs/en/skills)
- [Anthropic Introducing Agent Skills](https://www.anthropic.com/news/skills)
- [Agent Skills Open Standard (Dec 2025)](https://github.com/anthropics/skills)
- [Research: Claude Code Agent Teams](research/2026-02-17_10-30-00_claude-code-agent-teams.md)
