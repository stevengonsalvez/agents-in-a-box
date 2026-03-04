# Specification: GitHub Copilot CLI Integration

**Generated from:** Interview session + research of GitHub Copilot CLI docs
**Interview date:** 2026-03-04
**Version:** 1.0

## Executive Summary

Add GitHub Copilot CLI (`copilot` command) as a first-class session type in ainb-tui alongside Claude, Codex, and Gemini. Sessions run in tmux (not Docker) with standard feature parity: model selection, Interactive + Boss mode, log streaming. Integrate with the existing toolkit/packages ecosystem by generating custom instructions and adding auth/config utilities.

## Objectives

### Primary Goals
- Add `Copilot` as a fully functional `SessionAgentType` in ainb-tui
- Enable launching, attaching, detaching, and managing Copilot CLI sessions from the TUI
- Provide auth detection and guided setup for GitHub Copilot
- Reuse existing toolkit skills by injecting relevant ones as Copilot custom instructions
- Add Copilot-specific utilities to toolkit/packages

### Success Metrics
- User can select "GitHub Copilot" from the agent picker and launch a session
- Boss mode works with `copilot -p "prompt"` for unattended execution
- Existing skills (/commit, /plan) can be injected into Copilot sessions as custom instructions
- Auth setup flow guides users through `gh auth login` if needed

## Scope

### In Scope
- `SessionAgentType::Copilot` with icon, name, availability check
- `CliProvider::Copilot` mapping command, env var, permission flags
- `AgentProvider` entry for Copilot with model selection (Claude Sonnet 4.5, etc.)
- Tmux-based session launch (no Docker)
- Interactive mode (`copilot`) and Boss mode (`copilot -p "prompt"`)
- Auth detection (`gh auth status`) and guided setup
- Toolkit utility: `copilot-setup.sh` for install + auth
- Toolkit skill: `/copilot-setup` for guided onboarding
- Custom instructions generation from toolkit skills
- Model selection UI (Copilot supports switching models via `/model`)

### Out of Scope (Future)
- Docker container support for Copilot sessions
- MCP server configuration from TUI
- Copilot memory/learning management
- Copilot plan mode integration (Shift+Tab)
- Custom Copilot agents created from toolkit agent definitions
- Copilot experimental/autopilot mode

### Future Considerations
- Bridge Copilot's MCP support with toolkit MCP configs
- Cross-session communication between Copilot and Claude sessions
- Copilot-specific log parsing and progress indicators

## Technical Requirements

### Architecture

**Session Runtime**: Tmux (same as Shell sessions), NOT Docker containers
- Copilot CLI runs directly on host
- Requires `copilot` binary installed locally
- Uses `gh` OAuth for authentication (no separate API key)

**Session Launch Flow**:
1. Check `copilot` binary exists on PATH
2. Check `gh auth status` is valid with Copilot scope
3. Create tmux session with workspace as CWD
4. For Interactive mode: `copilot` (bare command, interactive session)
5. For Boss mode: `copilot -p "prompt" --allow-all-tools` (or with safety limits)

### Components

| Component | Purpose | Location |
|-----------|---------|----------|
| `SessionAgentType::Copilot` | Agent type enum variant | `models/session.rs` |
| `CliProvider::Copilot` | CLI command mapping | `config/mod.rs` |
| `AgentProvider` (Copilot) | Provider + model definitions | `app/state.rs` |
| `copilot-setup.sh` | Install + auth utility | `toolkit/packages/utilities/utils/` |
| `/copilot-setup` skill | Guided onboarding skill | `toolkit/packages/skills/copilot-setup/` |
| Custom instructions generator | Bridge toolkit → Copilot | `toolkit/packages/utilities/utils/` |

### Key Code Changes

#### 1. `models/session.rs` — SessionAgentType

```rust
pub enum SessionAgentType {
    #[default]
    Claude,
    Shell,
    Ssh,
    Codex,
    Gemini,
    Copilot,    // NEW
    Kiro,       // Disabled
}
```

