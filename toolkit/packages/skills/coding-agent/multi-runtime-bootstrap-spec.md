# Specification: Multi-Runtime Bootstrap Extension

**Generated from:** User interview (research-backed)
**Interview date:** 2026-04-10
**Version:** 1.0

## Executive Summary

Extend `toolkit/bootstrap.js` to deploy toolkit skills to hermes-agent and nanoclaw runtimes, in addition to the existing claude-code-4.5, codex, copilot, gemini, etc. Fix hardcoded `~/.claude/` references in existing skills so they work across runtimes. Establish a `TOOLKIT_RUNTIME` env var convention for scripts that need runtime detection.

## Objectives

### Primary Goals
- Add `hermes-agent` as a bootstrap target (writes skills-only to `~/.hermes/skills/`)
- Add `nanoclaw` as a bootstrap target (alias/shared with claude-code-4.5, writes to `~/.claude/`)
- Audit and fix hardcoded `~/.claude/` paths in skills (use `{{HOME_TOOL_DIR}}` substitution)
- Introduce `TOOLKIT_RUNTIME` env var so scripts can detect which runtime they are running under
- Defer OpenClaw support until later (explicit user decision)

### Success Metrics
- `node bootstrap.js --tool=hermes-agent` deploys all toolkit skills to `~/.hermes/skills/`
- `node bootstrap.js --tool=nanoclaw` writes to `~/.claude/` (same target as claude-code-4.5)
- Zero skills contain hardcoded `~/.claude/` literal references after audit (exception: claude-code-4.5 template substitutions, which resolve to `.claude`)
- All existing runtimes (claude-code-4.5, codex, copilot, gemini) still work unchanged

## Scope

### In Scope
- New `hermes-agent` entry in `TOOL_CONFIG` with skills-only mapping
- New `nanoclaw` entry in `TOOL_CONFIG` (shares `~/.claude/` with claude-code-4.5)
- Audit of `toolkit/packages/skills/**` for hardcoded `~/.claude/` paths
- Replace hardcoded paths with `{{HOME_TOOL_DIR}}` template placeholders
- Add `TOOLKIT_RUNTIME` env var export in bootstrap (written to a wrapper script or activated in shell profile hint)
- Update `webapp-testing/SKILL.md` (20+ hardcoded refs) and `tmux-monitor/scripts/monitor.sh` (line 95)
- Documentation updates: add a portability section to any skill that still needs runtime-specific behavior

### Out of Scope
- OpenClaw bootstrap support (deferred per user decision)
- Hermes `config.yaml` generation (hermes users manage their own config)
- Migration tooling from openclaw to hermes (hermes-agent has its own `hermes claw migrate`)
- Skill-level unit tests or CI for runtime portability (deferred)
- Restructuring skills into a new "universal" vs "runtime-specific" hierarchy
- Rewriting skills that use runtime-specific tools (e.g., Task, AskUserQuestion) — they stay Claude-specific for now

### Future Considerations
- OpenClaw bootstrap (minimal TOOL_CONFIG, after hermes+nanoclaw proven)
- Per-skill `applies-to:` frontmatter filter so bootstrap can skip unsupported skills
- Full template substitution audit (ensure ALL file types are covered, not just `.md/.sh/.py`)
- CI test that runs `node bootstrap.js --tool=X` for each runtime and validates output

## Technical Requirements

### New Tool Configs

**hermes-agent** (skills-only):

```javascript
'hermes-agent': {
    ruleDir: 'packages',
    targetSubdir: '.hermes',
    forceHomeInstall: true,
    copyClaudeMd: false,
    copySettings: false,
    usePackagesStructure: true,
    externalDepTypes: ['npx-skills', 'agent-skills'],
    packageMappings: {
        'skills': 'skills',   // ONLY skills — no agents, utils, hooks, output-styles, reflections
    },
    templateSubstitutions: {
        '**/*.md':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.sh':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.py':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.js':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.ts':   { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.yaml': { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.yml':  { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.toml': { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
        '**/*.json': { 'TOOL_DIR': '.hermes', 'HOME_TOOL_DIR': '~/.hermes', 'TOOLKIT_RUNTIME': 'hermes-agent' },
    }
}
```

**nanoclaw** (shared with claude-code-4.5):

```javascript
'nanoclaw': {
    ruleDir: 'packages',
    targetSubdir: '.claude',   // SHARED with claude-code-4.5
    forceHomeInstall: true,
    usePackagesStructure: true,
    externalDepTypes: ['npx-skills', 'agent-skills'],  // no claude-plugins (nanoclaw syncs from container)
    packageMappings: {
        'skills': 'skills',
        'agents': 'agents',
        'utilities/utils': 'utils',
        'utilities/hooks': 'hooks',
        'utilities/output-styles': 'output-styles',
        'utilities/reflections': 'reflections'
    },
    templateSubstitutions: {
        '**/*.md':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude', 'TOOLKIT_RUNTIME': 'nanoclaw' },
        '**/*.sh':   { 'TOOL_DIR': '.claude', 'HOME_TOOL_DIR': '~/.claude', 'TOOLKIT_RUNTIME': 'nanoclaw' },
        // ... same file types as above
    }
}
```