`Copilot` variant:
- `icon()` → `"🐙"` (GitHub octocatish) or `"✈️"` (copilot)
- `name()` → `"GitHub Copilot"`
- `description()` → `"GitHub Copilot CLI — AI coding agent"`
- `is_available()` → `true` (check `which copilot` at runtime)

#### 2. `config/mod.rs` — CliProvider

```rust
pub enum CliProvider {
    #[default]
    Claude,
    Codex,
    Gemini,
    Copilot,    // NEW
}
```

`Copilot` variant:
- `command()` → `"copilot"`
- `api_key_env_var()` → `"GITHUB_TOKEN"` (or none — uses `gh` OAuth)
- `display_name()` → `"GitHub Copilot"`
- `skip_permissions_flag()` → `"--allow-all-tools"`

#### 3. `app/state.rs` — AgentProvider

Add Copilot to `AgentProvider::all()`:
```rust
AgentProvider {
    name: "GitHub Copilot".to_string(),
    vendor: "GitHub".to_string(),
    models: vec![
        AgentModel { name: "Claude Sonnet 4.5".into(), id: "claude-sonnet-4-5".into(), recommended: true },
        AgentModel { name: "GPT-4o".into(), id: "gpt-4o".into(), recommended: false },
        // Additional models as Copilot expands
    ],
    status: ProviderStatus::Available,
}
```

#### 4. Session Launch — Tmux Integration

Copilot sessions use the same tmux pattern as Shell sessions:
- Create tmux session named `ainb-copilot-{session-id-short}`
- Set working directory to workspace path
- For Interactive: `tmux send-keys "copilot" C-m`
- For Boss: `tmux send-keys "copilot -p '{prompt}'" C-m`
- Attach/detach via standard tmux operations

#### 5. Auth Detection

Before launching:
```bash
# Check copilot CLI exists
which copilot || echo "NOT_INSTALLED"

# Check gh auth status
gh auth status 2>&1 | grep -q "Logged in" && echo "AUTH_OK" || echo "AUTH_MISSING"

# Check copilot scope
gh auth status --show-token 2>&1 | grep -q "copilot" && echo "COPILOT_SCOPE_OK"
```

### Integrations

#### Toolkit Skills → Copilot Custom Instructions

Generate `.github/copilot-instructions.md` from selected toolkit skills:
- `/commit` skill → Copilot commit conventions
- `/plan` skill → Copilot planning instructions
- Project-level CLAUDE.md → Copilot project instructions

Utility script: `generate-copilot-instructions.sh`
- Reads selected skill SKILL.md files
- Extracts key instructions (strips frontmatter, formats for Copilot)
- Writes to `.github/copilot-instructions.md` in the workspace

#### Toolkit Auth Utilities

New utility: `copilot-setup.sh`
- Check for `copilot` binary, install if missing (Homebrew/curl)
- Check `gh` auth status, guide through `gh auth login` if needed
- Verify Copilot subscription status
- Test with `copilot --version`

### Performance Requirements
- Session launch: < 3 seconds (tmux is fast)
- Auth check: < 2 seconds
- No performance impact on existing session types

### Security Requirements
- Never store GitHub tokens directly — rely on `gh` OAuth
- Boss mode defaults to requiring tool approval (no `--allow-all-tools` unless user opts in)
- Custom instructions don't leak sensitive paths or credentials

## User Experience

### User Flows

1. **New Copilot Session (Happy Path)**:
   - Press `n` for new session
   - Select repository source (Local/Remote/Favorites)
   - Select GitHub Copilot from agent picker
   - Select model (defaults to Claude Sonnet 4.5)
   - Choose Interactive or Boss mode
   - Session launches in tmux

2. **First-Time Setup (No Auth)**:
   - User selects Copilot but `gh auth` is missing
   - TUI shows: "GitHub auth required. Run guided setup?"
   - Opens `copilot-setup.sh` in a tmux pane
   - Returns to session creation after auth completes

3. **Boss Mode Execution**:
   - User selects Boss mode
   - Enters prompt (e.g., "Fix all lint errors in src/")
   - TUI launches: `copilot -p "Fix all lint errors in src/"`
   - Log streaming shows Copilot output in real-time

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| `copilot` not installed | Show install instructions, offer to run `copilot-setup.sh` |
| `gh auth` expired | Detect and prompt re-authentication |
| No Copilot subscription | Show clear error message with subscription link |
| Copilot session hangs | Standard tmux kill works (same as Shell sessions) |
| Multiple Copilot sessions | Each gets unique tmux session name, no conflicts |
| Workspace has `.github/copilot-instructions.md` | Copilot reads it automatically — no TUI action needed |

## Constraints & Dependencies

### Technical Constraints
- Copilot CLI must be installed on the host (no Docker fallback in v1)
- Requires `gh` CLI for authentication
- Model selection happens inside Copilot (`/model` command), not pre-launch
- Copilot's trusted directory prompt is per-directory (one-time)

### External Dependencies
- GitHub Copilot subscription (Individual, Business, or Enterprise)
- `gh` CLI (for OAuth)
- `copilot` binary (installable via Homebrew, curl, or npm)

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Copilot CLI changes API/flags | High | Medium | Pin to known version, version detection |
| Auth flow differs across GitHub plans | Medium | Low | Test with Individual and Business plans |
| Model list changes frequently | Low | High | Fetch available models dynamically or update periodically |
| Copilot doesn't support `--allow-all-tools` in all contexts | Medium | Low | Graceful fallback to interactive approval |

## Decisions Made

### Key Trade-offs
- **Tmux over Docker**: Simpler, faster, no container overhead. Docker can be added later.
- **Auth via `gh` only**: No separate API key management. Simpler but couples to `gh` CLI.
- **Standard parity first**: Same UX as Claude/Codex/Gemini. Advanced features (MCP, memory) later.
- **Custom instructions opt-in**: Don't automatically generate — let user choose per-session.

### Deferred Decisions
- MCP server configuration: Explore after basic sessions work
- Copilot memory/learning: Understand usage patterns first
- Cross-session communication: Complex, defer to future phase

## Implementation Notes

### Priority Order
1. **P0**: Add `SessionAgentType::Copilot` + `CliProvider::Copilot` + `AgentProvider` entry
2. **P0**: Tmux session launch (Interactive mode)
3. **P1**: Auth detection and error handling
4. **P1**: Boss mode support (`copilot -p`)
5. **P2**: `copilot-setup.sh` utility + `/copilot-setup` skill
6. **P2**: Custom instructions generator from toolkit skills
7. **P3**: Model selection refinement (dynamic model list)

### Files to Modify

| File | Change |
|------|--------|
| `ainb-tui/src/models/session.rs` | Add `Copilot` to `SessionAgentType` |
| `ainb-tui/src/config/mod.rs` | Add `Copilot` to `CliProvider` |
| `ainb-tui/src/app/state.rs` | Add Copilot `AgentProvider` + models |
| `ainb-tui/src/docker/session_lifecycle.rs` | Handle Copilot tmux launch |
| `ainb-tui/src/components/new_session.rs` | UI renders automatically via AgentProvider |
| `toolkit/packages/utilities/utils/copilot-setup.sh` | **NEW** — Install + auth |
| `toolkit/packages/skills/copilot-setup/SKILL.md` | **NEW** — Onboarding skill |
| `toolkit/packages/utilities/utils/generate-copilot-instructions.sh` | **NEW** — Bridge toolkit → Copilot |

### Technical Debt Accepted
- Model list is hardcoded initially (not fetched dynamically from Copilot)
- No Docker support in v1 — tmux only
- Custom instructions generator is a bash script, not integrated into TUI

## Open Questions

- [ ] What icon for Copilot sessions? `🐙` (GitHub) vs `✈️` (copilot) vs `🤖` (generic AI)?
- [ ] Should `copilot` binary detection happen at TUI startup or lazily per-session?
- [ ] Does Copilot CLI support specifying the model at launch time (flag), or only via `/model` interactively?

---

*This specification was generated through systematic interview of the project author.*