### TOOLKIT_RUNTIME Env Var Convention

**Bootstrap writes** a small wrapper script at `${HOME_TOOL_DIR}/bin/agent-env.sh`:

```bash
#!/usr/bin/env bash
# Auto-generated by toolkit/bootstrap.js
# Source this to set runtime context for skill scripts
export TOOLKIT_RUNTIME="hermes-agent"  # or claude-code-4.5, nanoclaw, etc.
export TOOLKIT_HOME="$HOME/.hermes"    # or $HOME/.claude, etc.
export TOOLKIT_DIR=".hermes"            # or .claude, etc.
```

**Skills read it** via:

```bash
# In any skill script that needs runtime context:
TOOLKIT_RUNTIME="${TOOLKIT_RUNTIME:-claude-code-4.5}"
TOOLKIT_HOME="${TOOLKIT_HOME:-$HOME/.claude}"

# Runtime-specific logic:
case "$TOOLKIT_RUNTIME" in
    hermes-agent)
        SESSION_DIR="$TOOLKIT_HOME/sessions"  # hermes uses sessions/
        ;;
    nanoclaw|claude-code-4.5)
        SESSION_DIR="$TOOLKIT_HOME/session"   # claude uses session/
        ;;
esac
```

### Hardcoded Path Audit

**Must fix:**

| File | Issue | Fix |
|------|-------|-----|
| `skills/webapp-testing/SKILL.md` | 20+ refs to `~/.claude/skills/webapp-testing/bin/browser-tools` | Replace with `{{HOME_TOOL_DIR}}/skills/webapp-testing/bin/browser-tools` |
| `skills/tmux-monitor/scripts/monitor.sh` L95 | `$HOME/.claude/agents/${SESSION_NAME}.json` | Replace with `${TOOLKIT_HOME:-$HOME/.claude}/agents/${SESSION_NAME}.json` |
| `skills/reflect/SKILL.md` | References `~/.claude/session/` as fallback | Use `{{HOME_TOOL_DIR}}/session/` |
| Other `.sh`, `.py`, `.md` in `skills/` | Any `~/.claude/` literal not under template substitution | Replace with `{{HOME_TOOL_DIR}}` |

**Grep strategy:**

```bash
# Find all hardcoded references
grep -rn '~/\.claude\|$HOME/\.claude\|/Users/[^/]*/\.claude' \
  toolkit/packages/skills/ \
  --include='*.md' --include='*.sh' --include='*.py' --include='*.js' --include='*.ts'
```

### Bootstrap Flow Changes

1. **No changes** to existing `handlePackagesStructureCopy()` — the new tool configs slot in via `TOOL_CONFIG`
2. **Add** `TOOLKIT_RUNTIME` to `templateSubstitutions` for all runtimes (not just new ones) — so existing claude-code-4.5 installs get the env var too
3. **Optional:** Write `${HOME_TOOL_DIR}/bin/agent-env.sh` after package copy (new step in the existing flow)

## User Experience

### User Flows

1. **First-time hermes bootstrap**
   - User: `node bootstrap.js --tool=hermes-agent`
   - Skill: copies `toolkit/packages/skills/` to `~/.hermes/skills/` with template substitutions
   - Output: "✓ Skills copied to ~/.hermes/skills/ (N files)"
   - User verifies: `ls ~/.hermes/skills/reflect/SKILL.md`

2. **First-time nanoclaw bootstrap**
   - User: `node bootstrap.js --tool=nanoclaw`
   - Skill: copies to `~/.claude/` (same as claude-code-4.5)
   - If claude-code-4.5 already installed, files are overwritten cleanly

3. **Multi-runtime user**
   - User installs claude-code-4.5 first, then hermes-agent
   - Both runtimes have independent skill directories
   - Skills with runtime-specific logic use `TOOLKIT_RUNTIME` to differentiate

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| User runs `--tool=nanoclaw` after `--tool=claude-code-4.5` | Files overwritten in `~/.claude/`. No error. |
| Hermes `~/.hermes/` doesn't exist | Bootstrap creates it (via `mkdirSync({recursive: true})`) |
| Skill has a hardcoded `~/.claude/` that wasn't caught in audit | Leaks into hermes install as a literal path. Runtime detection won't save it. Must be caught in review. |
| User has both `TOOLKIT_RUNTIME=hermes-agent` and is running claude | Scripts will misroute. Document that env var should only be set per-terminal session. |

## Constraints & Dependencies

### Technical Constraints
- No breaking changes to existing tool configs (claude-code-4.5, codex, copilot, gemini, etc.)
- Must preserve backward compatibility — existing `~/.claude/` installs should continue working
- No new npm dependencies in bootstrap.js
- Template substitution must handle ALL file types that skills ship (`.md`, `.sh`, `.py`, `.js`, `.ts`, `.yaml`, `.yml`, `.toml`, `.json`)

### External Dependencies
- Node.js (existing bootstrap dependency)
- `tmux` only matters for skills that use it (coding-agent, tmux-monitor)
- Hermes-agent repo at `~/d/git/hermes-agent` — reference for format/conventions, no build-time dep

### Timeline Constraints
- None. This is toolkit maintenance, no external pressure

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Audit misses a hardcoded `~/.claude/` path | Skills break silently on hermes | Medium | Grep strategy above + test install + file-by-file review |
| `TOOLKIT_RUNTIME` env var not set when skill runs | Skills fall back to `~/.claude/` default | Low | Default fallback is safe; document the env var in skill docs |
| nanoclaw shared target causes conflict with claude-code-4.5 | Last-install wins; files overwritten | Low | User intentional (per interview decision); add warning in docs |
| Hermes-agent skill format incompatible | Skills don't load in hermes | Low | Research confirmed hermes uses same YAML frontmatter as Claude Code; agentskills.io standard |
| Bootstrap error in one tool breaks all | Existing users can't install updates | Medium | Each tool config is independent; test each new tool in isolation before merging |

## Decisions Made

### Key Trade-offs

**Decision 1: nanoclaw shares `~/.claude/` with claude-code-4.5**
- Alternatives: separate `~/.nanoclaw/`; skip nanoclaw entirely
- Rationale: Nanoclaw IS a claude-code fork that reads from `~/.claude/`. Shared target avoids duplication. Nanoclaw's native-runner already auto-syncs its container skills to `~/.claude/skills/`, so toolkit bootstrap is additive.

**Decision 2: Hermes-agent gets skills-only (no agents/hooks/utils/etc.)**
- Alternatives: same full set as claude-code-4.5; skills+hooks+utils
- Rationale: User explicitly chose minimal footprint. Hermes manages its own memory, sessions, config — toolkit only owns skills.

**Decision 3: Defer OpenClaw**
- Alternatives: add minimal support now; full parity
- Rationale: User prefers to ship hermes+nanoclaw first, address OpenClaw separately. OpenClaw repo not locally available for testing anyway.

**Decision 4: Fix hardcoded paths NOW, not later**
- Alternatives: fix on demand; separate follow-up
- Rationale: User explicitly chose "Fix now" to avoid silent bugs. Webapp-testing and tmux-monitor are the biggest offenders and both are widely used.

**Decision 5: Template substitution + runtime detection for scripts**
- Alternatives: env vars only; runtime detection only; drop runtime-specific refs entirely
- Rationale: User wants BOTH — template substitution at install (existing mechanism) PLUS runtime detection via `TOOLKIT_RUNTIME` env var for scripts that need dynamic behavior. Belt and braces.

**Decision 6: Env var `TOOLKIT_RUNTIME` as the runtime marker**
- Alternatives: marker files; parent process name
- Rationale: Explicit, no guessing, user can override for testing. Set by bootstrap in `bin/agent-env.sh` wrapper script.

### Deferred Decisions
- OpenClaw bootstrap (explicit deferral)
- `applies-to:` frontmatter filter (future enhancement)
- Runtime-specific skill variants (e.g., `reflect-claude.md` vs `reflect-hermes.md`) — not needed yet

## Implementation Notes

### Priority Order

1. **Add `TOOLKIT_RUNTIME` to existing template substitutions** for all tools (single source of truth)
2. **Grep audit** — find all hardcoded `~/.claude/` references in `toolkit/packages/skills/`
3. **Fix webapp-testing** — replace `~/.claude/` with `{{HOME_TOOL_DIR}}` in SKILL.md
4. **Fix tmux-monitor** — replace with `${TOOLKIT_HOME:-$HOME/.claude}` in monitor.sh
5. **Fix reflect and any others** found in audit
6. **Add `hermes-agent` to TOOL_CONFIG**
7. **Add `nanoclaw` to TOOL_CONFIG** (alias-like, shared target)
8. **Add `agent-env.sh` generation** in bootstrap (writes to `${HOME_TOOL_DIR}/bin/agent-env.sh`)
9. **Test** — `node bootstrap.js --tool=hermes-agent` end-to-end
10. **Test** — `node bootstrap.js --tool=nanoclaw` end-to-end
11. **Verify** no regressions in claude-code-4.5 install
12. **Commit** — ideally 2-3 commits: (a) audit/fix hardcoded paths, (b) add hermes-agent+nanoclaw configs, (c) add agent-env.sh generation

### Technical Debt Accepted
- Audit may miss edge cases in less common file types (`.yaml`, `.toml`, `.json`) — covered by template substitutions but not by grep
- `TOOLKIT_RUNTIME` env var requires users to source `agent-env.sh` — not automatic unless they add it to shell profile
- Some skills may still have Claude-specific tool references (Task, AskUserQuestion) — out of scope, noted for future

## Open Questions

- [ ] Should `agent-env.sh` be auto-sourced via shell profile hint, or left to user? (Assumption: optional, documented only)
- [ ] Should we add a `--dry-run` flag to bootstrap to preview changes? (Assumption: no, out of scope)
- [ ] Do we need a rollback mechanism if bootstrap fails halfway? (Assumption: no, existing behavior is "fail and leave partial state" — acceptable for this task)

---

*This specification was generated through systematic interview. Ready for implementation.*
